# Cross-Crate Cap'n Proto Schema Sharing

## Problem

capnpc generates Rust trait types (e.g. `Server`, `Client`) per crate. If two
crates both compile the same `.capnp` file, their generated traits are
**distinct types** — a struct implementing crate A's `Server` trait does not
satisfy crate B's `Server` trait, even though the schema is identical.

Concretely: `stem` compiles `stem.capnp` and exports `MembraneServer` which
implements `stem::stem_capnp::membrane::Server`. If `rs` also compiles
`stem.capnp`, it gets its own `rs::stem_capnp::membrane::Server` — and stem's
`MembraneServer` doesn't implement it.

## Solution: `crate_provides`

capnpc (≥ 0.17.2) has `CompilerCommand::crate_provides(crate_name, file_ids)`.
This tells the code generator: "don't generate code for these schema files;
instead, emit `use` statements that reference the named crate's generated
modules."

```rust
// rs/build.rs
capnpc::CompilerCommand::new()
    .file("capnp/peer.capnp")
    .file("capnp/membrane.capnp")   // imports stem.capnp
    .crate_provides("stem", [0x9bce094a026970c4]) // stem.capnp file ID
    .run()
    .expect("failed to compile capnp schemas");
```

The file ID is the `@0x...` annotation at the top of each `.capnp` file:

```capnp
# stem.capnp
@0x9bce094a026970c4;
```

## Requirements

1. **The `.capnp` file must still be on disk.** capnpc needs it for import
   resolution when other schemas (e.g. `membrane.capnp`) reference it. We
   vendor `stem.capnp` into `rs/capnp/` for this reason.

2. **The providing crate must expose its generated module.** stem does this via:
   ```rust
   pub mod stem_capnp {
       include!(concat!(env!("OUT_DIR"), "/capnp/stem_capnp.rs"));
   }
   ```

3. **The consuming crate depends on the provider normally** (via `Cargo.toml`).
   No special dependency features needed.

## How it works in this project

- **stem** compiles `stem.capnp` → generates `stem::stem_capnp` with
  `Membrane`, `Session`, `StatusPoller`, `Epoch`, etc.
- **rs** compiles `membrane.capnp` (which imports `stem.capnp`) but declares
  `crate_provides("stem", ...)` for `stem.capnp`. capnpc generates code for
  `membrane.capnp` only, with references like `stem::stem_capnp::membrane::*`.
- rs uses `stem::membrane::MembraneServer` directly — no trait mismatch because
  both sides share the same generated `stem::stem_capnp` types.

## What NOT to do

Don't compile the same `.capnp` in both crates without `crate_provides`. You'll
get a confusing `E0277` error like:

```
the trait `stem_capnp::membrane::Server<…>` is not implemented for `MembraneServer<…>`
note: `MembraneServer<…>` implements similarly named trait
      `stem::stem_capnp::membrane::Server`, but not `stem_capnp::membrane::Server<…>`
```

The fix is always `crate_provides` — not reimplementing the server locally.
