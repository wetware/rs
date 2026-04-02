# PriceOracle capability — gas price feed for the oracle demo.
#
# A persistent capability that serves live gas price data.
# Protected by Terminal(PriceOracle) auth gate.

@0xd4e7a2b1c3f5e890;

interface PriceOracle {
  getPrice @0 (pair :Text) -> (price :Int64, decimals :UInt8,
                                timestamp :Int64, confidence :Float64);
  # Get the latest price for a trading pair (e.g. "ETH/gas").
  # price is in the smallest unit (e.g. gwei), decimals indicates
  # how many decimal places to shift. confidence is 0.0..1.0
  # (decays toward 0.0 if data is stale).

  getPairs @1 () -> (pairs :List(Text));
  # List available trading pairs.
}
