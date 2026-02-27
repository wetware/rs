//! Chess guest: cross-node play via subprotocol handlers.
//!
//! This binary serves two roles, selected by env vars:
//!
//! **Main mode** (no `WW_HANDLER`): Grafts the Membrane to obtain
//! `host.network()` → `(Listener, Dialer)`, registers a `/ww/0.1.0/chess`
//! listener, announces on the DHT, discovers peers, and dials them.
//! The dialer side plays random moves against the remote handler via
//! a text-based UCI protocol over the returned ByteStream.
//!
//! **Handler mode** (`WW_HANDLER=1`): A pure bytestream handler spawned
//! by the Listener. Reads newline-delimited UCI moves from stdin, applies
//! them, picks a random legal response, writes it to stdout.

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
// Handler mode — pure bytestream chess engine over stdin/stdout
// ---------------------------------------------------------------------------

/// Text-based chess handler for Listener-spawned processes.
///
/// Protocol: newline-delimited UCI moves.
/// - Reads a UCI move from stdin (e.g. "e2e4\n")
/// - Applies opponent's move to the local engine
/// - Picks a random legal response
/// - Writes it to stdout (e.g. "e7e5\n")
/// - Repeats until game over or EOF
fn handle_chess_stream() {
    let engine = ChessEngineImpl::new();
    let stdin = wasip2::cli::stdin::get_stdin();
    let stdout = wasip2::cli::stdout::get_stdout();
    let mut buf = Vec::new();

    log::info!("handler started, waiting for moves");

    loop {
        // Accumulate data — blocking_read returns arbitrary chunks, not lines.
        let data = match stdin.blocking_read(4096) {
            Ok(d) if d.is_empty() => break,
            Ok(d) => d,
            Err(_) => break,
        };
        buf.extend_from_slice(&data);

        // Process complete lines.
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
            let uci = match std::str::from_utf8(&line_bytes) {
                Ok(s) => s.trim().to_string(),
                Err(_) => continue,
            };
            if uci.is_empty() {
                continue;
            }

            // Apply opponent's move.
            if let Err(e) = engine.apply(&uci) {
                log::error!("invalid move from peer '{uci}': {e}");
                return;
            }
            log::info!("opponent: {uci}");

            // Check if game over after opponent's move.
            if engine.legal_moves_uci().is_empty() {
                log::info!("game over after opponent's move ({})", engine.fen());
                return;
            }

            // Pick a random response.
            let moves = engine.legal_moves_uci();
            let response = &moves[rand::random_range(0..moves.len())];
            engine.apply(response).unwrap();
            log::info!("response: {response}");

            // Send response.
            let _ = stdout.blocking_write_and_flush(format!("{response}\n").as_bytes());

            // Check if game over after our move.
            if engine.legal_moves_uci().is_empty() {
                log::info!("game over after our move ({})", engine.fen());
                return;
            }
        }
    }

    log::info!("handler: stdin closed");
}

// ---------------------------------------------------------------------------
// DialingSink — discovers peers and dials them on the chess subprotocol
// ---------------------------------------------------------------------------

struct DialingSink {
    dialer: system_capnp::dialer::Client,
    self_id: Vec<u8>,
}

#[allow(refining_impl_trait)]
impl routing_capnp::provider_sink::Server for DialingSink {
    fn provider(
        self: Rc<Self>,
        params: routing_capnp::provider_sink::ProviderParams,
    ) -> Promise<(), capnp::Error> {
        let peer_id = pry!(pry!(pry!(params.get()).get_info()).get_peer_id()).to_vec();

        // Skip self.
        if peer_id == self.self_id {
            log::info!("discovered self, skipping");
            return Promise::ok(());
        }

        log::info!(
            "discovered chess peer: {}, dialing...",
            hex::encode(&peer_id)
        );

        let dialer = self.dialer.clone();
        let peer = peer_id.clone();

        Promise::from_future(async move {
            if let Err(e) = play_against_peer(&dialer, &peer).await {
                log::error!("game against {} failed: {e}", hex::encode(&peer));
            }
            Ok(())
        })
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
// play_against_peer — dial and play a text-based chess game
// ---------------------------------------------------------------------------

async fn play_against_peer(
    dialer: &system_capnp::dialer::Client,
    peer_id: &[u8],
) -> Result<(), capnp::Error> {
    // Dial peer → get bidirectional ByteStream.
    let mut req = dialer.dial_request();
    req.get().set_peer(peer_id);
    req.get().set_protocol("chess");
    let resp = req.send().promise.await?;
    let stream = resp.get()?.get_stream()?;

    log::info!("connected to peer {}, starting game", hex::encode(peer_id));

    // Play game: send UCI moves, read responses via the single Stream capability.
    let engine = ChessEngineImpl::new();
    let mut move_num = 0u32;

    loop {
        // Pick a random move.
        let moves = engine.legal_moves_uci();
        if moves.is_empty() {
            break;
        }
        let our_move = &moves[rand::random_range(0..moves.len())];
        engine.apply(our_move).unwrap();
        move_num += 1;

        // Send move via stream.write().
        let mut wreq = stream.write_request();
        wreq.get().set_data(format!("{our_move}\n").as_bytes());
        wreq.send().promise.await?;

        // Check if game over after our move (no legal moves for opponent).
        // We can't know this for sure locally since we don't track opponent's
        // perspective, but we can check if the position has no legal moves.
        if engine.legal_moves_uci().is_empty() {
            log::info!("game over after our move {move_num}. {our_move}");
            break;
        }

        // Read response via stream.read().
        let mut rreq = stream.read_request();
        rreq.get().set_max_bytes(4096);
        let rresp = rreq.send().promise.await?;
        let data = rresp.get()?.get_data()?;
        if data.is_empty() {
            log::info!("stream closed after {move_num} moves");
            break;
        }
        let response = std::str::from_utf8(data)
            .map_err(|e| capnp::Error::failed(format!("invalid UTF-8: {e}")))?
            .trim()
            .to_string();
        if response.is_empty() {
            log::info!("empty response after {move_num} moves");
            break;
        }

        engine
            .apply(&response)
            .map_err(|e| capnp::Error::failed(format!("invalid response move: {e}")))?;
        log::info!("{move_num}. {our_move} → {response}");
    }

    log::info!("game complete ({move_num} moves)");

    // Close the stream.
    let _ = stream.close_request().send().promise.await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Game loop (main mode)
// ---------------------------------------------------------------------------

async fn run_game(membrane: Membrane) -> Result<(), capnp::Error> {
    // Graft membrane → get capabilities.
    let graft_resp = membrane.graft_request().send().promise.await?;
    let results = graft_resp.get()?;
    let host = results.get_host()?;
    let executor = results.get_executor()?;
    let ipfs = results.get_ipfs()?;
    let routing = results.get_routing()?;

    // Pipeline: host.network() → (listener, dialer).
    let network_resp = host.network_request().send().promise.await?;
    let network = network_resp.get()?;
    let listener = network.get_listener()?;
    let dialer = network.get_dialer()?;

    // Read our own binary so handler instances run the same code in handler mode.
    let wasm_bytes = std::fs::read("/bin/main.wasm")
        .map_err(|e| capnp::Error::failed(format!("read handler wasm: {e}")))?;

    // Register /ww/0.1.0/chess listener.
    // OCAP: pass our executor to listener.listen(), explicitly delegating spawn rights.
    let mut listen_req = listener.listen_request();
    listen_req.get().set_executor(executor.clone());
    listen_req.get().set_protocol("chess");
    listen_req.get().set_handler(&wasm_bytes);
    listen_req.send().promise.await?;
    log::info!("registered /ww/0.1.0/chess listener");

    // Log peer identity.
    let id_resp = host.id_request().send().promise.await?;
    let self_id = id_resp.get()?.get_peer_id()?.to_vec();
    log::info!("peer id: {}", hex::encode(&self_id));

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

    // Discover other chess peers; dial them as they're found.
    let sink: routing_capnp::provider_sink::Client =
        capnp_rpc::new_client(DialingSink { dialer, self_id });
    let mut fp_req = routing.find_providers_request();
    {
        let mut b = fp_req.get();
        b.set_key(&ns_cid);
        b.set_count(5);
        b.set_sink(sink);
    }
    fp_req.send().promise.await?;
    log::info!("searching for chess peers...");

    // Block until stdin closes (host signals shutdown).
    let stdin = wasip2::cli::stdin::get_stdin();
    loop {
        match stdin.blocking_read(4096) {
            Ok(b) if b.is_empty() => break,
            Err(_) => break,
            Ok(_) => {}
        }
    }

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
            // Handler mode: text-based chess engine over stdin/stdout.
            handle_chess_stream();
        } else {
            // Main mode: register listener, discover peers, dial, play game.
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
            engine
                .apply(uci)
                .unwrap_or_else(|e| panic!("move {uci} failed: {e}"));
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
                    assert!(resp.get().unwrap().get_ok(), "move {uci} rejected over RPC");
                }

                let resp = client.get_status_request().send().promise.await.unwrap();
                let status = resp.get().unwrap().get_status().unwrap();
                assert_eq!(status, GameStatus::Checkmate);
            })
            .await;
    }
}
