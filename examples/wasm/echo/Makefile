ARCH := wasm32-wasip1
TARGET_WASM := target/$(ARCH)/release/echo.wasm

all: clean build ipfs

build:
	mkdir -p target
	cargo build --target $(ARCH) --release

clean:
	rm -rf target

hash:
	@ipfs add --only-hash --quieter $(TARGET_WASM)

ipfs:
	@ipfs add $(TARGET_WASM)
