# Wetware

Rust implementation of Wetware.

---

## Run Wetware

> TODO: document IPFS dependency

Run a WASM module directly from IPFS with, e.g. the one in `test/wasm/read_from_ipfs`:

```sh
cargo run -- --load /ipfs/QmaGNHr18jf5RiWMYme63XJh5k4TuPSMiGbvAZueQEC8rH
```

The example WASM module will read `QmaGNHr18jf5RiWMYme63XJh5k4TuPSMiGbvAZueQEC8rH` from IPFS and assert its contents.
