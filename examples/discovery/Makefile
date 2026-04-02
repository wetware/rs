# Discovery — Greeter RPC cell with schema-keyed peer discovery
#
# Usage: make -C examples/discovery

WASM_TARGET := wasm32-wasip2
ROOT := ../..

.PHONY: all clean

all:
	cargo build -p discovery --target $(WASM_TARGET) --release
	@mkdir -p bin
	cp $(ROOT)/target/$(WASM_TARGET)/release/discovery.wasm bin/discovery.wasm

clean:
	rm -f bin/discovery.wasm
