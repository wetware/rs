# Chess engine — Cap'n Proto RPC cell with schema-keyed discovery
#
# Usage: make -C examples/chess

WASM_TARGET := wasm32-wasip2
ROOT := ../..

.PHONY: all clean

all:
	cargo build -p chess --target $(WASM_TARGET) --release
	@mkdir -p bin boot
	cp $(ROOT)/target/$(WASM_TARGET)/release/chess.wasm bin/chess-demo.wasm
	@# Copy compiled schema bytes to boot/main.schema.
	@CHESS_OUT=$$(find $(ROOT)/target/$(WASM_TARGET)/release/build -path '*/chess-*/out/chess_engine_schema.bin' | head -1) && \
		if [ -n "$$CHESS_OUT" ]; then \
			cp "$$CHESS_OUT" boot/main.schema; \
			echo "boot/main.schema ($$(wc -c < "$$CHESS_OUT" | tr -d ' ') bytes)"; \
		else \
			echo "WARNING: chess_engine_schema.bin not found"; \
		fi

clean:
	rm -f bin/chess-demo.wasm boot/main.schema
