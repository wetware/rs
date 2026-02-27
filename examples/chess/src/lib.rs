//! Chess guest: self-play demo and subprotocol handler.
//!
//! This binary serves two roles, selected by the `WW_HANDLER` env var:
//!
//! **Main mode** (no `WW_HANDLER`): Grafts the Membrane to obtain routing +
//! IPFS + Server capabilities, registers a `/ww/0.1.0/chess` subprotocol
//! handler, announces on the DHT as a chess player, and plays a random
//! self-play game.
//!
//! **Handler mode** (`WW_HANDLER=1`): Exports a `ChessEngine` capability
//! over stdin/stdout (which the host has wired to the incoming libp2p
//! stream). Each incoming peer connection spawns a fresh handler instance.

use std::cell::RefCell;
use std::rc::Rc;

use capnp::capability::Promise;
use capnp_rpc::pry;
use shakmaty::fen::Fen;
use shakmaty::uci::UciMove;
use shakmaty::{Chess, EnPassantMode, Position};
use wasip2::cli::stderr::get_stderr;
use wasip2::exports::cli::run::Guest;

// ---------------------------------------------------------------------------
// Cap'n Proto generated modules
// ---------------------------------------------------------------------------

#[allow(dead_code)]
mod system_capnp {
    include!(concat!(env!("OUT_DIR"), "/system_capnp.rs"));
}

#[allow(dead_code)]
mod stem_capnp {
    include!(concat!(env!("OUT_DIR"), "/stem_capnp.rs"));
}

#[allow(dead_code)]
mod ipfs_capnp {
    include!(concat!(env!("OUT_DIR"), "/ipfs_capnp.rs"));
}

#[allow(dead_code)]
mod routing_capnp {
    include!(concat!(env!("OUT_DIR"), "/routing_capnp.rs"));
}

#[allow(dead_code)]
mod chess_capnp {
    include!(concat!(env!("OUT_DIR"), "/chess_capnp.rs"));
}

/// Bootstrap capability: the concrete Membrane defined in stem.capnp.
type Membrane = stem_capnp::membrane::Client;

// ---------------------------------------------------------------------------
// Logging (WASI stderr, same pattern as kernel)
// ---------------------------------------------------------------------------

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Trace
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let stderr = get_stderr();
        let _ = stderr.blocking_write_and_flush(
            format!("[{}] {}\n", record.level(), record.args()).as_bytes(),
        );
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

fn init_logging() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
}

// ---------------------------------------------------------------------------
// ChessEngineImpl — shakmaty-backed Cap'n Proto server
// ---------------------------------------------------------------------------

/// Chess engine backed by shakmaty.
///
/// Implements `chess_capnp::chess_engine::Server`. In PR 2 this is used
/// locally for unit testing; PR 3 exports it via `system::serve`.
pub struct ChessEngineImpl {
    pos: RefCell<Chess>,
}

impl Default for ChessEngineImpl {
    fn default() -> Self {
        Self {
            pos: RefCell::new(Chess::default()),
        }
    }
}

impl ChessEngineImpl {
    pub fn new() -> Self {
        Self {
            pos: RefCell::new(Chess::default()),
        }
    }

    // -- Direct accessors for unit tests (no RPC round-trip) --

    pub fn fen(&self) -> String {
        Fen::from_position(self.pos.borrow().clone(), EnPassantMode::Legal).to_string()
    }

    pub fn apply(&self, uci: &str) -> Result<(), String> {
        let uci_move: UciMove = uci
            .parse()
            .map_err(|e| format!("invalid UCI '{uci}': {e}"))?;
        let mut pos = self.pos.borrow_mut();
        let m = uci_move
            .to_move(&*pos)
            .map_err(|e| format!("illegal move '{uci}': {e}"))?;
        pos.play_unchecked(&m);
        Ok(())
    }

    pub fn legal_moves_uci(&self) -> Vec<String> {
        let pos = self.pos.borrow();
        pos.legal_moves()
            .iter()
            .map(|m| UciMove::from_standard(m).to_string())
            .collect()
    }

    pub fn status(&self) -> chess_capnp::chess_engine::GameStatus {
        use chess_capnp::chess_engine::GameStatus;
        let pos = self.pos.borrow();
        if pos.is_checkmate() {
            GameStatus::Checkmate
        } else if pos.is_stalemate() {
            GameStatus::Stalemate
        } else if pos.is_insufficient_material() {
            GameStatus::Draw
        } else {
            GameStatus::Ongoing
        }
    }
}

#[allow(refining_impl_trait)]
impl chess_capnp::chess_engine::Server for ChessEngineImpl {
    fn get_state(
        self: Rc<Self>,
        _params: chess_capnp::chess_engine::GetStateParams,
        mut results: chess_capnp::chess_engine::GetStateResults,
    ) -> Promise<(), capnp::Error> {
        results.get().set_fen(self.fen());
        Promise::ok(())
    }

    fn apply_move(
        self: Rc<Self>,
        params: chess_capnp::chess_engine::ApplyMoveParams,
        mut results: chess_capnp::chess_engine::ApplyMoveResults,
    ) -> Promise<(), capnp::Error> {
        let uci = pry!(pry!(params.get()).get_uci()).to_str().unwrap_or("");
        match self.apply(uci) {
            Ok(()) => {
                results.get().set_ok(true);
                results.get().set_reason("");
            }
            Err(reason) => {
                results.get().set_ok(false);
                results.get().set_reason(&reason);
            }
        }
        Promise::ok(())
    }

    fn get_legal_moves(
        self: Rc<Self>,
        _params: chess_capnp::chess_engine::GetLegalMovesParams,
        mut results: chess_capnp::chess_engine::GetLegalMovesResults,
    ) -> Promise<(), capnp::Error> {
        let moves = self.legal_moves_uci();
        let mut list = results.get().init_moves(moves.len() as u32);
        for (i, m) in moves.iter().enumerate() {
            list.set(i as u32, m);
        }
        Promise::ok(())
    }

    fn get_status(
        self: Rc<Self>,
        _params: chess_capnp::chess_engine::GetStatusParams,
        mut results: chess_capnp::chess_engine::GetStatusResults,
    ) -> Promise<(), capnp::Error> {
        results.get().set_status(self.status());
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// LoggingSink — ProviderSink that logs discovered peers
// ---------------------------------------------------------------------------

struct LoggingSink;

#[allow(refining_impl_trait)]
impl routing_capnp::provider_sink::Server for LoggingSink {
    fn provider(
        self: Rc<Self>,
        params: routing_capnp::provider_sink::ProviderParams,
    ) -> Promise<(), capnp::Error> {
        let peer_id = pry!(pry!(pry!(params.get()).get_info()).get_peer_id());
        log::info!("found chess peer: {}", hex::encode(peer_id));
        Promise::ok(())
    }

    fn done(
        self: Rc<Self>,
        _params: routing_capnp::provider_sink::DoneParams,
        _results: routing_capnp::provider_sink::DoneResults,
    ) -> Promise<(), capnp::Error> {
        log::info!("provider search complete");
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// Game loop
// ---------------------------------------------------------------------------

async fn run_game(membrane: Membrane) -> Result<(), capnp::Error> {
    // Graft membrane → get capabilities.
    let graft_resp = membrane.graft_request().send().promise.await?;
    let results = graft_resp.get()?;
    let host = results.get_host()?;
    let executor = results.get_executor()?;
    let ipfs = results.get_ipfs()?;
    let routing = results.get_routing()?;
    let server = results.get_server()?;

    // Register /ww/0.1.0/chess subprotocol handler.
    // Read our own binary so handler instances run the same code in handler mode.
    let wasm_bytes = std::fs::read("/bin/main.wasm")
        .map_err(|e| capnp::Error::failed(format!("read handler wasm: {e}")))?;

    // OCAP: pass our executor to server.serve(), explicitly delegating spawn rights.
    // A future PR could wrap executor in an attenuating proxy to restrict handler
    // resources (memory, CPU, network) before delegating — Server treats it opaquely.
    let mut serve_req = server.serve_request();
    serve_req.get().set_executor(executor);
    serve_req.get().set_protocol("chess");
    serve_req.get().set_handler(&wasm_bytes);
    serve_req.send().promise.await?;
    log::info!("registered /ww/0.1.0/chess subprotocol handler");

    // Log peer identity.
    let id_resp = host.id_request().send().promise.await?;
    let peer_id = id_resp.get()?.get_peer_id()?;
    log::info!("peer id: {}", hex::encode(peer_id));

    // Get UnixFS API.
    let unixfs_resp = ipfs.unixfs_request().send().promise.await?;
    let unixfs = unixfs_resp.get()?.get_api()?;

    // Hash chess namespace → deterministic CID (same on all nodes).
    let mut add_req = unixfs.add_request();
    add_req.get().set_data(b"wetware.chess.v1");
    let add_resp = add_req.send().promise.await?;
    let ns_cid = add_resp.get()?.get_cid()?.to_str()?.to_string();
    log::info!("chess namespace CID: {ns_cid}");

    // Announce as chess player.
    let mut provide_req = routing.provide_request();
    provide_req.get().set_key(&ns_cid);
    provide_req.send().promise.await?;
    log::info!("providing chess namespace");

    // Discover other chess peers (fire-and-forget; results logged by sink).
    let sink: routing_capnp::provider_sink::Client = capnp_rpc::new_client(LoggingSink);
    let mut fp_req = routing.find_providers_request();
    {
        let mut b = fp_req.get();
        b.set_key(&ns_cid);
        b.set_count(5);
        b.set_sink(sink);
    }
    fp_req.send().promise.await?;

    // Self-play game loop.
    let mut pos = Chess::default();
    let mut move_num = 0u32;
    loop {
        let moves = pos.legal_moves();
        if moves.is_empty() {
            break;
        }

        // Pick a random move.
        let idx = rand::random_range(0..moves.len());
        let mv = moves[idx].clone();
        pos.play_unchecked(&mv);
        move_num += 1;

        let uci = UciMove::from_standard(&mv).to_string();
        let fen = Fen::from_position(pos.clone(), EnPassantMode::Legal).to_string();

        // Store board state in IPFS.
        let mut add_req = unixfs.add_request();
        add_req.get().set_data(fen.as_bytes());
        let add_resp = add_req.send().promise.await?;
        let cid = add_resp.get()?.get_cid()?.to_str()?.to_string();

        // Announce board state on DHT.
        let mut provide_req = routing.provide_request();
        provide_req.get().set_key(&cid);
        provide_req.send().promise.await?;

        log::info!("{move_num}. {uci} → {fen} ({cid})");

        if pos.is_checkmate() || pos.is_stalemate() || pos.is_insufficient_material() {
            break;
        }
    }

    // Log game result.
    let status = if pos.is_checkmate() {
        "checkmate"
    } else if pos.is_stalemate() {
        "stalemate"
    } else if pos.is_insufficient_material() {
        "draw (insufficient material)"
    } else {
        "unknown"
    };
    log::info!("game over after {move_num} moves: {status}");

    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

struct ChessGuest;

impl Guest for ChessGuest {
    fn run() -> Result<(), ()> {
        init_logging();
        if std::env::var("WW_HANDLER").is_ok() {
            // Handler mode: export ChessEngine over stdin/stdout.
            // The host wires stdin/stdout to the incoming libp2p stream.
            let engine: chess_capnp::chess_engine::Client =
                capnp_rpc::new_client(ChessEngineImpl::new());
            system::serve_stdio(engine.client);
        } else {
            // Main mode: register subprotocol handler, self-play.
            system::run(|membrane: Membrane| async move { run_game(membrane).await });
        }
        Ok(())
    }
}

wasip2::cli::command::export!(ChessGuest);

// ---------------------------------------------------------------------------
// Unit tests (native, no RPC needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chess_capnp::chess_engine::GameStatus;

    #[test]
    fn test_initial_fen() {
        let engine = ChessEngineImpl::new();
        assert_eq!(
            engine.fen(),
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
        );
    }

    #[test]
    fn test_apply_valid_move() {
        let engine = ChessEngineImpl::new();
        assert!(engine.apply("e2e4").is_ok());
        assert!(engine.fen().contains("4P3")); // pawn on e4
    }

    #[test]
    fn test_apply_invalid_move() {
        let engine = ChessEngineImpl::new();
        let result = engine.apply("e1e5"); // king can't jump to e5
        assert!(result.is_err());
    }

    #[test]
    fn test_legal_moves_count() {
        let engine = ChessEngineImpl::new();
        // Starting position has 20 legal moves (16 pawn + 4 knight).
        assert_eq!(engine.legal_moves_uci().len(), 20);
    }

    #[test]
    fn test_game_status_ongoing() {
        let engine = ChessEngineImpl::new();
        assert_eq!(engine.status(), GameStatus::Ongoing);
    }

    #[test]
    fn test_scholars_mate() {
        let engine = ChessEngineImpl::new();
        // 1. e4 e5 2. Bc4 Nc6 3. Qh5 Nf6 4. Qxf7#
        for uci in &["e2e4", "e7e5", "f1c4", "b8c6", "d1h5", "g8f6", "h5f7"] {
            engine.apply(uci).unwrap_or_else(|e| panic!("move {uci} failed: {e}"));
        }
        assert_eq!(engine.status(), GameStatus::Checkmate);
    }

    // -----------------------------------------------------------------------
    // RPC round-trip tests (Cap'n Proto over in-memory duplex)
    // -----------------------------------------------------------------------

    use capnp_rpc::rpc_twoparty_capnp::Side;
    use capnp_rpc::twoparty::VatNetwork;
    use capnp_rpc::RpcSystem;
    use tokio::io;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    /// Bootstrap a ChessEngine client/server pair over in-memory duplex.
    fn setup_engine() -> chess_capnp::chess_engine::Client {
        let (client_stream, server_stream) = io::duplex(64 * 1024);
        let (client_read, client_write) = io::split(client_stream);
        let (server_read, server_write) = io::split(server_stream);

        let engine_server: chess_capnp::chess_engine::Client =
            capnp_rpc::new_client(ChessEngineImpl::new());

        let server_network = VatNetwork::new(
            server_read.compat(),
            server_write.compat_write(),
            Side::Server,
            Default::default(),
        );
        let server_rpc = RpcSystem::new(Box::new(server_network), Some(engine_server.client));
        tokio::task::spawn_local(async move {
            let _ = server_rpc.await;
        });

        let client_network = VatNetwork::new(
            client_read.compat(),
            client_write.compat_write(),
            Side::Client,
            Default::default(),
        );
        let mut client_rpc = RpcSystem::new(Box::new(client_network), None);
        let client: chess_capnp::chess_engine::Client = client_rpc.bootstrap(Side::Server);
        tokio::task::spawn_local(async move {
            let _ = client_rpc.await;
        });

        client
    }

    #[tokio::test]
    async fn test_rpc_get_state() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let client = setup_engine();
                let resp = client.get_state_request().send().promise.await.unwrap();
                let fen = resp.get().unwrap().get_fen().unwrap().to_str().unwrap();
                assert_eq!(
                    fen,
                    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
                );
            })
            .await;
    }

    #[tokio::test]
    async fn test_rpc_apply_move_and_get_state() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let client = setup_engine();

                // Apply e2e4.
                let mut req = client.apply_move_request();
                req.get().set_uci("e2e4");
                let resp = req.send().promise.await.unwrap();
                let result = resp.get().unwrap();
                assert!(result.get_ok());

                // Verify FEN reflects the move.
                let resp = client.get_state_request().send().promise.await.unwrap();
                let fen = resp.get().unwrap().get_fen().unwrap().to_str().unwrap();
                assert!(fen.contains("4P3"), "expected pawn on e4, got: {fen}");
            })
            .await;
    }

    #[tokio::test]
    async fn test_rpc_apply_illegal_move() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let client = setup_engine();

                let mut req = client.apply_move_request();
                req.get().set_uci("e1e5"); // king can't jump to e5
                let resp = req.send().promise.await.unwrap();
                let result = resp.get().unwrap();
                assert!(!result.get_ok());
                let reason = result.get_reason().unwrap().to_str().unwrap();
                assert!(!reason.is_empty(), "expected error reason");
            })
            .await;
    }

    #[tokio::test]
    async fn test_rpc_get_legal_moves() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let client = setup_engine();
                let resp = client
                    .get_legal_moves_request()
                    .send()
                    .promise
                    .await
                    .unwrap();
                let moves = resp.get().unwrap().get_moves().unwrap();
                assert_eq!(moves.len(), 20); // 16 pawn + 4 knight
            })
            .await;
    }

    #[tokio::test]
    async fn test_rpc_scholars_mate_status() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let client = setup_engine();

                for uci in &["e2e4", "e7e5", "f1c4", "b8c6", "d1h5", "g8f6", "h5f7"] {
                    let mut req = client.apply_move_request();
                    req.get().set_uci(uci);
                    let resp = req.send().promise.await.unwrap();
                    assert!(
                        resp.get().unwrap().get_ok(),
                        "move {uci} rejected over RPC"
                    );
                }

                let resp = client.get_status_request().send().promise.await.unwrap();
                let status = resp.get().unwrap().get_status().unwrap();
                assert_eq!(status, GameStatus::Checkmate);
            })
            .await;
    }
}
