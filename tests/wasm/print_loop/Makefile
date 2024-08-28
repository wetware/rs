TARGET_WASM := target/print_loop.wasm

all: build ipfs

build:
	mkdir -p target
	rustc src/main.rs -o target/print_loop.wasm --target wasm32-wasi

hash:
	@ipfs add --only-hash --quieter $(TARGET_WASM)

ipfs:
	@ipfs add $(TARGET_WASM)
