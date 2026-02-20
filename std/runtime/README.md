# wetware-guest — Guest Runtime

The SDK for WASM programs running inside the wetware host environment.

## What it is

When a WASM component is executed by `ww`, it runs inside a sandbox that communicates
with the host over a WASI stream pair. This crate abstracts that connection into a
Cap'n Proto RPC session, letting guest code call host capabilities using ordinary
`async/await`.

## Entry points

```rust
// Receive host capabilities; no export back to the host.
wetware_guest::run(|host: Membrane| async move {
    let session = host.graft_request().send().promise.await?;
    // ...
    Ok(())
});

// Receive host capabilities AND export `bootstrap` back to the host.
// Use this when the guest needs to surface a capability to external peers.
wetware_guest::serve(my_capability, |host: Membrane| async move {
    // ...
    Ok(())
});
```

`run()` is suitable for processes that consume capabilities but don't export any.
`serve()` is the pattern for the kernel and other processes that act as intermediaries —
they receive raw capabilities from the host, wrap or attenuate them, and hand the
wrapped version back so the host can expose it to external peers.

## Relationship to the kernel

The kernel (`std/kernel`) uses `serve()` to export an attenuated Membrane back to the
host. Other guest processes that only need to *use* capabilities (not re-export them)
use `run()`.
