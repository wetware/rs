# Echo cell — minimal WASI stdin/stdout echo
#
# Usage: make -C examples/echo

WASM_TARGET := wasm32-wasip2

.PHONY: all clean

all:
	cargo build -p echo --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin
	cp target/$(WASM_TARGET)/release/echo.wasm bin/echo.wasm

clean:
	rm -f bin/echo.wasm
	cargo clean --manifest-path Cargo.toml
