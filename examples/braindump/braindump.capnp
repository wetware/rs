# Braindump — symmetric peer-to-peer context sharing for LLMs.
#
# Both peers export a Braindump capability to each other. Each side
# can push context (as content-addressed CIDs) and prompt the other's
# local LLM. The capability IS the access control.
#
# "You have a Braindump or you don't."

@0xb7d3e1f2a4c6089a;

struct Metadata {
  contentType @0 :Text;       # MIME type hint
  tags        @1 :List(Text); # freeform tags
  relation    @2 :Text;       # future: supports/contradicts/refines/supersedes
  supersedes  @3 :Data;       # future: CID of content this replaces
}

interface Braindump {
  # Get a write surface for pushing context into this peer's store.
  context @0 () -> (writer :ContextWriter);
  # Get a rate-limited prompt capability against this peer's LLM.
  prompt  @1 () -> (prompt :Prompt);
}

interface ContextWriter {
  # Push content by CID. Receiver resolves via local content store.
  # inlineContent: raw bytes for small payloads; empty means resolve
  # cid from content store.
  push @0 (cid :Data, meta :Metadata, inlineContent :Data) -> ();
}

interface Prompt {
  # Prompt the local LLM with context from the export dir.
  ask @0 (text :Text) -> (response :Text, error :Text);
  # Check remaining rate limit.
  remaining @1 () -> (calls :UInt32, resetIn :UInt64);
}
