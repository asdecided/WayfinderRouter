//! Loopback server lifecycle and bounded graceful shutdown.

use std::future::{Future, IntoFuture};
use std::time::Duration;

use axum::Router;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Maximum connection drain after shutdown begins.
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Gateway serving or shutdown failure.
#[derive(Debug, Error)]
pub enum ServerError {
    /// Axum's listener/server failed.
    #[error("gateway server failed: {0}")]
    Io(#[from] std::io::Error),
    /// Existing connections did not drain before the configured deadline.
    #[error("gateway connections did not drain within {0:?}")]
    DrainTimedOut(Duration),
    /// The process could not install an operating-system signal handler.
    #[error("cannot install gateway shutdown signal: {0}")]
    Signal(std::io::Error),
}

/// Serve an already-bound listener until shutdown, then drain with a deadline.
///
/// Binding is deliberately separate so callers can enforce loopback policy,
/// report the actual ephemeral test port, and surface permission/address errors
/// before starting the service future.
pub async fn serve_with_shutdown<F>(
    listener: TcpListener,
    application: Router,
    shutdown: F,
    drain_timeout: Duration,
) -> Result<(), ServerError>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (shutdown_started, shutdown_observed) = oneshot::channel();
    let shutdown_notifier = async move {
        shutdown.await;
        let _ = shutdown_started.send(());
    };
    let server = axum::serve(listener, application)
        .with_graceful_shutdown(shutdown_notifier)
        .into_future();
    drain_with_deadline(server, shutdown_observed, drain_timeout).await
}

/// Await a serving future and enforce a finite drain after shutdown starts.
///
/// This socket-independent seam makes cancellation and deadline behavior
/// deterministic under Tokio's paused clock and fake serving futures.
pub async fn drain_with_deadline<S>(
    server: S,
    shutdown_observed: oneshot::Receiver<()>,
    drain_timeout: Duration,
) -> Result<(), ServerError>
where
    S: Future<Output = Result<(), std::io::Error>> + Send,
{
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result.map_err(ServerError::Io),
        _ = shutdown_observed => {
            match tokio::time::timeout(drain_timeout, &mut server).await {
                Ok(result) => result.map_err(ServerError::Io),
                Err(_) => Err(ServerError::DrainTimedOut(drain_timeout)),
            }
        }
    }
}

/// Wait for SIGINT or SIGTERM on Unix, and Ctrl-C elsewhere.
pub async fn shutdown_signal() -> Result<(), ServerError> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(ServerError::Signal)?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(ServerError::Signal),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.map_err(ServerError::Signal)
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::routing::get;

    use super::*;

    #[tokio::test]
    async fn immediate_shutdown_completes_without_a_socket_leak() -> Result<(), ServerError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let application = Router::new().route("/healthz", get(|| async { "ok" }));
        serve_with_shutdown(listener, application, async {}, Duration::from_millis(100)).await
    }

    #[tokio::test]
    async fn finite_shutdown_future_type_is_accepted() -> Result<(), ServerError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let application = Router::new();
        let (sender, receiver) = oneshot::channel::<()>();
        let shutdown = async move {
            let _: Result<(), oneshot::error::RecvError> = receiver.await;
        };
        let task = tokio::spawn(serve_with_shutdown(
            listener,
            application,
            shutdown,
            Duration::from_secs(1),
        ));
        let _: Result<(), ()> = sender.send(());
        task.await.map_err(|error| {
            ServerError::Io(std::io::Error::other(format!(
                "server task failed: {error}"
            )))
        })?
    }

    struct DropObserved {
        dropped: Arc<AtomicBool>,
    }

    impl Future for DropObserved {
        type Output = Result<(), std::io::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            std::task::Poll::Pending
        }
    }

    impl Drop for DropObserved {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn drain_deadline_is_deterministic_and_cancels_server_future() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = oneshot::channel();
        let task = tokio::spawn(drain_with_deadline(
            DropObserved {
                dropped: Arc::clone(&dropped),
            },
            receiver,
            Duration::from_secs(10),
        ));
        let _: Result<(), ()> = sender.send(());
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        let result = task.await;
        assert!(
            matches!(result, Ok(Err(ServerError::DrainTimedOut(timeout))) if timeout == Duration::from_secs(10))
        );
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn completed_fake_server_wins_without_waiting_for_shutdown() -> Result<(), ServerError> {
        let (_sender, receiver) = oneshot::channel();
        drain_with_deadline(async { Ok(()) }, receiver, Duration::from_secs(10)).await
    }

    #[test]
    fn server_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ServerError>();
        assert_send_sync::<Infallible>();
    }
}
