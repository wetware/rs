# MCP cell — JSON-RPC server for AI agent integration
#
# Usage: make -C std/mcp  (or `make mcp` from project root)

CARGO ?= $(HOME)/.cargo/bin/cargo

WASM_TARGET := wasm32-wasip2

.PHONY: all clean

all:
	$(CARGO) build -p mcp --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin
	cp target/$(WASM_TARGET)/release/mcp.wasm bin/mcp.wasm
	cp target/$(WASM_TARGET)/release/mcp.wasm bin/main.wasm

clean:
	rm -f bin/mcp.wasm
	$(CARGO) clean --manifest-path Cargo.toml
