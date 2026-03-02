# Replay Log

Every completed chess game publishes a content-addressed replay to IPFS.
The root CID is logged at the end of each game and can be used to
reconstruct the full move history.

## Data structure

The replay is a **linked list of JSON nodes**, one per move pair.
Each node references the previous node's CID via a `prev` field,
forming a backward-traversable chain:

```
 move 1          move 2              move N (terminal)
┌──────────┐    ┌──────────┐        ┌──────────────────┐
│ n: 1     │◄───│ n: 2     │◄─ … ◄─│ n: N             │
│ w: e2e4  │    │ w: d2d4  │        │ w: c7c4          │
│ b: e7e5  │    │ b: d7d5  │        │ result: 1-0      │
│ prev: ø  │    │ prev: …1 │        │ prev: …(N-1)     │
└──────────┘    └──────────┘        └──────────────────┘
                                     ▲
                                     └── root CID (logged)
```

## Node schema

| Field    | Type          | Description                          |
|----------|---------------|--------------------------------------|
| `n`      | integer       | Move number (1-indexed)              |
| `w`      | string        | White's move (UCI notation)          |
| `b`      | string        | Black's response (UCI notation)      |
| `prev`   | string / null | CID of the previous node, or `null`  |
| `result` | string        | Terminal only: `"1-0"`, `"0-1"`, `"*"` |

Normal nodes have `n`, `w`, `b`, `prev`.  The terminal node adds
`result` and may omit `b` (e.g. when White's move ends the game).

### Result codes

- `1-0` — White wins (opponent has no legal moves after White's move)
- `0-1` — Black wins (White has no legal moves at start of turn)
- `*` — game interrupted (stream closed or empty response)

## Viewing a replay

```sh
# Start from the root CID logged at the end of the game.
$ ipfs cat bafkreig...
{"n":36,"w":"c7c4","result":"1-0","prev":"bafkreif..."}

# Follow the prev link to see the previous move.
$ ipfs cat bafkreif...
{"n":35,"w":"a5a6","b":"h7h6","prev":"bafkreie..."}

# Keep following prev links back to the first move.
$ ipfs cat bafkreie...
{"n":34,"w":"b3b4","b":"g8f6","prev":"bafkreid..."}
# ...
$ ipfs cat bafkrei0...
{"n":1,"w":"e2e4","b":"e7e5","prev":null}
```

## Design rationale

Each node is published to IPFS via `unixfs.add()` immediately after the
move pair is played. This means:

- **Turn-by-turn publishing**: each move is content-addressed before the
  next is played; if the process crashes mid-game, all completed moves
  are already persisted.
- **Immutable history**: once a CID is published, that node can never
  change. The linked list is append-only by construction.
- **Minimal capability surface**: only requires `unixfs.add(data) → cid`,
  which is already available to every Wetware guest. No DAG or Block API
  needed.
- **User-friendly**: `ipfs cat <cid>` returns readable JSON. No special
  tooling required to inspect a game.

When the DAG API is implemented, this structure can migrate to DAG-CBOR
for smaller nodes and native IPLD traversal. The linked list topology
remains the same.
