# Fuel auction protocol: RFQ (request-for-quote) compute marketplace.
#
# A ComputeProvider advertises capacity on the DHT, signs Quotes for
# prospective buyers, and spawns cells with budget-tracked fuel on
# acceptance.

@0xe1a4c7b2d3f6e589;

struct Quote {
  pricePerMFuel @0 :UInt64;   # price per million fuel units
  fuel          @1 :UInt64;   # total fuel granted
  expiresAt     @2 :Int64;    # UNIX timestamp (seconds)
  provider      @3 :Data;     # provider's peer ID
  wasmCid       @4 :Data;     # CID of the WASM binary to run
  nonce         @5 :UInt64;   # replay-prevention nonce
  signature     @6 :Data;     # Ed25519 signature over fields 0-5
}

struct AuctionStatus {
  totalCapacity @0 :UInt64;   # total fuel budget per epoch
  available     @1 :UInt64;   # fuel remaining (total - committed)
  activeTickets @2 :UInt32;   # count of active (running) cells
  utilization   @3 :Float64;  # committed / total (0.0..1.0)
}

interface ComputeProvider {
  quote @0 (wasmCid :Data, fuelRequested :UInt64) -> (quote :Quote);
  # Request a signed quote for running a WASM binary with the given
  # fuel budget.  The provider checks capacity, calculates price,
  # generates a nonce, signs the quote, and returns it.
  # Fails if fuelRequested exceeds available capacity.

  accept @1 (quote :Quote) -> (process :AnyPointer);
  # Accept a previously-issued quote.  The provider verifies the
  # signature, checks nonce freshness and expiry, deducts capacity,
  # loads the WASM binary, and spawns it with FuelPolicy::Oneshot.
  # Returns a handle to the running process (cast to system.Process).

  price @2 () -> (pricePerMFuel :UInt64);
  # Current price per million fuel units, based on utilization:
  #   base_price * (1 + committed / total_capacity)

  status @3 () -> (status :AuctionStatus);
  # Live auction state for metrics and monitoring.
}
