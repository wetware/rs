# Wetware build system
#
# Builds std/ components and places artifacts at <component>/boot/main.wasm.
# Publish to IPFS with: ww push std/

WASM_TARGET := wasm32-wasip2

.PHONY: all host std kernel shell examples chess schema-inject clean run-kernel
.PHONY: container-build container-run container-dev container-clean

all: std examples host

# --- Host --------------------------------------------------------------------

host:
	cargo build --release

# --- Std components ----------------------------------------------------------

std: kernel shell

kernel:
	cargo build -p kernel --target $(WASM_TARGET) --release
	@mkdir -p crates/kernel/bin
	cp target/$(WASM_TARGET)/release/kernel.wasm crates/kernel/bin/main.wasm

shell:
	cargo build -p shell --target $(WASM_TARGET) --release
	@mkdir -p std/shell/boot
	cp target/$(WASM_TARGET)/release/shell.wasm std/shell/boot/main.wasm

# --- Examples ----------------------------------------------------------------

examples: chess

chess: schema-inject
	cargo build -p chess --target $(WASM_TARGET) --release
	@mkdir -p examples/chess/bin
	cp target/$(WASM_TARGET)/release/chess.wasm examples/chess/bin/chess-demo.wasm
	@# Inject cell.capnp custom section into the WASM binary.
	@# The schema bytes were written by build.rs during compilation.
	@CHESS_OUT=$$(find target/$(WASM_TARGET)/release/build -path '*/chess-*/out/chess_engine_schema.bin' | head -1) && \
		if [ -n "$$CHESS_OUT" ]; then \
			cargo run --bin schema-inject -p schema-id --features inject -- \
				examples/chess/bin/chess-demo.wasm --capnp "$$CHESS_OUT"; \
		else \
			echo "WARNING: chess_engine_schema.bin not found; skipping schema injection"; \
		fi

schema-inject:
	cargo build --bin schema-inject -p schema-id --features inject

# --- Run ---------------------------------------------------------------------

run-kernel: kernel
	cargo run -- run crates/kernel

# --- Clean -------------------------------------------------------------------

clean:
	cargo clean
	rm -f crates/kernel/bin/main.wasm
	rm -f std/shell/boot/main.wasm
	rm -f examples/chess/bin/chess-demo.wasm
	rm -f examples/chess/bin/main.wasm

# --- Container ---------------------------------------------------------------

CONTAINER_ENGINE ?= podman
CONTAINER_TAG    ?= wetware:latest

container-build:
	$(CONTAINER_ENGINE) build \
		--build-arg GIT_COMMIT=$$(git rev-parse --short HEAD) \
		-t $(CONTAINER_TAG) .

container-run:
	$(CONTAINER_ENGINE) run --rm -it -p 8080:8080 $(CONTAINER_TAG)

container-dev: container-build
	$(CONTAINER_ENGINE) run --rm -it \
		-v $(PWD)/config:/app/config:ro \
		-p 8080:8080 $(CONTAINER_TAG)

container-clean:
	$(CONTAINER_ENGINE) rmi $(CONTAINER_TAG) || true
