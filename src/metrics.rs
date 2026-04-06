//! Prometheus metrics endpoint for fuel auction observability.
//!
//! Phase 1: exposes per-cell fuel metrics (`ww_cell_fuel_remaining`,
//! `ww_cell_fuel_consumed_total`) from host-side [`FuelEstimator`] state.
//!
//! Auction-specific metrics (`ww_auction_*`) are stubbed — they require
//! the host to hold a `ComputeProvider` client reference, which will be
//! wired in a future PR.
//!
//! The endpoint is a minimal axum handler that responds to `GET /metrics`
//! with Prometheus text exposition format (text/plain; version=0.0.4).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use axum::{extract::State, response::IntoResponse, routing::get, Router};
use tokio::sync::watch;

// ---------------------------------------------------------------------------
// Per-cell fuel snapshot
// ---------------------------------------------------------------------------

/// A point-in-time snapshot of a cell's fuel state, published by the epoch
/// callback and consumed by the metrics scrape handler.
#[derive(Clone, Debug)]
pub struct CellFuelSnapshot {
    /// Fuel remaining in the current epoch budget.
    pub remaining: u64,
    /// Cumulative fuel consumed over the cell's lifetime.
    pub consumed_total: u64,
}

/// Shared registry of per-cell fuel snapshots.
///
/// Keys are cell identifiers (e.g. "kernel", or a CID-derived name for
/// spawned children).  The epoch callback writes; the metrics handler reads.
pub type FuelRegistry = Arc<RwLock<HashMap<String, CellFuelSnapshot>>>;

/// Create a new, empty fuel registry.
pub fn new_fuel_registry() -> FuelRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// RPC latency histogram (Prometheus native format)
// ---------------------------------------------------------------------------

/// Fixed-bucket Prometheus histogram for RPC call latency.
/// Constant memory: 9 bucket counters + sum + count per method.
#[derive(Clone, Debug)]
pub struct LatencyHistogram {
    /// (le_boundary, count) pairs. Last entry is +Inf.
    buckets: [(f64, u64); 9],
    sum: f64,
    count: u64,
}

const HISTOGRAM_BOUNDARIES: [f64; 8] = [0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0];

impl LatencyHistogram {
    fn new() -> Self {
        let mut buckets = [(0.0, 0u64); 9];
        for (i, &le) in HISTOGRAM_BOUNDARIES.iter().enumerate() {
            buckets[i].0 = le;
        }
        buckets[8].0 = f64::INFINITY; // +Inf bucket
        Self {
            buckets,
            sum: 0.0,
            count: 0,
        }
    }

    /// Record an observation (duration in seconds).
    pub fn observe(&mut self, value: f64) {
        self.sum += value;
        self.count += 1;
        for bucket in &mut self.buckets {
            if value <= bucket.0 {
                bucket.1 += 1;
            }
        }
    }
}

/// Per-method RPC metrics: call counts + latency histograms.
pub struct RpcMetrics {
    pub histograms: HashMap<String, LatencyHistogram>,
    pub calls_total: HashMap<String, u64>,
}

impl RpcMetrics {
    fn new() -> Self {
        Self {
            histograms: HashMap::new(),
            calls_total: HashMap::new(),
        }
    }

    /// Record an RPC call with its duration in seconds.
    pub fn observe(&mut self, method: &str, duration_secs: f64) {
        *self.calls_total.entry(method.to_string()).or_insert(0) += 1;
        self.histograms
            .entry(method.to_string())
            .or_insert_with(LatencyHistogram::new)
            .observe(duration_secs);
    }
}

pub type RpcMetricsRegistry = Arc<RwLock<RpcMetrics>>;

/// Create a new, empty RPC metrics registry.
pub fn new_rpc_metrics() -> RpcMetricsRegistry {
    Arc::new(RwLock::new(RpcMetrics::new()))
}

/// Cache hit/miss/eviction counters + current state gauges.
pub struct CacheMetrics {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub weight_bytes: u64,
    pub entries: u64,
}

impl CacheMetrics {
    fn new() -> Self {
        Self {
            hits: 0,
            misses: 0,
            evictions: 0,
            weight_bytes: 0,
            entries: 0,
        }
    }
}

pub type CacheMetricsRegistry = Arc<RwLock<CacheMetrics>>;

/// Create a new, empty cache metrics registry.
pub fn new_cache_metrics() -> CacheMetricsRegistry {
    Arc::new(RwLock::new(CacheMetrics::new()))
}

/// Stream pump throughput counters.
pub struct StreamMetrics {
    pub bytes_pumped_total: u64,
    pub pump_ops_total: u64,
}

impl StreamMetrics {
    fn new() -> Self {
        Self {
            bytes_pumped_total: 0,
            pump_ops_total: 0,
        }
    }
}

pub type StreamMetricsRegistry = Arc<RwLock<StreamMetrics>>;

/// Create a new, empty stream metrics registry.
pub fn new_stream_metrics() -> StreamMetricsRegistry {
    Arc::new(RwLock::new(StreamMetrics::new()))
}

// ---------------------------------------------------------------------------
// Metrics HTTP handler
// ---------------------------------------------------------------------------

/// Shared state for the metrics axum handler.
#[derive(Clone)]
struct MetricsState {
    fuel_registry: FuelRegistry,
    rpc_metrics: RpcMetricsRegistry,
    cache_metrics: CacheMetricsRegistry,
    stream_metrics: StreamMetricsRegistry,
}

/// Render all metrics in Prometheus text exposition format.
fn render_metrics(state: &MetricsState) -> String {
    let mut out = String::with_capacity(2048);

    // ---- Auction metrics (Phase 1: stubs) ----
    //
    // These require a ComputeProvider client reference to query the auction
    // vat cell's status() method.  Wired in a future PR.

    out.push_str("# HELP ww_auction_bids_total Total bids processed.\n");
    out.push_str("# TYPE ww_auction_bids_total counter\n");
    // TODO: populate from ComputeProvider.status() when available
    // ww_auction_bids_total{status="accepted"} 0
    // ww_auction_bids_total{status="rejected"} 0

    out.push_str("# HELP ww_auction_capacity_fuel Total fuel capacity this epoch.\n");
    out.push_str("# TYPE ww_auction_capacity_fuel gauge\n");
    // TODO: populate from ComputeProvider.status()

    out.push_str("# HELP ww_auction_available_fuel Uncommitted fuel capacity.\n");
    out.push_str("# TYPE ww_auction_available_fuel gauge\n");
    // TODO: populate from ComputeProvider.status()

    out.push_str("# HELP ww_auction_utilization_ratio Committed / total fuel.\n");
    out.push_str("# TYPE ww_auction_utilization_ratio gauge\n");
    // TODO: populate from ComputeProvider.status()

    out.push_str("# HELP ww_auction_price_per_mfuel Current posted price.\n");
    out.push_str("# TYPE ww_auction_price_per_mfuel gauge\n");
    // TODO: populate from ComputeProvider.status()

    out.push_str("# HELP ww_auction_active_tickets Active fuel tickets.\n");
    out.push_str("# TYPE ww_auction_active_tickets gauge\n");
    // TODO: populate from ComputeProvider.status()

    // ---- Per-cell fuel metrics (Phase 1: live) ----

    out.push_str("# HELP ww_cell_fuel_remaining Per-cell remaining fuel budget.\n");
    out.push_str("# TYPE ww_cell_fuel_remaining gauge\n");

    out.push_str("# HELP ww_cell_fuel_consumed_total Per-cell cumulative fuel consumed.\n");
    out.push_str("# TYPE ww_cell_fuel_consumed_total counter\n");

    if let Ok(registry) = state.fuel_registry.read() {
        for (cell_id, snap) in registry.iter() {
            out.push_str(&format!(
                "ww_cell_fuel_remaining{{cell_id=\"{}\"}} {}\n",
                cell_id, snap.remaining,
            ));
            out.push_str(&format!(
                "ww_cell_fuel_consumed_total{{cell_id=\"{}\"}} {}\n",
                cell_id, snap.consumed_total,
            ));
        }
    }

    // ---- RPC metrics ----

    out.push_str("# HELP ww_rpc_calls_total Total RPC calls by method.\n");
    out.push_str("# TYPE ww_rpc_calls_total counter\n");

    out.push_str("# HELP ww_rpc_duration_seconds RPC call latency.\n");
    out.push_str("# TYPE ww_rpc_duration_seconds histogram\n");

    if let Ok(rpc) = state.rpc_metrics.read() {
        for (method, count) in &rpc.calls_total {
            out.push_str(&format!(
                "ww_rpc_calls_total{{method=\"{method}\"}} {count}\n",
            ));
        }
        for (method, hist) in &rpc.histograms {
            for &(le, count) in &hist.buckets {
                let le_str = if le.is_infinite() {
                    "+Inf".to_string()
                } else {
                    format!("{le}")
                };
                out.push_str(&format!(
                    "ww_rpc_duration_seconds_bucket{{method=\"{method}\",le=\"{le_str}\"}} {count}\n",
                ));
            }
            out.push_str(&format!(
                "ww_rpc_duration_seconds_sum{{method=\"{method}\"}} {}\n",
                hist.sum,
            ));
            out.push_str(&format!(
                "ww_rpc_duration_seconds_count{{method=\"{method}\"}} {}\n",
                hist.count,
            ));
        }
    }

    // ---- Cache metrics ----

    out.push_str("# HELP ww_cache_hits_total ARC cache hits.\n");
    out.push_str("# TYPE ww_cache_hits_total counter\n");
    out.push_str("# HELP ww_cache_misses_total ARC cache misses.\n");
    out.push_str("# TYPE ww_cache_misses_total counter\n");
    out.push_str("# HELP ww_cache_evictions_total ARC cache evictions.\n");
    out.push_str("# TYPE ww_cache_evictions_total counter\n");
    out.push_str("# HELP ww_cache_weight_bytes Current ARC cache weight in bytes.\n");
    out.push_str("# TYPE ww_cache_weight_bytes gauge\n");
    out.push_str("# HELP ww_cache_entries Current ARC cache entry count.\n");
    out.push_str("# TYPE ww_cache_entries gauge\n");

    if let Ok(cache) = state.cache_metrics.read() {
        out.push_str(&format!("ww_cache_hits_total {}\n", cache.hits));
        out.push_str(&format!("ww_cache_misses_total {}\n", cache.misses));
        out.push_str(&format!("ww_cache_evictions_total {}\n", cache.evictions));
        out.push_str(&format!("ww_cache_weight_bytes {}\n", cache.weight_bytes));
        out.push_str(&format!("ww_cache_entries {}\n", cache.entries));
    }

    // ---- Stream metrics ----

    out.push_str("# HELP ww_stream_bytes_pumped_total Total bytes pumped through stream listeners.\n");
    out.push_str("# TYPE ww_stream_bytes_pumped_total counter\n");
    out.push_str("# HELP ww_stream_pump_ops_total Total pump read/write cycles.\n");
    out.push_str("# TYPE ww_stream_pump_ops_total counter\n");

    if let Ok(stream) = state.stream_metrics.read() {
        out.push_str(&format!(
            "ww_stream_bytes_pumped_total {}\n",
            stream.bytes_pumped_total,
        ));
        out.push_str(&format!(
            "ww_stream_pump_ops_total {}\n",
            stream.pump_ops_total,
        ));
    }

    out
}

/// `GET /metrics` handler.
async fn metrics_handler(State(state): State<MetricsState>) -> impl IntoResponse {
    let body = render_metrics(&state);
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

// ---------------------------------------------------------------------------
// MetricsService (runtime::Service implementation)
// ---------------------------------------------------------------------------

/// A [`crate::runtime::Service`] that serves Prometheus metrics over HTTP.
pub struct MetricsService {
    pub listen_addr: SocketAddr,
    pub fuel_registry: FuelRegistry,
    pub rpc_metrics: RpcMetricsRegistry,
    pub cache_metrics: CacheMetricsRegistry,
    pub stream_metrics: StreamMetricsRegistry,
}

impl crate::runtime::Service for MetricsService {
    fn run(self, mut shutdown: watch::Receiver<()>) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _span = tracing::info_span!("metrics").entered();

        rt.block_on(async move {
            let state = MetricsState {
                fuel_registry: self.fuel_registry,
                rpc_metrics: self.rpc_metrics,
                cache_metrics: self.cache_metrics,
                stream_metrics: self.stream_metrics,
            };

            let app = Router::new()
                .route("/metrics", get(metrics_handler))
                .with_state(state);

            let listener = tokio::net::TcpListener::bind(self.listen_addr).await?;
            let local_addr = listener.local_addr()?;
            tracing::info!(%local_addr, "Prometheus metrics server listening");

            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown.changed().await;
                    tracing::info!("Metrics server shutting down");
                })
                .await?;

            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> MetricsState {
        MetricsState {
            fuel_registry: new_fuel_registry(),
            rpc_metrics: new_rpc_metrics(),
            cache_metrics: new_cache_metrics(),
            stream_metrics: new_stream_metrics(),
        }
    }

    #[test]
    fn render_empty_registry() {
        let state = test_state();
        let output = render_metrics(&state);
        assert!(output.contains("# TYPE ww_cell_fuel_remaining gauge"));
        assert!(output.contains("# TYPE ww_cell_fuel_consumed_total counter"));
        assert!(output.contains("# TYPE ww_auction_bids_total counter"));
        assert!(output.contains("# TYPE ww_rpc_calls_total counter"));
        assert!(output.contains("# TYPE ww_rpc_duration_seconds histogram"));
        assert!(output.contains("# TYPE ww_cache_hits_total counter"));
        assert!(output.contains("# TYPE ww_stream_bytes_pumped_total counter"));
        // No data lines when registries are empty.
        assert!(!output.contains("cell_id="));
        assert!(!output.contains("method="));
    }

    #[test]
    fn render_with_cells() {
        let state = test_state();
        {
            let mut map = state.fuel_registry.write().unwrap();
            map.insert(
                "kernel".into(),
                CellFuelSnapshot {
                    remaining: 500_000,
                    consumed_total: 1_200_000,
                },
            );
            map.insert(
                "worker-1".into(),
                CellFuelSnapshot {
                    remaining: 0,
                    consumed_total: 5_000_000,
                },
            );
        }
        let output = render_metrics(&state);
        assert!(output.contains("ww_cell_fuel_remaining{cell_id=\"kernel\"} 500000"));
        assert!(output.contains("ww_cell_fuel_consumed_total{cell_id=\"kernel\"} 1200000"));
        assert!(output.contains("ww_cell_fuel_remaining{cell_id=\"worker-1\"} 0"));
        assert!(output.contains("ww_cell_fuel_consumed_total{cell_id=\"worker-1\"} 5000000"));
    }

    #[test]
    fn render_rpc_histogram() {
        let state = test_state();
        {
            let mut rpc = state.rpc_metrics.write().unwrap();
            rpc.observe("host.id", 0.005);
            rpc.observe("host.id", 0.050);
        }
        let output = render_metrics(&state);
        // 0.005s falls in le=0.005 bucket (<=)
        assert!(output.contains("ww_rpc_duration_seconds_bucket{method=\"host.id\",le=\"0.005\"} 1"));
        // 0.050s falls in le=0.05 bucket
        assert!(output.contains("ww_rpc_duration_seconds_bucket{method=\"host.id\",le=\"0.05\"} 2"));
        // +Inf always has all observations
        assert!(output.contains("ww_rpc_duration_seconds_bucket{method=\"host.id\",le=\"+Inf\"} 2"));
        assert!(output.contains("ww_rpc_duration_seconds_count{method=\"host.id\"} 2"));
        assert!(output.contains("ww_rpc_calls_total{method=\"host.id\"} 2"));
    }

    #[test]
    fn render_rpc_histogram_empty() {
        let state = test_state();
        let output = render_metrics(&state);
        assert!(output.contains("# HELP ww_rpc_duration_seconds"));
        assert!(output.contains("# TYPE ww_rpc_duration_seconds histogram"));
        // No bucket lines when no observations
        assert!(!output.contains("ww_rpc_duration_seconds_bucket"));
    }

    #[test]
    fn render_cache_metrics() {
        let state = test_state();
        {
            let mut cache = state.cache_metrics.write().unwrap();
            cache.hits = 42;
            cache.misses = 7;
            cache.evictions = 3;
            cache.weight_bytes = 1_048_576;
            cache.entries = 100;
        }
        let output = render_metrics(&state);
        assert!(output.contains("ww_cache_hits_total 42"));
        assert!(output.contains("ww_cache_misses_total 7"));
        assert!(output.contains("ww_cache_evictions_total 3"));
        assert!(output.contains("ww_cache_weight_bytes 1048576"));
        assert!(output.contains("ww_cache_entries 100"));
    }

    #[test]
    fn render_stream_metrics() {
        let state = test_state();
        {
            let mut stream = state.stream_metrics.write().unwrap();
            stream.bytes_pumped_total = 1_000_000;
            stream.pump_ops_total = 500;
        }
        let output = render_metrics(&state);
        assert!(output.contains("ww_stream_bytes_pumped_total 1000000"));
        assert!(output.contains("ww_stream_pump_ops_total 500"));
    }
}
