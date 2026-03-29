# Chess engine capability.
#
# One instance = one game.  Spawn a new guest to start a new game.
#
# PR 3 (MCP bridge):   calls getLegalMoves + applyMove
# PR 5 (P2P session):  wraps applyMove / getStatus in submitMove / waitForTurn

@0xe3c2dfb1868218d1;

interface ChessEngine {
  getState      @0 () -> (fen :Text);
  applyMove     @1 (uci :Text) -> (ok :Bool, reason :Text);
  getLegalMoves @2 () -> (moves :List(Text));
  getStatus     @3 () -> (status :GameStatus);

  enum GameStatus {
    ongoing   @0;
    checkmate @1;
    stalemate @2;
    draw      @3;
  }
}
