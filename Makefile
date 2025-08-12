.PHONY: clean examples podman-build podman-run podman-clean
WASM_EXAMPLES := examples/wasm

all: clean build examples

build:
	cargo build --release

clean:
	rm -rf target

examples:
	@cd $(WASM_EXAMPLES)/echo; make

# Podman targets
podman-build:
	podman build -t wetware:latest .

podman-run:
	podman run --rm -it wetware:latest

podman-clean:
	podman rmi wetware:latest || true
	podman system prune -f

# Development with Podman
podman-dev: podman-build
	podman run --rm -it -v $(PWD):/app wetware:latest
