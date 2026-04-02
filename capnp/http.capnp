# Outbound HTTP capability for WASM guests.
#
# Domain-scoped: the host-side proxy checks the URL host against an
# allowlist before making the request. Epoch-guarded.

@0xc8e3b0a1d2f4e567;

struct Header {
  name @0 :Text;
  value @1 :Text;
}

interface HttpClient {
  get @0 (url :Text, headers :List(Header)) -> (status :UInt16, headers :List(Header), body :Data);
  # Fetch a URL. Domain scoping enforced host-side.
}
