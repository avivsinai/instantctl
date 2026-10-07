use std::{future::pending, sync::mpsc, time::Duration};

use crate::{
    Error,
    mutation::{Mutation, Plan, Report, apply_once},
};
use tokio::sync::{Notify, oneshot};

pub(super) struct ClockGuard {
    _sender: mpsc::Sender<()>,
}

/// Copy Tokio's documented blocking-task guard against auto-advance during I/O:
/// https://docs.rs/tokio/1.53.2/tokio/time/fn.pause.html#preventing-auto-advance
pub(super) async fn keep_clock_paused() -> ClockGuard {
    let (sender, receiver) = mpsc::channel();
    let (started, ready) = oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        let _ = receiver.recv();
    });
    ready.await.expect("clock guard blocking task started");
    ClockGuard { _sender: sender }
}

struct ReadCompleted<'a, M> {
    inner: &'a M,
    completed: Notify,
    elapsed_before_read: Duration,
}

impl<M: Mutation> Mutation for ReadCompleted<'_, M> {
    type State = M::State;
    async fn read(&self) -> Result<Self::State, Error> {
        if !self.elapsed_before_read.is_zero() {
            tokio::time::advance(self.elapsed_before_read).await;
        }
        let result = self.inner.read().await;
        self.completed.notify_one();
        result
    }
    async fn write(&self, desired: &Self::State) -> Result<(), Error> {
        self.inner.write(desired).await
    }
}

/// Structural one-readback cases expire only after the real socket result has
/// reached apply_once. Keep the production write, read, and report paths intact.
pub(super) async fn apply_readback_once<M: Mutation>(
    mutation: &M,
    plan: &Plan<M::State>,
    timeout: Duration,
) -> Result<Report<M::State>, Error> {
    apply_readback_after(mutation, plan, timeout, Duration::ZERO).await
}

/// Reboot uptime evidence uses elapsed monotonic time before the first read.
pub(super) async fn apply_readback_after<M: Mutation>(
    mutation: &M,
    plan: &Plan<M::State>,
    timeout: Duration,
    elapsed: Duration,
) -> Result<Report<M::State>, Error> {
    let wrapped = ReadCompleted {
        inner: mutation,
        completed: Notify::new(),
        elapsed_before_read: elapsed,
    };
    let expire = async {
        wrapped.completed.notified().await;
        tokio::time::advance(timeout).await;
        pending::<()>().await;
    };
    tokio::select! {
        biased;
        result = apply_once(&wrapped, plan, timeout) => result,
        () = expire => unreachable!("clock driver never returns"),
    }
}
