TARGET_WASM := target/read_from_ipfs.wasm

all: build ipfs

build:
	mkdir -p target
	rustc src/main.rs -o $(TARGET_WASM) --target wasm32-wasi

hash:
	@ipfs add --only-hash --quieter $(TARGET_WASM)

publish:
	@ipfs add $(TARGET_WASM)  # TODO replace with ipfs add -r, as in the Go implementation

# TODO look for something similar to go generate in Rust
