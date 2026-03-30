# Discovery — Greeter RPC cell with schema-keyed peer discovery
#
# Usage: make -C examples/discovery

WASM_TARGET := wasm32-wasip2
ROOT := ../..

.PHONY: all schema-inject clean

all: schema-inject
	cargo build -p discovery --target $(WASM_TARGET) --release
	@mkdir -p bin
	cp $(ROOT)/target/$(WASM_TARGET)/release/discovery.wasm bin/discovery.wasm
	@# Inject cell.capnp custom section into the WASM binary.
	@GREETER_OUT=$$(find $(ROOT)/target/$(WASM_TARGET)/release/build -path '*/discovery-*/out/greeter_schema.bin' | head -1) && \
		if [ -n "$$GREETER_OUT" ]; then \
			cargo run --bin schema-inject -p schema-id --features inject -- \
				bin/discovery.wasm --capnp "$$GREETER_OUT"; \
		else \
			echo "WARNING: greeter_schema.bin not found; skipping schema injection"; \
		fi

schema-inject:
	cargo build --bin schema-inject -p schema-id --features inject

clean:
	rm -f bin/discovery.wasm
