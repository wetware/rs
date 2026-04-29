# Status endpoint

The `std/status/` cell ships in the install layer and serves
`GET /status` over WAGI. It's the engagement starter kit's
**60-second proof point**: install Wetware, run one curl, see a
WebAssembly cell with zero ambient authority answer with structured
data about your node.

## It works

```sh
$ curl -sSL https://wetware.run/install | sh
... ⚗️  Installing wetware...
... ✓ Default init.d (05-status.glia)
... ✓ Background daemon
...

$ curl http://localhost:2080/status
{
  "status":       "ok",
  "version":      "0.1.0",
  "peer_id":      "12D3KooWRLf8DAFsNfbv3s2DjRMbUuPc8AYdcBfokZbz6kJ2aUss",
  "listen_addrs": ["/ip4/127.0.0.1/tcp/2025", "/ip6/::1/tcp/2025", ...],
  "peer_count":   216
}
```

That's the entire interactive demo. Install, curl, JSON.

`status` and `version` are always populated. `peer_id`,
`listen_addrs`, and `peer_count` come from the `host` capability if
it's in the cell's graft. If the cap is withheld (an init.d author
attenuated the membrane), those fields degrade to `null` — the
endpoint still answers, it just reports less.

## What just happened

What you hit was a WebAssembly cell running inside the daemon, with
zero ambient authority. The cell can't read your filesystem. It
can't reach the network. It can't see your environment variables.
The only thing it can do is what the membrane handed it.

We gave it the `host` capability so it can report your peer ID and
your connected peers. We did not give it the network or the
filesystem or anything else. **The capability is the permission.**

### The wiring

Three files do the work:

1. **`std/status/src/lib.rs`** — the cell itself. ~80 LOC of Rust
   compiled to `wasm32-wasip2`. Detects WAGI mode (`REQUEST_METHOD`
   env var present), grafts the membrane, looks up the `host` cap by
   name, builds JSON, writes a CGI response to stdout. That's it.

2. **`std/status/etc/init.d/05-status.glia`** — the init script the
   kernel runs at boot. One line:
   ```
   (perform host :listen (cell (load "bin/status.wasm")) "/status")
   ```
   `host :listen` registers the cell on the WAGI HTTP listener at
   path `/status`. The `cell` form bundles the WASM bytes; `load`
   resolves `bin/status.wasm` through the loader chain (which finds
   the embedded blob in the host binary).

3. **`src/rpc/membrane.rs`** — the membrane graft. When the cell
   asks for capabilities (`membrane.graft()` from the guest side),
   the host returns a list of named caps. By default that includes
   `host`, `runtime`, `identity`, `routing`, and (if `--http-dial`
   is set) `http-client`. The status cell looks up just `host` and
   ignores the rest.

### Attenuation in code

The init.d author can narrow what the cell sees by wrapping the
listen call in a `with` block:

```
(with [(host (perform host :host))]
  (perform host :listen (cell (load "bin/status.wasm")) "/status"))
```

That registers the cell with **only** the `host` cap in scope —
not the default-membrane set. The cell's `graft()` returns just
that one named cap. WAGI cap propagation landed in #429 (`HttpListener.listen`
gained a `caps :List(Export)` parameter mirroring `VatListener`),
so this works for HTTP cells the same way it already worked for
vat cells.

The status cell handles withheld caps gracefully — fields that need
the missing cap come back as `null`. Nothing crashes; the response
just gets sparser.

### Verifying the security story

The capability isn't aspirational. To prove it, look at what the
cell tries that doesn't appear in `/status`:

- It doesn't read `/etc/passwd`. It can't — no filesystem cap was
  granted.
- It doesn't fetch `https://example.com`. It can't — no
  `http-client` cap was granted.
- It doesn't read your shell environment. It can't — WASI gives
  the guest only the env vars passed by the host (CGI vars in this
  case: `REQUEST_METHOD`, `PATH_INFO`, `QUERY_STRING`, etc.).

If you want to see this fail visibly, modify `std/status/src/lib.rs`
to call something it shouldn't have access to (e.g. open a file).
`cargo build --target wasm32-wasip2` succeeds. `curl /status`
returns 502 with the trap in the daemon log. The boundary is real,
not stylistic.

## Your LLM can do this too

`ww perform install` wires Wetware into Claude Code as an MCP server
automatically. The same capability surface curl just hit, an LLM
can reach via the MCP/Glia bridge — same caps, same membrane, same
attenuation guarantees.

Open Claude Code on the same machine. Ask:

> What's running on my Wetware node?

Claude can call into the node via MCP tools. Behind the scenes
it's evaluating Glia expressions against the live membrane:

```clojure
(perform host :id)         ; → "12D3Koo..."
(perform host :addrs)      ; → ["/ip4/127.0.0.1/tcp/2025", ...]
(perform host :peers)      ; → list of connected peers
(schema host)              ; → the canonical Schema.Node bytes for the Host interface
```

The LLM has exactly the capabilities the membrane handed it.
**No more, no less.** That's the AI-safety story: instead of
telling an LLM "please don't read this file," you give it a
capability set that doesn't include filesystem access, and the
question is moot. The system can't, not won't.

## Where this is going

The status endpoint is the first beat of a longer story:

- **WHOA 2 — `ww shell` discovers + invokes status** (future).
  Attach a shell to a running node, enumerate cells, call them
  through capnp from Glia. Same data, different transport.
- **WHOA 3 — IPNS hot-reload** (future). Update an IPNS UnixPath
  to add a second init.d script. Capabilities revoke, cells
  restart, new service appears live. The actual production
  operational loop, in-process.

For now: install, curl, done.
