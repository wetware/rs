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
// Metrics HTTP handler
// ---------------------------------------------------------------------------

/// Shared state for the metrics axum handler.
#[derive(Clone)]
struct MetricsState {
    fuel_registry: FuelRegistry,
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

    #[test]
    fn render_empty_registry() {
        let state = MetricsState {
            fuel_registry: new_fuel_registry(),
        };
        let output = render_metrics(&state);
        assert!(output.contains("# TYPE ww_cell_fuel_remaining gauge"));
        assert!(output.contains("# TYPE ww_cell_fuel_consumed_total counter"));
        assert!(output.contains("# TYPE ww_auction_bids_total counter"));
        // No cell lines when registry is empty.
        assert!(!output.contains("cell_id="));
    }

    #[test]
    fn render_with_cells() {
        let registry = new_fuel_registry();
        {
            let mut map = registry.write().unwrap();
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
        let state = MetricsState {
            fuel_registry: registry,
        };
        let output = render_metrics(&state);
        assert!(output.contains("ww_cell_fuel_remaining{cell_id=\"kernel\"} 500000"));
        assert!(output.contains("ww_cell_fuel_consumed_total{cell_id=\"kernel\"} 1200000"));
        assert!(output.contains("ww_cell_fuel_remaining{cell_id=\"worker-1\"} 0"));
        assert!(output.contains("ww_cell_fuel_consumed_total{cell_id=\"worker-1\"} 5000000"));
    }
}
