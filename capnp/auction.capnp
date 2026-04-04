# Fuel auction protocol for Wetware compute marketplace.
#
# RFQ model: consumers solicit signed Quotes from ComputeProviders,
# compare prices, and accept the best. Quotes are defunctionalized
# (plain structs, not live capabilities) so they flow through agent
# graphs as data.

@0xd7952b1b365fe19a;

using System = import "system.capnp";

struct Quote {
  pricePerMFuel @0 :UInt64;   # committed price per million fuel units
  fuel @1 :UInt64;            # fuel credits in this quote
  expiresAt @2 :Int64;        # unix timestamp, after which quote is void
  provider @3 :Data;          # provider's peer ID (for attribution)
  wasmCid @4 :Data;           # CID of the quoted payload (binds quote to code)
  nonce @5 :UInt64;           # one-use, prevents replay
  signature @6 :Data;         # provider's Ed25519 sig over fields 0-5
}

interface ComputeProvider {
  quote @0 (wasm :Data, fuelRequested :UInt64) -> (quote :Quote);
  # Get a signed quote. Returns a plain struct — pass it around,
  # compare it, hand it to another agent. No live reference needed.
  # Provider commits to the price for the TTL duration.
  # wasm: CID of the WASM binary to run.
  # fuelRequested: desired fuel budget.

  accept @1 (quote :Quote, args :List(Text), env :List(Text))
         -> (process :System.Process);
  # Redeem a signed quote. Provider verifies its own signature,
  # checks expiry and nonce. Starts computation at the quoted
  # price with a FuelPolicy::Oneshot budget. Cell runs until
  # fuel budget exhausted (Trap::OutOfFuel).
  # Returns the spawned Process capability.

  price @2 () -> (perMFuel :UInt64);
  # Spot rate for quick comparison. No commitment — just the
  # current price based on utilization. Use quote() to get a
  # binding commitment.

  status @3 () -> (capacity :UInt64, available :UInt64,
                   activeTickets :UInt32, utilization :Float64);
  # Live auction state for metrics scraping. Returns total capacity,
  # available (uncommitted) fuel, number of active tickets, and
  # utilization ratio (0.0..1.0).
}
