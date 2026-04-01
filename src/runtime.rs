//! Thread-per-subsystem runtime inspired by Cloudflare Pingora.
//!
//! Each subsystem (libp2p swarm, epoch pipeline, WASM executor) runs on its
//! own OS thread with its own single-threaded tokio runtime.  The [`Host`]
//! supervisor owns all threads and coordinates shutdown.
//!
//! Executor threads use `current_thread` + `LocalSet` because `wasmtime::Store`
//! is `!Send`.  M:N cell scheduling comes from the AIMD fuel scheduler
//! (`src/cell/proc.rs`), not tokio work stealing.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::Result;
use tokio::sync::{mpsc, watch};

// ---------------------------------------------------------------------------
// Service trait
// ---------------------------------------------------------------------------

/// A subsystem that runs on its own OS thread.
///
/// Implementations should enter a tracing span with the service name
/// (e.g., `tracing::info_span!("swarm")`) for observability.
pub trait Service: Send + 'static {
    /// Run the service until shutdown is signaled.
    /// Returns `Err` for non-panic failures (e.g., swarm fails to bind).
    fn run(self, shutdown: watch::Receiver<()>) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Host supervisor
// ---------------------------------------------------------------------------

/// The Host supervisor owns all subsystem threads and coordinates shutdown.
///
/// ```text
/// Host (Rust-side supervisor)
///  ├── Thread 1: SwarmService    — libp2p event loop
///  ├── Thread 2: EpochService    — on-chain watcher
///  └── Thread 3..N: ExecutorPool — cells via fuel scheduling
/// ```
pub struct Host {
    threads: Vec<(String, JoinHandle<Result<()>>)>,
    shutdown_tx: watch::Sender<()>,
    shutdown_rx: watch::Receiver<()>,
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Host {
    /// Create a new Host supervisor.
    pub fn new() -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        Self {
            threads: Vec::new(),
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Get a shutdown receiver for passing to services or other components.
    pub fn shutdown_rx(&self) -> watch::Receiver<()> {
        self.shutdown_rx.clone()
    }

    /// Spawn a service on its own OS thread.
    pub fn spawn<S: Service>(&mut self, name: &str, service: S) {
        let shutdown = self.shutdown_rx.clone();
        let thread_name = name.to_string();
        let handle = std::thread::Builder::new()
            .name(thread_name.clone())
            .spawn(move || service.run(shutdown))
            .unwrap_or_else(|e| panic!("failed to spawn thread {}: {}", thread_name, e));
        self.threads.push((name.to_string(), handle));
    }

    /// Signal all services to shut down and join all threads.
    ///
    /// Panicked or errored threads are logged but don't prevent other
    /// threads from shutting down.
    pub fn shutdown(self) {
        drop(self.shutdown_tx);
        for (name, handle) in self.threads {
            match handle.join() {
                Ok(Ok(())) => tracing::info!(name, "service stopped"),
                Ok(Err(e)) => tracing::error!(name, error = %e, "service failed"),
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("<non-string panic>");
                    tracing::error!(name, panic = msg, "service panicked");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Executor pool
// ---------------------------------------------------------------------------

/// A factory closure that crosses the thread boundary (Send) and produces
/// a !Send future that runs on the worker's LocalSet.
///
/// The factory receives a shutdown receiver so cells can drain gracefully.
pub type SpawnRequest =
    Box<dyn FnOnce(watch::Receiver<()>) -> Pin<Box<dyn Future<Output = ()>>> + Send>;

/// Pool of executor worker threads for M:N cell scheduling.
///
/// Each worker is an OS thread with a `current_thread` tokio runtime and a
/// `LocalSet`.  Cells are assigned to workers and cooperatively scheduled
/// via the AIMD fuel scheduler.
pub struct ExecutorPool {
    workers: Vec<mpsc::UnboundedSender<SpawnRequest>>,
    cell_counts: Arc<Vec<AtomicUsize>>,
    next: AtomicUsize,
}

impl ExecutorPool {
    /// Create a new executor pool with `n` worker threads.
    ///
    /// Each worker thread runs its own `current_thread` tokio runtime.
    /// Pass `0` to use `std::thread::available_parallelism()`.
    pub fn new(n: usize, shutdown: watch::Receiver<()>) -> Self {
        let n = if n == 0 {
            std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(1)
        } else {
            n
        };

        let mut workers = Vec::with_capacity(n);
        let cell_counts: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
        let cell_counts = Arc::new(cell_counts);

        for i in 0..n {
            let (tx, rx) = mpsc::unbounded_channel();
            let shutdown = shutdown.clone();
            let counts = cell_counts.clone();
            std::thread::Builder::new()
                .name(format!("executor-{}", i))
                .spawn(move || worker_loop(i, rx, shutdown, counts))
                .unwrap_or_else(|e| panic!("failed to spawn executor-{}: {}", i, e));
            workers.push(tx);
        }

        tracing::info!(workers = n, "executor pool started");

        Self {
            workers,
            cell_counts,
            next: AtomicUsize::new(0),
        }
    }

    /// Submit a cell to the pool using least-loaded assignment.
    ///
    /// Returns `Err` if all worker channels are closed (pool is shut down).
    pub fn spawn(&self, request: SpawnRequest) -> Result<(), SpawnRequest> {
        let n = self.workers.len();

        // Find the worker with the fewest cells.
        let mut best = 0;
        let mut best_count = self.cell_counts[0].load(Ordering::Relaxed);
        for i in 1..n {
            let count = self.cell_counts[i].load(Ordering::Relaxed);
            if count < best_count {
                best = i;
                best_count = count;
            }
        }

        // Fall back to round-robin if counts are equal (avoids always
        // picking worker 0 when all counts are the same).
        if best_count > 0 {
            let all_equal =
                (0..n).all(|i| self.cell_counts[i].load(Ordering::Relaxed) == best_count);
            if all_equal {
                best = self.next.fetch_add(1, Ordering::Relaxed) % n;
            }
        }

        match self.workers[best].send(request) {
            Ok(()) => {
                self.cell_counts[best].fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => Err(e.0),
        }
    }

    /// Number of worker threads in the pool.
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }
}

/// Drop guard that decrements the cell count when a cell task completes
/// or panics.  Prevents counter leaks on panic (adversarial finding #3).
struct CellCountGuard {
    counts: Arc<Vec<AtomicUsize>>,
    worker_id: usize,
}

impl Drop for CellCountGuard {
    fn drop(&mut self) {
        // Saturating subtract prevents underflow to usize::MAX (finding #4).
        let _ =
            self.counts[self.worker_id].fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                Some(c.saturating_sub(1))
            });
    }
}

/// The event loop for a single executor worker thread.
///
/// Runs a `current_thread` tokio runtime with a `LocalSet`.  Receives
/// `SpawnRequest` factories over the channel, spawns them as local tasks.
/// Each cell cooperatively yields via the AIMD fuel scheduler.
fn worker_loop(
    id: usize,
    rx: mpsc::UnboundedReceiver<SpawnRequest>,
    shutdown: watch::Receiver<()>,
    cell_counts: Arc<Vec<AtomicUsize>>,
) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| panic!("executor-{}: failed to build runtime: {}", id, e));

    let local = tokio::task::LocalSet::new();
    let _span = tracing::info_span!("executor", worker = id).entered();

    rt.block_on(local.run_until(async move {
        let mut rx = rx;
        let mut shutdown = shutdown;
        loop {
            tokio::select! {
                req = rx.recv() => match req {
                    Some(factory) => {
                        let cell_shutdown = shutdown.clone();
                        let guard = CellCountGuard {
                            counts: cell_counts.clone(),
                            worker_id: id,
                        };
                        tokio::task::spawn_local(async move {
                            let _guard = guard; // dropped on completion or panic
                            factory(cell_shutdown).await;
                        });
                    }
                    None => break, // channel closed
                },
                _ = shutdown.changed() => break,
            }
        }
        tracing::info!("executor worker shutting down");
    }));
}

// ---------------------------------------------------------------------------
// SwarmService
// ---------------------------------------------------------------------------

use crate::host::{KuboBootstrapInfo, SwarmCommand};
use crate::rpc::NetworkState;

/// Parameters for constructing a [`Libp2pHost`] inside the swarm thread.
///
/// The host must be constructed on the same tokio runtime that will poll it,
/// because `with_tokio()` registers TCP listeners with the current reactor.
/// Constructing on one runtime and polling on another is a cross-runtime bug.
pub struct SwarmServiceParams {
    pub port: u16,
    pub keypair: libp2p::identity::Keypair,
    pub kubo_bootstrap: Option<KuboBootstrapInfo>,
    pub kubo_peers: Vec<(libp2p::PeerId, libp2p::Multiaddr)>,
}

/// The libp2p swarm running on its own thread.
///
/// Sends back `stream_control` and `network_state` via oneshot channels
/// after constructing the host on the correct runtime.
pub struct SwarmService {
    pub params: SwarmServiceParams,
    pub cmd_rx: mpsc::Receiver<SwarmCommand>,
    pub ready_tx: tokio::sync::oneshot::Sender<SwarmReady>,
}

/// Values sent back from SwarmService after host construction.
pub struct SwarmReady {
    pub stream_control: libp2p_stream::Control,
    pub network_state: NetworkState,
}

impl Service for SwarmService {
    fn run(self, _shutdown: watch::Receiver<()>) -> Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _span = tracing::info_span!("swarm").entered();

        rt.block_on(async move {
            // Construct the host on THIS runtime so TCP listeners register
            // with the correct reactor.
            let p = self.params;
            let host =
                crate::host::Libp2pHost::new(p.port, p.keypair, p.kubo_bootstrap, p.kubo_peers)?;
            let network_state = NetworkState::from_peer_id(host.local_peer_id().to_bytes());
            let stream_control = host.stream_control();

            // Send construction results back to the main thread.
            let _ = self.ready_tx.send(SwarmReady {
                stream_control,
                network_state: network_state.clone(),
            });

            host.run(network_state, self.cmd_rx).await
        })
    }
}

// ---------------------------------------------------------------------------
// EpochService
// ---------------------------------------------------------------------------

use membrane::Epoch;

/// The on-chain epoch watcher running on its own thread.
pub struct EpochService {
    pub config: atom::IndexerConfig,
    pub epoch_tx: watch::Sender<Epoch>,
    pub confirmation_depth: u64,
    pub ipfs_client: crate::ipfs::HttpClient,
}

impl Service for EpochService {
    fn run(self, _shutdown: watch::Receiver<()>) -> Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _span = tracing::info_span!("epoch").entered();
        rt.block_on(crate::epoch::run_epoch_pipeline(
            self.config,
            self.epoch_tx,
            self.confirmation_depth,
            self.ipfs_client,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    /// A minimal service that sets a flag and waits for shutdown.
    struct FlagService {
        flag: Arc<AtomicBool>,
    }

    impl Service for FlagService {
        fn run(self, mut shutdown: watch::Receiver<()>) -> Result<()> {
            self.flag.store(true, Ordering::SeqCst);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async move {
                let _ = shutdown.changed().await;
            });
            Ok(())
        }
    }

    #[test]
    fn host_spawns_and_shuts_down_services() {
        let mut host = Host::new();
        let flag1 = Arc::new(AtomicBool::new(false));
        let flag2 = Arc::new(AtomicBool::new(false));

        host.spawn(
            "svc-1",
            FlagService {
                flag: flag1.clone(),
            },
        );
        host.spawn(
            "svc-2",
            FlagService {
                flag: flag2.clone(),
            },
        );

        // Give threads a moment to start.
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            flag1.load(Ordering::SeqCst),
            "service 1 should have started"
        );
        assert!(
            flag2.load(Ordering::SeqCst),
            "service 2 should have started"
        );

        host.shutdown();
    }

    #[test]
    fn executor_pool_runs_cells() {
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let pool = ExecutorPool::new(2, shutdown_rx);

        let (tx, rx) = std::sync::mpsc::channel();
        let request: SpawnRequest = Box::new(move |_shutdown| {
            Box::pin(async move {
                tx.send(42).unwrap();
            })
        });

        assert!(pool.spawn(request).is_ok(), "spawn failed");

        let result = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(result, 42);

        drop(shutdown_tx);
        // Give workers time to drain.
        std::thread::sleep(Duration::from_millis(100));
    }

    #[test]
    fn executor_pool_least_loaded_assignment() {
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let pool = ExecutorPool::new(2, shutdown_rx);

        // Spawn a long-running cell on one worker.
        let (block_tx, block_rx) = std::sync::mpsc::channel::<()>();
        let long_cell: SpawnRequest = Box::new(move |_shutdown| {
            Box::pin(async move {
                // Block until signaled.
                let _ = tokio::task::spawn_blocking(move || block_rx.recv()).await;
            })
        });
        assert!(pool.spawn(long_cell).is_ok(), "spawn long_cell failed");
        std::thread::sleep(Duration::from_millis(50));

        // Worker 0 has count=1. Next cell should go to worker 1.
        let (tx, rx) = std::sync::mpsc::channel();
        let short_cell: SpawnRequest = Box::new(move |_shutdown| {
            Box::pin(async move {
                tx.send(()).unwrap();
            })
        });
        assert!(pool.spawn(short_cell).is_ok(), "spawn short_cell failed");
        rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // Clean up.
        let _ = block_tx.send(());
        drop(shutdown_tx);
        std::thread::sleep(Duration::from_millis(100));
    }

    #[test]
    fn host_handles_service_error() {
        struct FailService;
        impl Service for FailService {
            fn run(self, _shutdown: watch::Receiver<()>) -> Result<()> {
                anyhow::bail!("intentional failure")
            }
        }

        let mut host = Host::new();
        host.spawn("fail-svc", FailService);
        std::thread::sleep(Duration::from_millis(50));
        // Should not panic — errors are logged, not propagated.
        host.shutdown();
    }

    #[test]
    fn host_handles_service_panic() {
        struct PanicService;
        impl Service for PanicService {
            fn run(self, _shutdown: watch::Receiver<()>) -> Result<()> {
                panic!("intentional panic")
            }
        }

        let mut host = Host::new();
        host.spawn("panic-svc", PanicService);
        std::thread::sleep(Duration::from_millis(50));
        // Should not panic in the supervisor — panics are caught by join.
        host.shutdown();
    }

    #[test]
    fn executor_pool_spawn_after_shutdown() {
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let pool = ExecutorPool::new(1, shutdown_rx);

        // Shut down the pool.
        drop(shutdown_tx);
        std::thread::sleep(Duration::from_millis(100));

        // Spawn should fail gracefully.
        let request: SpawnRequest = Box::new(|_| Box::pin(async {}));
        assert!(
            pool.spawn(request).is_err(),
            "spawn after shutdown should fail"
        );
    }
}
