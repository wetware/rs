# Chess engine — Cap'n Proto RPC cell with schema-keyed discovery
#
# Usage: make -C examples/chess

WASM_TARGET := wasm32-wasip2
ROOT := ../..

.PHONY: all schema-inject clean

all: schema-inject
	cargo build -p chess --target $(WASM_TARGET) --release
	@mkdir -p bin
	cp $(ROOT)/target/$(WASM_TARGET)/release/chess.wasm bin/chess-demo.wasm
	@# Inject cell.capnp custom section into the WASM binary.
	@# The schema bytes were written by build.rs during compilation.
	@CHESS_OUT=$$(find $(ROOT)/target/$(WASM_TARGET)/release/build -path '*/chess-*/out/chess_engine_schema.bin' | head -1) && \
		if [ -n "$$CHESS_OUT" ]; then \
			cargo run --bin schema-inject -p schema-id --features inject -- \
				bin/chess-demo.wasm --capnp "$$CHESS_OUT"; \
		else \
			echo "WARNING: chess_engine_schema.bin not found; skipping schema injection"; \
		fi

schema-inject:
	cargo build --bin schema-inject -p schema-id --features inject

clean:
	rm -f bin/chess-demo.wasm
