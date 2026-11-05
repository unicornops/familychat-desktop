//! The background runtime every Matrix session runs on.
//!
//! GPUI has its own executors and no Tokio reactor, while matrix-sdk (and
//! reqwest, hyper and its stores under it) needs Tokio. Both frontends
//! therefore start one [`BackgroundRuntime`]: a multi-thread Tokio runtime
//! on a dedicated thread. Every SDK future is spawned on its handle, and
//! results come back through runtime-agnostic channels that GPUI tasks (or
//! the terminal UI's loop) await (PLAN.md, section 5.2; ADR 0005 when the
//! session lands in Phase 3).
//!
//! [`BackgroundRuntime`] is copied from Zmail's `zmail-sync`.

use std::{sync::mpsc, thread, time::Duration};

use anyhow::{Context, Result};
use familychat_core::RuntimeSummary;
use tokio::{
    runtime::{Builder, Handle},
    sync::oneshot,
};

/// Dedicated Tokio runtime host for background tasks.
#[derive(Debug)]
pub struct BackgroundRuntime {
    handle: Handle,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<thread::JoinHandle<()>>,
    summary: RuntimeSummary,
}

impl BackgroundRuntime {
    /// Starts a dedicated multithreaded Tokio runtime and verifies that task execution works.
    pub fn start(worker_threads: usize) -> Result<Self> {
        let (handle_tx, handle_rx) = mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let join_handle = thread::Builder::new()
            .name("familychat-tokio-runtime".to_owned())
            .spawn(move || {
                let runtime = match Builder::new_multi_thread()
                    .worker_threads(worker_threads)
                    .thread_name("familychat-tokio-worker")
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = handle_tx.send(Err(error.to_string()));
                        return;
                    }
                };

                if handle_tx.send(Ok(runtime.handle().clone())).is_err() {
                    return;
                }

                runtime.block_on(async move {
                    let _ = shutdown_rx.await;
                });
            })
            .context("failed to spawn the dedicated tokio runtime thread")?;

        let handle = handle_rx
            .recv()
            .context("tokio runtime thread exited before it reported readiness")?
            .map_err(anyhow::Error::msg)?;

        Self::probe(&handle)?;

        Ok(Self {
            handle,
            shutdown_tx: Some(shutdown_tx),
            join_handle: Some(join_handle),
            summary: RuntimeSummary {
                worker_threads,
                bridge_verified: true,
            },
        })
    }

    /// Returns a cloneable Tokio handle for async protocol services.
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// Returns the startup summary presented by the shell.
    pub fn summary(&self) -> RuntimeSummary {
        self.summary.clone()
    }

    fn probe(handle: &Handle) -> Result<()> {
        let (probe_tx, probe_rx) = mpsc::sync_channel(1);
        handle.spawn(async move {
            let _ = probe_tx.send(());
        });

        probe_rx
            .recv_timeout(Duration::from_secs(1))
            .context("tokio runtime probe did not complete within one second")?;

        Ok(())
    }
}

impl Drop for BackgroundRuntime {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }

        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::BackgroundRuntime;

    #[test]
    fn starts_probes_and_reports_its_summary() {
        let runtime = BackgroundRuntime::start(2).expect("runtime should start");
        let summary = runtime.summary();
        assert_eq!(summary.worker_threads, 2);
        assert!(summary.bridge_verified);
    }

    #[test]
    fn runs_tokio_futures_on_its_own_threads() {
        let runtime = BackgroundRuntime::start(1).expect("runtime should start");
        let (tx, rx) = std::sync::mpsc::channel();
        runtime.handle().spawn(async move {
            // Tokio's timer only works inside a Tokio runtime.
            tokio::time::sleep(Duration::from_millis(5)).await;
            let name = std::thread::current().name().map(str::to_owned);
            let _ = tx.send(name);
        });
        let name = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the task should finish");
        assert_eq!(name.as_deref(), Some("familychat-tokio-worker"));
    }

    #[test]
    fn dropping_it_shuts_the_runtime_down() {
        let runtime = BackgroundRuntime::start(1).expect("runtime should start");
        let handle = runtime.handle().clone();
        drop(runtime);
        // A handle to a runtime that has shut down still accepts a spawn,
        // but the task never runs and its join handle reports cancellation.
        let task = handle.spawn(async {});
        let joined = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a probe runtime")
                .block_on(task)
        })
        .join()
        .expect("the probe thread");
        assert!(joined.is_err_and(|error| error.is_cancelled()));
    }
}
