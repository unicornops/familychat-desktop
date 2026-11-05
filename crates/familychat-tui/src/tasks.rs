//! Running `familychat-actions`' tasks and the session's slower answers
//! off the UI thread (ADR 0008, "Writing"; ADR 0005).
//!
//! The terminal UI has no async runtime of its own: it polls the session's
//! channels on each tick. An action is a future that needs no particular
//! runtime (it only awaits the session's `tokio::sync` answers), but some
//! of its work is plain blocking work (reading a file and taking a
//! picture's location out happen inside `send_file`). So each one runs to
//! its end on a short-lived thread of its own, parked between wake-ups,
//! and reports through the actions' event channel, or through the
//! oneshot [`spawn`] hands back. [`Tasks::running`] counts them, so the
//! snapshot tests wait until every one has finished.

use std::{
    future::Future,
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
};

use tokio::sync::oneshot;

/// Wakes the thread a task runs on.
struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs `future` to its end on this thread, parking it between wake-ups.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        // A spurious wake-up only polls once more.
        thread::park();
    }
}

/// The tasks in flight.
#[derive(Debug, Clone, Default)]
pub(crate) struct Tasks {
    running: Arc<AtomicUsize>,
}

/// Counts a task down when it ends, however it ends.
struct Running(Arc<AtomicUsize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Tasks {
    /// How many have not finished.
    pub(crate) fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    /// Runs `future` on a thread of its own and drops its output (an
    /// action reports through its event channel).
    pub(crate) fn run(&self, future: impl Future<Output = ()> + Send + 'static) {
        drop(self.spawn(future));
    }

    /// Runs `future` on a thread of its own; its output arrives on the
    /// receiver.
    pub(crate) fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> oneshot::Receiver<T> {
        let (answer, receiver) = oneshot::channel();
        self.running.fetch_add(1, Ordering::SeqCst);
        let running = Running(self.running.clone());
        let spawned = thread::Builder::new()
            .name("familychat-tui-action".to_owned())
            .spawn(move || {
                let _running = running;
                let _ = answer.send(block_on(future));
            });
        if let Err(error) = spawned {
            // The thread couldn't start: `running` was dropped with the
            // closure, and the receiver reports the task as gone.
            tracing::warn!("an action's thread couldn't start: {error}");
        }
        receiver
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_task_runs_to_its_end_and_is_counted_until_then() {
        let tasks = Tasks::default();
        let (release, wait) = oneshot::channel::<u8>();
        let mut answer = tasks.spawn(async move { wait.await.unwrap_or(0) + 1 });
        assert_eq!(tasks.running(), 1);
        release.send(41).expect("the task waits");
        let deadline = Instant::now() + Duration::from_secs(5);
        let value = loop {
            match answer.try_recv() {
                Ok(value) => break value,
                Err(_) => {
                    assert!(Instant::now() < deadline, "the task never answered");
                    thread::sleep(Duration::from_millis(2));
                }
            }
        };
        assert_eq!(value, 42);
        while tasks.running() > 0 {
            assert!(Instant::now() < deadline, "the task is still counted");
            thread::sleep(Duration::from_millis(2));
        }
    }
}
