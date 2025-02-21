TARGET_WASM := target/wasm32-wasip1/release/echo.wasm

all: clean build ipfs

build:
	mkdir -p target
	cargo build --target "wasm32-wasip1" --release

clean:
	rm -rf target

hash:
	@ipfs add --only-hash --quieter $(TARGET_WASM)

ipfs:
	@ipfs add $(TARGET_WASM)
