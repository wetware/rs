TARGET_WASM := target/echo.wasm

all: build ipfs

build:
	mkdir -p target
	rustc src/main.rs -o $(TARGET_WASM) --target wasm32-wasip1

hash:
	@ipfs add --only-hash --quieter $(TARGET_WASM)

ipfs:
	@ipfs add $(TARGET_WASM)
