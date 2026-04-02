# Oracle — gas price feed with schema-keyed RPC + Terminal auth
#
# Usage: make -C examples/oracle

WASM_TARGET := wasm32-wasip2
ROOT := ../..

.PHONY: all schema-inject clean

all: schema-inject
	cargo build -p oracle --target $(WASM_TARGET) --release
	@mkdir -p bin
	cp $(ROOT)/target/$(WASM_TARGET)/release/oracle.wasm bin/oracle.wasm
	@# Inject cell.capnp custom section into the WASM binary.
	@ORACLE_OUT=$$(find $(ROOT)/target/$(WASM_TARGET)/release/build -path '*/oracle-*/out/oracle_schema.bin' | head -1) && \
		if [ -n "$$ORACLE_OUT" ]; then \
			cargo run --bin schema-inject -p schema-id --features inject -- \
				bin/oracle.wasm --capnp "$$ORACLE_OUT"; \
		else \
			echo "WARNING: oracle_schema.bin not found; skipping schema injection"; \
		fi

schema-inject:
	cargo build --bin schema-inject -p schema-id --features inject

clean:
	rm -f bin/oracle.wasm
