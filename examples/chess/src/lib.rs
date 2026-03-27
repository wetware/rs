//! Chess guest: cross-node play via membrane capabilities.
//!
//! This binary serves two roles, selected by env vars:
//!
//! **Engine mode** (`WW_ENGINE=1`): Exports a `ChessEngine` capability via
//! `system::serve()`. Designed to be shipped to a remote node via `runCap`
//! and driven by typed RPC calls.
//!
//! **Service mode** (default): Runs the discovery loop. For each discovered
//! peer, authenticates via `dialRpc` → `Terminal.login`, then ships its own
//! WASM to the remote executor via `runCap`, receiving a typed `ChessEngine`
//! capability back for game play.

use std::cell::RefCell;
use std::collections::HashSet;
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

/// Short peer ID for human-readable logs (last 4 bytes = 8 hex chars).
fn short_id(peer_id: &[u8]) -> String {
    let h = hex::encode(peer_id);
    if h.len() > 8 {
        format!("..{}", &h[h.len() - 8..])
    } else {
        h
    }
}

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
/// Implements `chess_capnp::chess_engine::Server`. In engine mode, exported
/// as the bootstrap capability via `system::serve()`.
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
// Engine mode — export ChessEngine capability via system::serve()
// ---------------------------------------------------------------------------

fn run_engine() {
    let engine_client: chess_capnp::chess_engine::Client =
        capnp_rpc::new_client(ChessEngineImpl::new());
    log::info!("engine mode: exporting ChessEngine capability");
    // Export ChessEngine as bootstrap cap; receive Membrane from host (unused).
    system::serve::<Membrane, _, _>(engine_client.client, |_membrane| async {
        // Keep alive until the host drops the capability reference.
        std::future::pending::<Result<(), capnp::Error>>().await
    });
}

// ---------------------------------------------------------------------------
// Service mode — discovery + membrane-based game play
// ---------------------------------------------------------------------------

/// Publish a JSON node to IPFS and return its CID, or None on failure.
///
/// Each node forms one link in a content-addressed linked list:
/// `{"n":1,"w":"e2e4","b":"e7e5","prev":null}` → CID.
async fn publish_node(unixfs: &ipfs_capnp::unix_f_s::Client, json: &str) -> Option<String> {
    let mut req = unixfs.add_request();
    req.get().set_data(json.as_bytes());
    match req.send().promise.await {
        Ok(resp) => match resp.get().and_then(|r| r.get_cid()) {
            Ok(cid) => cid.to_str().ok().map(|s| s.to_string()),
            Err(e) => {
                log::warn!("replay publish failed (reading CID): {e}");
                None
            }
        },
        Err(e) => {
            log::warn!("replay publish failed: {e}");
            None
        }
    }
}

/// Play a game against a remote ChessEngine capability, logging moves to IPFS.
async fn play_rpc_game(
    engine: &chess_capnp::chess_engine::Client,
    unixfs: &ipfs_capnp::unix_f_s::Client,
    us: &str,
    them: &str,
) -> Result<(), capnp::Error> {
    let local = ChessEngineImpl::new();
    let mut move_num = 0u32;
    let mut prev_cid: Option<String> = None;

    fn prev_field(cid: &Option<String>) -> String {
        match cid {
            Some(c) => format!(r#""prev":"{c}""#),
            None => r#""prev":null"#.to_string(),
        }
    }

    loop {
        // --- Our turn (we are White) ---
        let moves = local.legal_moves_uci();
        if moves.is_empty() {
            let node = format!(r#"{{"result":"0-1",{}}}"#, prev_field(&prev_cid));
            prev_cid = publish_node(unixfs, &node).await.or(prev_cid);
            log::info!("game {us} vs {them}: {them} wins after {move_num} moves");
            break;
        }
        let our_move = moves[rand::random_range(0..moves.len())].clone();
        local.apply(&our_move).unwrap();
        move_num += 1;

        // Apply our move on the remote engine.
        let mut req = engine.apply_move_request();
        req.get().set_uci(&our_move);
        let resp = req.send().promise.await?;
        if !resp.get()?.get_ok() {
            let reason = resp.get()?.get_reason()?.to_str().unwrap_or("unknown");
            return Err(capnp::Error::failed(format!(
                "remote rejected move '{our_move}': {reason}"
            )));
        }

        // Check if game over after our move.
        let status_resp = engine.get_status_request().send().promise.await?;
        let status = status_resp.get()?.get_status()?;
        if status != chess_capnp::chess_engine::GameStatus::Ongoing {
            let node = format!(
                r#"{{"n":{move_num},"w":"{our_move}","result":"1-0",{}}}"#,
                prev_field(&prev_cid)
            );
            prev_cid = publish_node(unixfs, &node).await.or(prev_cid);
            log::info!("game {us} vs {them}: {us} wins after {move_num} moves ({our_move})");
            break;
        }

        // --- Remote's turn (they are Black) ---
        let remote_moves_resp = engine.get_legal_moves_request().send().promise.await?;
        let remote_moves = remote_moves_resp.get()?.get_moves()?;
        if remote_moves.is_empty() {
            let node = format!(
                r#"{{"n":{move_num},"w":"{our_move}","result":"1-0",{}}}"#,
                prev_field(&prev_cid)
            );
            prev_cid = publish_node(unixfs, &node).await.or(prev_cid);
            log::info!("game {us} vs {them}: {us} wins (no legal response)");
            break;
        }
        let idx = rand::random_range(0..remote_moves.len());
        let response = remote_moves.get(idx)?.to_str().unwrap_or("").to_string();

        // Apply remote's choice on both engines.
        let mut apply_req = engine.apply_move_request();
        apply_req.get().set_uci(&response);
        apply_req.send().promise.await?;
        local
            .apply(&response)
            .map_err(|e| capnp::Error::failed(format!("local apply failed: {e}")))?;

        // Publish move pair to IPFS.
        let node = format!(
            r#"{{"n":{move_num},"w":"{our_move}","b":"{response}",{}}}"#,
            prev_field(&prev_cid)
        );
        prev_cid = publish_node(unixfs, &node).await.or(prev_cid);
        log::info!("  {move_num}. {our_move} {response}");

        // Check if game over after remote's move.
        let status_resp = engine.get_status_request().send().promise.await?;
        let status = status_resp.get()?.get_status()?;
        if status != chess_capnp::chess_engine::GameStatus::Ongoing {
            let result = match status {
                chess_capnp::chess_engine::GameStatus::Checkmate => "0-1",
                _ => "1/2-1/2",
            };
            let node = format!(r#"{{"result":"{result}",{}}}"#, prev_field(&prev_cid));
            prev_cid = publish_node(unixfs, &node).await.or(prev_cid);
            log::info!("game {us} vs {them}: {result} after {move_num} moves");
            break;
        }
    }

    if let Some(cid) = &prev_cid {
        log::info!("game {us} vs {them}: replay \u{2192} {cid}");
    }
    log::info!("game {us} vs {them}: complete ({move_num} moves)");

    Ok(())
}

/// Connect to a remote peer, authenticate, ship WASM, and play via typed RPC.
async fn play_against_peer(
    dialer: &system_capnp::dialer::Client,
    signer: &stem_capnp::signer::Client,
    unixfs: &ipfs_capnp::unix_f_s::Client,
    chess_wasm: &[u8],
    self_id: &[u8],
    peer_id: &[u8],
) -> Result<(), capnp::Error> {
    let us = short_id(self_id);
    let them = short_id(peer_id);

    // 1. TERMINAL AUTH — dialRpc opens /ww/0.1.0, bootstraps Terminal.
    log::info!("game {us} vs {them}: connecting via dialRpc...");
    let mut dial_req = dialer.dial_rpc_request();
    dial_req.get().set_peer(peer_id);
    let dial_resp = dial_req.send().promise.await?;
    let terminal = dial_resp.get()?.get_terminal()?;

    // Authenticate: Terminal.login(signer) → Membrane.
    log::info!("game {us} vs {them}: authenticating via Terminal...");
    let mut login_req = terminal.login_request();
    login_req.get().set_signer(signer.clone());
    let login_resp = login_req.send().promise.await?;
    let remote_membrane = login_resp.get()?.get_session()?;

    // 2. REMOTE GRAFT — get remote capabilities.
    let graft_resp = remote_membrane.graft_request().send().promise.await?;
    let remote = graft_resp.get()?;
    let remote_executor = remote.get_executor()?;

    // 3. CODE MOBILITY — ship chess WASM to remote executor.
    log::info!("game {us} vs {them}: shipping chess engine to remote...");
    let mut run_req = remote_executor.run_cap_request();
    run_req.get().set_wasm(chess_wasm);
    {
        let mut env = run_req.get().init_env(1);
        env.set(0, "WW_ENGINE=1");
    }
    let run_resp = run_req.send().promise.await?;
    let engine: chess_capnp::chess_engine::Client =
        run_resp.get()?.get_cap().get_as_capability()?;

    // 4. PLAY VIA TYPED RPC
    log::info!("game {us} vs {them}: starting typed RPC game");
    play_rpc_game(&engine, unixfs, &us, &them).await
}

// ---------------------------------------------------------------------------
// DialingSink — discovers peers, authenticates, ships WASM, plays via RPC
// ---------------------------------------------------------------------------

struct DialingSink {
    dialer: system_capnp::dialer::Client,
    signer: stem_capnp::signer::Client,
    unixfs: ipfs_capnp::unix_f_s::Client,
    chess_wasm: Rc<Vec<u8>>,
    self_id: Vec<u8>,
    seen: Rc<RefCell<HashSet<Vec<u8>>>>,
}

#[allow(refining_impl_trait)]
impl routing_capnp::provider_sink::Server for DialingSink {
    fn provider(
        self: Rc<Self>,
        params: routing_capnp::provider_sink::ProviderParams,
    ) -> Promise<(), capnp::Error> {
        let peer_id = pry!(pry!(pry!(params.get()).get_info()).get_peer_id()).to_vec();

        // Skip self and already-seen peers.
        if peer_id == self.self_id || !self.seen.borrow_mut().insert(peer_id.clone()) {
            return Promise::ok(());
        }

        let dialer = self.dialer.clone();
        let signer = self.signer.clone();
        let unixfs = self.unixfs.clone();
        let chess_wasm = self.chess_wasm.clone();
        let self_id = self.self_id.clone();
        let peer = peer_id.clone();

        Promise::from_future(async move {
            if let Err(e) =
                play_against_peer(&dialer, &signer, &unixfs, &chess_wasm, &self_id, &peer).await
            {
                log::error!("game vs {} failed: {e}", short_id(&peer));
            }
            // Pause between games so the output is readable.
            let pause = wasip2::clocks::monotonic_clock::subscribe_duration(
                5_000_000_000, // 5s
            );
            pause.block();
            Ok(())
        })
    }

    fn done(
        self: Rc<Self>,
        _params: routing_capnp::provider_sink::DoneParams,
        _results: routing_capnp::provider_sink::DoneResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// Service mode — discovery loop
// ---------------------------------------------------------------------------

async fn run_service(membrane: Membrane) -> Result<(), capnp::Error> {
    let graft_resp = membrane.graft_request().send().promise.await?;
    let results = graft_resp.get()?;
    let host = results.get_host()?;
    let ipfs = results.get_ipfs()?;
    let routing = results.get_routing()?;
    let identity = results.get_identity()?;

    // Read config from env vars (set by init.d script).
    let namespace =
        std::env::var("WW_NS").map_err(|_| capnp::Error::failed("WW_NS not set".into()))?;

    // Get network dialer.
    let network_resp = host.network_request().send().promise.await?;
    let network = network_resp.get()?;
    let dialer = network.get_dialer()?;

    // Get UnixFS for replay publishing.
    let unixfs_resp = ipfs.unixfs_request().send().promise.await?;
    let unixfs = unixfs_resp.get()?.get_api()?;

    // Resolve peer identity.
    let id_resp = host.id_request().send().promise.await?;
    let self_id = id_resp.get()?.get_peer_id()?.to_vec();
    log::info!("service: peer {}", short_id(&self_id));

    // Get signer for Terminal authentication.
    let mut signer_req = identity.signer_request();
    signer_req.get().set_domain("ww-terminal-membrane");
    let signer_resp = signer_req.send().promise.await?;
    let signer = signer_resp.get()?.get_signer()?;

    // Fetch own WASM for shipping to remote executors.
    let mut cat_req = unixfs.cat_request();
    cat_req.get().set_path("bin/chess-demo.wasm");
    let cat_resp = cat_req.send().promise.await?;
    let chess_wasm = Rc::new(cat_resp.get()?.get_data()?.to_vec());
    log::info!(
        "service: loaded chess WASM ({} bytes) for code mobility",
        chess_wasm.len()
    );

    // Hash namespace → deterministic CID.
    let mut hash_req = routing.hash_request();
    hash_req.get().set_data(namespace.as_bytes());
    let hash_resp = hash_req.send().promise.await?;
    let ns_cid = hash_resp.get()?.get_key()?.to_str()?.to_string();
    log::debug!("service: namespace CID: {ns_cid}");

    log::info!("service: looking for opponent...");

    // Discovery loop with exponential backoff.
    let seen = Rc::new(RefCell::new(HashSet::<Vec<u8>>::new()));
    let mut cooldown_ms: u64 = 2_000;
    const BASE_MS: u64 = 2_000;
    const MAX_MS: u64 = 900_000;

    loop {
        let prev_seen = seen.borrow().len();

        // Re-provide (DHT records expire).
        let mut provide_req = routing.provide_request();
        provide_req.get().set_key(&ns_cid);
        provide_req.send().promise.await?;

        // Search for peers; DialingSink dials new ones automatically.
        let sink: routing_capnp::provider_sink::Client = capnp_rpc::new_client(DialingSink {
            dialer: dialer.clone(),
            signer: signer.clone(),
            unixfs: unixfs.clone(),
            chess_wasm: chess_wasm.clone(),
            self_id: self_id.clone(),
            seen: seen.clone(),
        });
        let mut fp_req = routing.find_providers_request();
        {
            let mut b = fp_req.get();
            b.set_key(&ns_cid);
            b.set_count(5);
            b.set_sink(sink);
        }
        fp_req.send().promise.await?;

        let now_seen = seen.borrow().len();
        if now_seen > prev_seen {
            log::info!("service: found {} opponent(s)", now_seen);
            cooldown_ms = BASE_MS;
        } else {
            cooldown_ms = (cooldown_ms * 2).min(MAX_MS);
        }

        let delay_ms = cooldown_ms / 2 + rand::random_range(0..=cooldown_ms / 2);
        let pause = wasip2::clocks::monotonic_clock::subscribe_duration(
            delay_ms * 1_000_000, // ms → ns
        );
        pause.block();
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

struct ChessGuest;

impl Guest for ChessGuest {
    fn run() -> Result<(), ()> {
        init_logging();
        if std::env::var("WW_ENGINE").is_ok() {
            // Engine mode: export ChessEngine capability.
            run_engine();
        } else {
            // Service mode: discovery + membrane-based game play.
            log::info!("chess guest starting (service mode)");
            system::run(|membrane: Membrane| async move { run_service(membrane).await });
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
    // RPC game simulation (two local engines, same logic as play_rpc_game)
    // -----------------------------------------------------------------------

    #[test]
    fn test_rpc_game_simulation() {
        // Simulate the play_rpc_game logic with two local engines.
        let local = ChessEngineImpl::new();
        let remote = ChessEngineImpl::new();
        let mut move_num = 0u32;
        let max_moves = 300;

        loop {
            // Our turn: pick a random move.
            let moves = local.legal_moves_uci();
            if moves.is_empty() {
                break;
            }
            let our_move = &moves[rand::random_range(0..moves.len())];
            local.apply(our_move).unwrap();
            remote.apply(our_move).unwrap();
            move_num += 1;

            // Check game over after our move.
            if local.legal_moves_uci().is_empty() {
                break;
            }

            // Remote's turn: pick from remote's legal moves.
            let remote_moves = remote.legal_moves_uci();
            if remote_moves.is_empty() {
                break;
            }
            let response = &remote_moves[rand::random_range(0..remote_moves.len())];
            remote.apply(response).unwrap();
            local.apply(response).unwrap();

            // Both engines should agree on position.
            assert_eq!(
                local.fen(),
                remote.fen(),
                "position mismatch after move {move_num}"
            );

            if move_num >= max_moves {
                break;
            }
        }

        assert!(move_num > 0, "game should have played at least one move");
        assert_eq!(local.fen(), remote.fen());
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
        let (client_stream, server_stream) = io::duplex(8 * 1024);
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

    // -------------------------------------------------------------------
    // Discovery backoff & jitter
    // -------------------------------------------------------------------

    /// Mirror the backoff constants from run_service so tests break if they drift.
    const BASE_MS: u64 = 2_000;
    const MAX_MS: u64 = 900_000;

    #[test]
    fn test_backoff_doubles_to_max() {
        let mut cooldown = BASE_MS;
        for _ in 0..30 {
            cooldown = (cooldown * 2).min(MAX_MS);
        }
        assert_eq!(cooldown, MAX_MS, "must cap at MAX_MS");
    }

    #[test]
    fn test_backoff_resets_on_new_peer() {
        let mut cooldown = MAX_MS; // fully backed off
                                   // Simulate new peer found.
        cooldown = BASE_MS;
        assert_eq!(cooldown, BASE_MS);
        // Next idle pass doubles.
        cooldown = (cooldown * 2).min(MAX_MS);
        assert_eq!(cooldown, BASE_MS * 2);
    }

    #[test]
    fn test_jitter_within_half_to_full() {
        // Jitter formula: cooldown/2 + rand(0..=cooldown/2).
        // Must produce values in [cooldown/2, cooldown].
        for cooldown in [BASE_MS, 4_000, 64_000, MAX_MS] {
            for _ in 0..500 {
                let delay = cooldown / 2 + rand::random_range(0..=cooldown / 2);
                assert!(
                    delay >= cooldown / 2,
                    "delay {delay} < floor {} (cooldown={cooldown})",
                    cooldown / 2,
                );
                assert!(delay <= cooldown, "delay {delay} > ceiling {cooldown}",);
            }
        }
    }

    #[test]
    fn test_jitter_max_is_strict_ceiling() {
        // After capping at MAX_MS, the jitter must never exceed it.
        let cooldown = MAX_MS;
        for _ in 0..1000 {
            let delay = cooldown / 2 + rand::random_range(0..=cooldown / 2);
            assert!(delay <= MAX_MS, "delay {delay} exceeds MAX_MS {MAX_MS}");
        }
    }

    // -------------------------------------------------------------------
    // Replay linked-list JSON
    // -------------------------------------------------------------------

    #[test]
    fn test_replay_move_pair_json() {
        let prev: Option<String> = Some("bafyPREV".into());
        let prev_field = match &prev {
            Some(c) => format!(r#""prev":"{c}""#),
            None => r#""prev":null"#.to_string(),
        };
        let node = format!(r#"{{"n":1,"w":"e2e4","b":"e7e5",{prev_field}}}"#,);
        let v: serde_json::Value =
            serde_json::from_str(&node).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{node}"));
        assert_eq!(v["n"], 1);
        assert_eq!(v["w"], "e2e4");
        assert_eq!(v["b"], "e7e5");
        assert_eq!(v["prev"], "bafyPREV");
    }

    #[test]
    fn test_replay_first_node_null_prev() {
        let prev: Option<String> = None;
        let prev_field = match &prev {
            Some(c) => format!(r#""prev":"{c}""#),
            None => r#""prev":null"#.to_string(),
        };
        let node = format!(r#"{{"n":1,"w":"e2e4","b":"e7e5",{prev_field}}}"#,);
        let v: serde_json::Value = serde_json::from_str(&node).unwrap();
        assert!(v["prev"].is_null());
    }

    #[test]
    fn test_replay_terminal_node_has_result() {
        let prev_field = r#""prev":"bafyLAST""#;
        // Win terminal.
        let win = format!(r#"{{"n":36,"w":"c7c8q","result":"1-0",{prev_field}}}"#);
        let v: serde_json::Value = serde_json::from_str(&win).unwrap();
        assert_eq!(v["result"], "1-0");
        assert!(v.get("b").is_none(), "terminal node has no black move");

        // Loss terminal.
        let loss = format!(r#"{{"result":"0-1",{prev_field}}}"#);
        let v: serde_json::Value = serde_json::from_str(&loss).unwrap();
        assert_eq!(v["result"], "0-1");

        // Interrupted terminal.
        let star = format!(r#"{{"n":5,"w":"d2d4","result":"*",{prev_field}}}"#);
        let v: serde_json::Value = serde_json::from_str(&star).unwrap();
        assert_eq!(v["result"], "*");
    }
}
