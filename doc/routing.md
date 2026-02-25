# Routing

This document covers the DHT design: why wetware runs its own, how the
capability model works, and how the kernel bootstraps it.

For general architecture, see [architecture.md](architecture.md).
For transport plumbing, see [rpc-transport.md](rpc-transport.md).

## Why a private DHT

Wetware needs arbitrary key-value storage in the DHT — service discovery,
cluster state, content routing. Kubo's HTTP API only exposes provider
records and IPNS; there is no way to `putValue`/`getValue` for arbitrary
keys through it. We must run our own `kad::Behaviour`.

Once we're running native Kademlia anyway, the marginal cost of a private
overlay is near zero: one `set_protocol_names()` call. A private DHT gives
us sub-network isolation for free — different images specify different
protocol IDs, forming independent routing tables. Two organizations running
wetware don't share a DHT.

## Capability model

The DHT follows the same authority model as everything else in wetware:
the host provides mechanism, the kernel applies policy. No raw Kademlia
methods are exposed to guests.

```
Host                              Kernel (pid0)
────                              ──────
Membrane.graft() ──────────────> receives RoutingFactory capability
                                  reads /etc/routing.d/ config
                                  calls factory.create(config)
                                    → gets dormant Routing handle
                                  gathers bootstrap peers
                                  starts services
                                  calls routing.bootstrap()
                                    → DHT goes live
```

**RoutingFactory** is the authority gate. The host controls who gets it
(via Membrane graft). The kernel controls what DHT to join (via image
config). The factory returns a dormant handle — the DHT doesn't go live
until the kernel explicitly calls `bootstrap()`.

**Routing** is the operational interface, modeled after Go's
`routing.Routing`:

- `bootstrap()` — explicit go-live
- `provide(key)` / `findProviders(key, count)` — content routing
- `findPeer(peerId)` — peer routing
- `putValue(key, value)` / `getValue(key)` — value store

Intentionally minimal. No routing table introspection, no query-level
knobs. If a use case demands it later, extend the interface then.

## Configuration

The kernel reads DHT configuration from the image filesystem:

```
<image>/
  boot/
    <peerID_base58>         # static bootstrap peers (one file per peer,
                            # contents: newline-delimited multiaddrs)
  etc/
    routing.d/
      config.glia           # DHT parameters
      bootstrap             # additional multiaddr-format peers (optional)
```

### config.glia

```clojure
(routing
  (protocol-id "/ww/kad/1.0.0")
  (mode auto))              ; auto | client | server
```

The default mode is `auto` — starts as client, promotes to server when
publicly reachable (mirrors Kademlia's automatic mode).

The `protocol-id` determines which DHT namespace this node joins.
Different protocol IDs form isolated sub-networks. This is the primary
mechanism for network partitioning.

### Bootstrap peers

Bootstrap peers can come from multiple sources:

- `/boot/<peerID>` files in the image (static, known at image build time)
- `/etc/routing.d/bootstrap` file (multiaddrs, for peers whose ID is
  embedded in the address)
- Discovered at runtime via mDNS, IPFS, or other services

The kernel gathers peers from all available sources before calling
`routing.bootstrap()`.

## 1:1:1 model

One node, one protocol ID, one DHT. No multi-DHT per node.

DHTs mesh in practice — nodes in the same Kademlia routing table discover
each other transitively through lookups. Multiple DHT instances on one
node would mean overlapping routing tables, wasting memory and bandwidth
for little gain. Sub-networks are achieved by deploying images with
different protocol IDs, not by running multiple DHTs on the same node.

## Epoch lifecycle

The `Routing` handle is epoch-scoped, like all capabilities. On epoch
advance:

1. Old handle goes stale (EpochGuard)
2. Kernel re-grafts on the Membrane, gets fresh capabilities
3. Creates a new DHT via RoutingFactory, bootstraps it
4. Resumes normal operation

The kernel process is long-lived — it loops over the RPC socket and
re-grafts on each epoch change. Process death is reserved for hard
failures and graceful shutdown, not routine epoch transitions.

### Warm restart

The host outlives epochs. Before tearing down the old `kad::Behaviour`,
it snapshots known peers. When the kernel creates a new DHT via the
factory, the host seeds it with cached peers. The kernel doesn't need
to know about this optimization — it just calls `factory.create()` then
`routing.bootstrap()` and gets a fast warm start.

This is "chaos by design": the DHT must rebuild cheaply. Every investment
in fast bootstrap pays off everywhere — node restarts, network partitions,
crash recovery, epoch transitions. It's general resilience, not
epoch-specific hardening.

## Ambient authority

The protocol ID is ambient — anyone with a RoutingFactory capability can
specify any protocol ID. This is the same model as pubsub topics. The
meaningful authority boundary is "can you get a RoutingFactory at all"
(controlled via Membrane graft), not "which protocol string do you pass."

If protocol ID restriction is ever needed, attenuate the factory. But
this is a future concern, not a day-one one.

## Future: PeX

A persistent peerstore (host-side, across epochs) is sufficient for warm
restarts day-one. PeX (gossip-based peer exchange for uniformly random
peer sampling) solves different problems:

- Long-offline recovery (stale peers are dead)
- Eclipse attack resistance (biased routing tables)
- Protocol-level random peer selection

PeX would compose as a service (`/svc/pex/`) consuming the Routing
interface — not a prerequisite for it. Defer until eclipse resistance
is the concrete trigger.

## See also

- [architecture.md](architecture.md) — overall design and capability flow
- [shell.md](shell.md) — kernel shell reference
- [rpc-transport.md](rpc-transport.md) — transport plumbing
- [../capnp/system.capnp](../capnp/system.capnp) — Host, Executor interfaces
- GitHub milestone: Identity-Based Routing
