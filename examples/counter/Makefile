# Counter cell — WAGI demo
#
# Usage: make -C examples/counter

WASM_TARGET := wasm32-wasip2

.PHONY: all clean

all:
	cargo build -p counter --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin
	cp target/$(WASM_TARGET)/release/counter.wasm bin/counter.wasm

clean:
	rm -f bin/counter.wasm
	cargo clean --manifest-path Cargo.toml
