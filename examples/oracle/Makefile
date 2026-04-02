# Oracle — gas price feed with schema-keyed RPC + Terminal auth
#
# Usage: make -C examples/oracle

WASM_TARGET := wasm32-wasip2
ROOT := ../..

.PHONY: all schema-inject clean test demo

all: schema-inject
	cargo build --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin
	cp target/$(WASM_TARGET)/release/oracle.wasm bin/oracle.wasm
	@# Inject cell.capnp custom section into the WASM binary.
	@ORACLE_OUT=$$(find target/$(WASM_TARGET)/release/build -path '*/oracle-*/out/oracle_schema.bin' | head -1) && \
		if [ -n "$$ORACLE_OUT" ]; then \
			cargo run --manifest-path $(ROOT)/Cargo.toml \
				--bin schema-inject -p schema-id --features inject -- \
				bin/oracle.wasm --capnp "$$ORACLE_OUT"; \
		else \
			echo "WARNING: oracle_schema.bin not found; skipping schema injection"; \
		fi

schema-inject:
	cargo build --manifest-path $(ROOT)/Cargo.toml \
		--bin schema-inject -p schema-id --features inject

test:
	cargo test --manifest-path Cargo.toml

demo: all
	@echo ""
	@echo "=== Price Oracle Demo ==="
	@echo ""
	@echo "The oracle binary is at examples/oracle/bin/oracle.wasm"
	@echo ""
	@echo "Run the oracle service (terminal 1):"
	@echo "  cargo run -- run --http-listen 127.0.0.1:8080 examples/oracle/bin/oracle.wasm"
	@echo ""
	@echo "Run a consumer (terminal 2):"
	@echo "  WW_CONSUMER=1 cargo run -- run examples/oracle/bin/oracle.wasm"
	@echo ""

clean:
	rm -f bin/oracle.wasm
	cargo clean --manifest-path Cargo.toml
