//! Tiny `(TaskTracker, CancellationToken)` wrapper used by [`Client`] to own
//! its background tasks. Spawned tasks observe the group's cancellation token
//! and the tracker counts them so [`TaskGroup::shutdown`] can wait for every
//! task to exit.
//!
//! [`Client`]: crate::Client

use std::future::Future;

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Cheaply-cloneable handle to a tracked set of cancellable tasks.
#[derive(Clone, Default)]
pub struct TaskGroup {
    tracker: TaskTracker,
    token: CancellationToken,
}

impl TaskGroup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn `fut` onto the runtime. The task is dropped at its next await
    /// point after [`Self::cancel`] (or [`Self::shutdown`]) is called.
    pub fn spawn<R, Fut>(&self, fut: Fut)
    where
        Fut: Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
        let token = self.token.clone();
        self.tracker.spawn(async move {
            token.run_until_cancelled(fut).await;
        });
    }

    /// Signal cancellation to every spawned task. Returns immediately —
    /// tasks observe cancellation at their next await. Suitable for `Drop`.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Cancel and await every spawned task to completion.
    pub async fn shutdown(&self) {
        self.token.cancel();
        self.tracker.close();
        self.tracker.wait().await;
    }
}
