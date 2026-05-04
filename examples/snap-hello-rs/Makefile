# snap-hello-rs cell — Farcaster Snap POC over WAGI
#
# Usage: make -C examples/snap-hello-rs

# Default to rustup cargo so `make -C examples/snap-hello-rs` works without shell profile
CARGO ?= cargo

WASM_TARGET := wasm32-wasip2

.PHONY: all clean

all:
	$(CARGO) build -p snap-hello-rs --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin
	cp target/$(WASM_TARGET)/release/snap_hello_rs.wasm bin/snap-hello-rs.wasm

clean:
	rm -f bin/snap-hello-rs.wasm
	$(CARGO) clean --manifest-path Cargo.toml
