@0x9bce094a026970c4;

struct Epoch {
  seq @0 :UInt64;        # Monotonic epoch sequence number (from Stem.seq).
  head @1 :Data;         # Opaque head bytes from the Stem contract.
  adoptedBlock @2 :UInt64;# Block number at which this epoch was adopted.
}

enum Status {
  ok @0;             # Operation succeeded under the current epoch.
  unauthorized @1;   # Caller not authorized under current policy.
  internalError @2;  # Unexpected internal failure.
}

interface Signer {
  sign @0 (domain :Text, nonce :UInt64) -> (sig :Data);
  # Sign an arbitrary nonce under a given domain string.
}

interface StatusPoller {
  pollStatus @0 () -> (status :Status);
}

struct Session(Extension) {
  issuedEpoch @0 :Epoch;
  # Epoch under which this session was minted.

  statusPoller @1 :StatusPoller;
  # Capability for polling session status. Can be withheld (client receives null capability).

  extension @2 :Extension;
  # Platform-specific capabilities scoped to this session.
}

interface Membrane(SessionExt) {
  graft @0 (signer :Signer) -> (session :Session(SessionExt));
  # Graft a signer to the membrane, establishing an epoch-scoped session.
}
