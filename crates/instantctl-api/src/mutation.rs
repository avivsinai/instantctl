use std::{future::Future, marker::PhantomData, time::Duration};

use serde::Serialize;
use serde_json::Value;
use tokio::time::{Instant, sleep_until, timeout_at};

use crate::{Error, ErrorKind};

/// The state this operation owns, and one write that requests that state.
pub trait Mutation: Sync {
    type State: Clone + PartialEq + Send + Sync;

    fn read(&self) -> impl Future<Output = Result<Self::State, Error>> + Send;
    fn write(&self, desired: &Self::State) -> impl Future<Output = Result<(), Error>> + Send;
}

#[derive(Debug, Serialize)]
pub struct Plan<S> {
    pub current: S,
    pub desired: S,
}

/// A resolved target and the plan bound to its mutation backend.
pub struct Prepared<M: Mutation> {
    pub backend: M,
    pub plan: Plan<M::State>,
    pub target: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Verified,
    Unverified,
    RequestFailedStateMatches,
    Failed,
}

#[derive(Debug, Serialize)]
pub struct Report<S> {
    pub outcome: Outcome,
    pub observed: Option<S>,
    pub readback_attempts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_error: Option<Error>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readback_error: Option<Error>,
}

impl<S> Report<S> {
    pub fn error_kind(&self) -> Option<ErrorKind> {
        match self.outcome {
            Outcome::Verified => None,
            Outcome::Unverified => Some(ErrorKind::Unverified),
            Outcome::RequestFailedStateMatches | Outcome::Failed => Some(
                self.request_error
                    .as_ref()
                    .map_or(ErrorKind::General, |error| error.kind),
            ),
        }
    }
}

/// Bound the single write and its readback polling by one monotonic deadline.
pub async fn apply_once<M: Mutation>(
    mutation: &M,
    plan: &Plan<M::State>,
    timeout: Duration,
) -> Result<Report<M::State>, Error> {
    if timeout.is_zero() {
        return Err(Error::new(
            ErrorKind::Config,
            "mutation timeout must be greater than zero",
        ));
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| Error::new(ErrorKind::Config, "mutation timeout is too large"))?;
    let request_error = match timeout_at(deadline, mutation.write(&plan.desired)).await {
        Ok(result) => result.err(),
        Err(_) => Some(Error::new(ErrorKind::General, "mutation request timed out")),
    };
    let mut report = Report {
        outcome: if request_error.is_some() {
            Outcome::Failed
        } else {
            Outcome::Unverified
        },
        observed: None,
        readback_attempts: 0,
        request_error,
        readback_error: None,
    };

    while Instant::now() < deadline {
        report.readback_attempts += 1;
        match timeout_at(deadline, mutation.read()).await {
            Ok(Ok(observed)) => {
                let matches = observed == plan.desired;
                report.observed = Some(observed);
                report.readback_error = None;
                if matches {
                    report.outcome = if report.request_error.is_some() {
                        Outcome::RequestFailedStateMatches
                    } else {
                        Outcome::Verified
                    };
                    return Ok(report);
                }
            }
            Ok(Err(error)) => {
                report.observed = None;
                report.readback_error = Some(error);
            }
            Err(_) => {
                report.observed = None;
                report.readback_error = Some(Error::new(
                    ErrorKind::General,
                    "mutation readback timed out",
                ));
                break;
            }
        }
        sleep_until((Instant::now() + Duration::from_millis(400)).min(deadline)).await;
    }
    Ok(report)
}

/// The source and destination of a full-object read-modify-write operation.
pub(crate) trait ObjectResource: Sync {
    fn read_object(&self) -> impl Future<Output = Result<Value, Error>> + Send;
    fn put_object(&self, body: &Value) -> impl Future<Output = Result<(), Error>> + Send;
}

/// Preserve the fetched object and verify only the state this operation owns.
pub(crate) struct FullObjectPut<R, S, F> {
    resource: R,
    desired_body: Value,
    observe: F,
    state: PhantomData<S>,
}

impl<R, S, F> FullObjectPut<R, S, F>
where
    R: ObjectResource,
    S: Clone + PartialEq + Send + Sync,
    F: Fn(&Value) -> Result<S, Error> + Sync,
{
    pub(crate) fn prepare(
        resource: R,
        current_body: Value,
        observe: F,
        patch: impl FnOnce(&mut Value) -> Result<(), Error>,
    ) -> Result<(Self, Plan<S>), Error> {
        if !current_body.is_object() {
            return Err(Error::new(
                ErrorKind::General,
                "portal resource is not a JSON object",
            ));
        }
        let current = observe(&current_body)?;
        let mut desired_body = current_body;
        patch(&mut desired_body)?;
        if !desired_body.is_object() {
            return Err(Error::new(
                ErrorKind::Config,
                "full-object update must preserve a JSON object",
            ));
        }
        let desired = observe(&desired_body)?;
        Ok((
            Self {
                resource,
                desired_body,
                observe,
                state: PhantomData,
            },
            Plan { current, desired },
        ))
    }
}

impl<R, S, F> Mutation for FullObjectPut<R, S, F>
where
    R: ObjectResource,
    S: Clone + PartialEq + Send + Sync,
    F: Fn(&Value) -> Result<S, Error> + Sync,
{
    type State = S;

    async fn read(&self) -> Result<S, Error> {
        (self.observe)(&self.resource.read_object().await?)
    }

    async fn write(&self, desired: &S) -> Result<(), Error> {
        if (self.observe)(&self.desired_body)? != *desired {
            return Err(Error::new(
                ErrorKind::Config,
                "desired state does not match the prepared update",
            ));
        }
        self.resource.put_object(&self.desired_body).await
    }
}
