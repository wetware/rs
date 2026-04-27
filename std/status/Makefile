# Status cell — minimal WAGI status endpoint.
#
# Usage: make -C std/status

CARGO ?= cargo
WASM_TARGET := wasm32-wasip2

.PHONY: all clean

all:
	$(CARGO) build -p status --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin
	cp target/$(WASM_TARGET)/release/status.wasm bin/status.wasm

clean:
	rm -f bin/status.wasm
	$(CARGO) clean --manifest-path Cargo.toml
