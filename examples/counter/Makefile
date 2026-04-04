# Counter cell — WAGI demo
#
# Usage: make -C examples/counter

# Default to rustup cargo so `make -C examples/counter` works without shell profile
CARGO ?= $(HOME)/.cargo/bin/cargo

WASM_TARGET := wasm32-wasip2

.PHONY: all clean

all:
	$(CARGO) build -p counter --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin
	cp target/$(WASM_TARGET)/release/counter.wasm bin/counter.wasm

clean:
	rm -f bin/counter.wasm
	$(CARGO) clean --manifest-path Cargo.toml
