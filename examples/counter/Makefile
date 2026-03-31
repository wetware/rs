# Counter cell — HTTP/WAGI demo
#
# Usage: make -C examples/counter

WASM_TARGET := wasm32-wasip2
ROOT := ../..

.PHONY: all schema-inject clean

all: schema-inject
	cargo build -p counter --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin
	cp target/$(WASM_TARGET)/release/counter.wasm bin/counter.wasm
	cargo run --manifest-path $(ROOT)/Cargo.toml \
		--bin schema-inject -p schema-id --features inject -- \
		bin/counter.wasm --http /counter

schema-inject:
	cargo build --manifest-path $(ROOT)/Cargo.toml \
		--bin schema-inject -p schema-id --features inject

clean:
	rm -f bin/counter.wasm
	cargo clean --manifest-path Cargo.toml
