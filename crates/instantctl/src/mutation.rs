use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use clap::Args as ClapArgs;
use instantctl_api::{
    Error, ErrorKind,
    mutation::{Mutation, Plan, apply_once},
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    context::{CommandContext, CommandResult},
    credentials::CredentialSource,
    exit::ExitStatus,
};

#[derive(Clone, Copy, Debug, Default, ClapArgs)]
pub struct Options {
    /// Apply the planned change once, then verify it by readback.
    #[arg(long)]
    pub apply: bool,
    /// Confirm the change without an interactive prompt.
    #[arg(long)]
    pub yes: bool,
}

impl Options {
    /// Reject applies that cannot be confirmed before they need credentials or network access.
    pub fn preflight(&self, token_source: CredentialSource) -> Result<(), Error> {
        if !self.apply || self.yes {
            return Ok(());
        }
        if matches!(token_source, CredentialSource::Stdin) {
            return Err(Error::new(
                ErrorKind::ConfirmationRequired,
                "--token-stdin requires --yes when applying a change",
            ));
        }
        if !io::stdin().is_terminal() {
            return Err(confirmation_required());
        }
        Ok(())
    }
}

/// Print a mutation plan, confirm it when requested, then make at most one write.
pub async fn execute<M>(
    backend: &M,
    plan: Plan<M::State>,
    operation: &str,
    target: Value,
    options: Options,
    context: &CommandContext,
) -> anyhow::Result<CommandResult>
where
    M: Mutation,
    M::State: Serialize,
{
    let mut data = json!({
        "operation": operation,
        "target": target,
        "current": plan.current,
        "desired": plan.desired,
        "request_attempted": false,
    });
    let stderr = io::stderr();
    crate::output::write_data(&mut stderr.lock(), context.format, &data)?;
    if !options.apply {
        return Ok(CommandResult::success(data));
    }

    options.preflight(context.token_source)?;
    confirm(options.yes)?;
    let started = tokio::time::Instant::now();
    let apply = apply_once(backend, &plan, context.timeout);
    tokio::pin!(apply);
    let progress = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(progress);
    let report = loop {
        tokio::select! {
            biased;
            result = &mut apply => break result?,
            () = &mut progress => {
                let progress_data = json!({
                    "operation": operation,
                    "status": "waiting",
                    "elapsed_seconds": started.elapsed().as_secs(),
                });
                let stderr = io::stderr();
                if crate::output::write_data(&mut stderr.lock(), context.format, &progress_data).is_err() {
                    // Keep awaiting the already-started write and readback after a display failure.
                    break apply.await?;
                }
                progress.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(5));
            }
        }
    };
    let report_value = serde_json::to_value(&report)?;
    let report_object = report_value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("mutation report must be a JSON object"))?;
    let data_object = data
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("mutation plan must be a JSON object"))?;
    data_object.insert("request_attempted".into(), Value::Bool(true));
    data_object.extend(report_object.clone());

    let status = report
        .error_kind()
        .map_or(ExitStatus::Success, ExitStatus::Error);
    Ok(CommandResult { data, status })
}

pub(crate) fn confirm(apply_yes: bool) -> Result<(), Error> {
    if apply_yes {
        return Ok(());
    }

    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    stderr
        .write_all(b"Apply this change? [y/N] ")
        .and_then(|()| stderr.flush())
        .map_err(|_| Error::new(ErrorKind::General, "failed to write confirmation prompt"))?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(|_| {
        Error::new(
            ErrorKind::ConfirmationRequired,
            "confirmation input was not available",
        )
    })?;
    if answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        Err(confirmation_required())
    }
}

fn confirmation_required() -> Error {
    Error::new(
        ErrorKind::ConfirmationRequired,
        "confirmation required; use --yes to apply this change",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use clap::Parser;
    use instantctl_api::mutation::{Mutation, Plan};

    use super::*;

    struct Controlled {
        current: AtomicBool,
        writes: AtomicUsize,
        write_fails: bool,
        update_state: bool,
    }

    impl Controlled {
        fn new(current: bool, write_fails: bool, update_state: bool) -> Self {
            Self {
                current: AtomicBool::new(current),
                writes: AtomicUsize::new(0),
                write_fails,
                update_state,
            }
        }

        fn write_count(&self) -> usize {
            self.writes.load(Ordering::SeqCst)
        }
    }

    impl Mutation for Controlled {
        type State = bool;

        async fn read(&self) -> Result<bool, Error> {
            Ok(self.current.load(Ordering::SeqCst))
        }

        async fn write(&self, desired: &bool) -> Result<(), Error> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.update_state {
                self.current.store(*desired, Ordering::SeqCst);
            }
            if self.write_fails {
                Err(Error::new(
                    ErrorKind::General,
                    "safe simulated request failure",
                ))
            } else {
                Ok(())
            }
        }
    }

    fn context(timeout: Duration, token_source: CredentialSource) -> CommandContext {
        CommandContext {
            profile: "test-mutation-nonexistent-profile-3d91".to_owned(),
            site: None,
            timeout,
            format: crate::output::Format::Json,
            token_source,
            prepared_token: None,
            protected_ports: Vec::new(),
        }
    }

    fn plan(current: bool, desired: bool) -> Plan<bool> {
        Plan { current, desired }
    }

    fn options_from_cli(args: &[&str]) -> Options {
        let cli = crate::cli::Cli::try_parse_from(args.iter().copied()).unwrap();
        match cli.command {
            crate::cli::Command::Device(device) => match device.command {
                crate::commands::device::Command::Locate(args) => args.options,
                crate::commands::device::Command::Reboot(args) => args.options,
                _ => panic!("expected device locate command"),
            },
            _ => panic!("expected device command"),
        }
    }

    #[tokio::test]
    async fn execute_enforces_mutation_policy_matrix() {
        let target = json!({"device_id":"00:11:22:33:44:55"});
        let normal_context = context(Duration::from_secs(1), CredentialSource::Environment);
        let default_args = ["instantctl", "device", "locate", "00:11:22:33:44:55", "on"];
        let yes_args = [
            "instantctl",
            "device",
            "locate",
            "00:11:22:33:44:55",
            "on",
            "--yes",
        ];
        let apply_yes_args = [
            "instantctl",
            "device",
            "locate",
            "00:11:22:33:44:55",
            "on",
            "--apply",
            "--yes",
        ];
        let apply_args = [
            "instantctl",
            "device",
            "locate",
            "00:11:22:33:44:55",
            "on",
            "--apply",
        ];

        let reboot_default_args = ["instantctl", "device", "reboot", "00:11:22:33:44:55"];
        for args in [&default_args[..], &yes_args[..], &reboot_default_args[..]] {
            let backend = Controlled::new(false, false, true);
            let result = execute(
                &backend,
                plan(false, true),
                "device.locate",
                target.clone(),
                options_from_cli(args),
                &normal_context,
            )
            .await
            .unwrap();
            assert_eq!(result.status, ExitStatus::Success);
            assert_eq!(result.data["request_attempted"], false);
            assert_eq!(backend.write_count(), 0);
        }

        let backend = Controlled::new(false, false, true);
        let result = execute(
            &backend,
            plan(false, true),
            "device.locate",
            target.clone(),
            options_from_cli(&apply_yes_args),
            &normal_context,
        )
        .await
        .unwrap();
        assert_eq!(backend.write_count(), 1);
        assert_eq!(result.data["request_attempted"], true);
        assert_eq!(result.data["outcome"], "verified");

        let backend = Controlled::new(false, true, true);
        let result = execute(
            &backend,
            plan(false, true),
            "device.locate",
            target.clone(),
            options_from_cli(&apply_yes_args),
            &normal_context,
        )
        .await
        .unwrap();
        assert_eq!(backend.write_count(), 1);
        assert_eq!(result.status, ExitStatus::Error(ErrorKind::General));
        assert_eq!(result.data["outcome"], "request_failed_state_matches");
        assert_eq!(
            result.data["request_error"]["message"],
            "safe simulated request failure"
        );

        let backend = Controlled::new(false, false, false);
        let short_context = context(Duration::from_millis(100), CredentialSource::Environment);
        let result = execute(
            &backend,
            plan(false, true),
            "device.locate",
            target.clone(),
            options_from_cli(&apply_yes_args),
            &short_context,
        )
        .await
        .unwrap();
        assert_eq!(backend.write_count(), 1);
        assert_eq!(result.status, ExitStatus::Error(ErrorKind::Unverified));
        assert_eq!(result.data["outcome"], "unverified");

        let backend = Controlled::new(false, false, true);
        let stdin_context = context(Duration::from_secs(1), CredentialSource::Stdin);
        let error = match execute(
            &backend,
            plan(false, true),
            "device.locate",
            target,
            options_from_cli(&apply_args),
            &stdin_context,
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("token-stdin apply without --yes must be rejected"),
        };
        assert_eq!(
            error.downcast_ref::<Error>().unwrap().kind,
            ErrorKind::ConfirmationRequired
        );
        assert_eq!(backend.write_count(), 0);
    }
}
