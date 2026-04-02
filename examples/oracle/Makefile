# Oracle — gas price feed with schema-keyed RPC + Terminal auth
#
# Usage: make -C examples/oracle

WASM_TARGET := wasm32-wasip2
ROOT := ../..

.PHONY: all clean test demo

all:
	cargo build --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin boot
	cp target/$(WASM_TARGET)/release/oracle.wasm bin/oracle.wasm
	@# Copy compiled schema bytes to boot/main.schema.
	@ORACLE_OUT=$$(find target/$(WASM_TARGET)/release/build -path '*/oracle-*/out/oracle_schema.bin' | head -1) && \
		if [ -n "$$ORACLE_OUT" ]; then \
			cp "$$ORACLE_OUT" boot/main.schema; \
			echo "boot/main.schema ($$(wc -c < "$$ORACLE_OUT" | tr -d ' ') bytes)"; \
		else \
			echo "WARNING: oracle_schema.bin not found"; \
		fi

test:
	cargo test --manifest-path Cargo.toml

demo: all
	@echo ""
	@echo "=== Price Oracle Demo ==="
	@echo ""
	@echo "The oracle binary is at examples/oracle/bin/oracle.wasm"
	@echo ""
	@echo "Run the oracle service (terminal 1):"
	@echo "  cargo run -- run --port 2025 crates/kernel examples/oracle"
	@echo ""
	@echo "Run a consumer (terminal 2):"
	@echo "  WW_CONSUMER=1 cargo run -- run --port 2026 crates/kernel examples/oracle"
	@echo ""

clean:
	rm -f bin/oracle.wasm boot/main.schema
	cargo clean --manifest-path Cargo.toml
