# Wetware build system
#
# Builds std/ components and places artifacts at <component>/boot/main.wasm.
# Publish to IPFS with: ww push std/

WASM_TARGET := wasm32-wasip2

.PHONY: all host std kernel shell examples chess echo counter discovery oracle auction clean run-kernel
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
	cargo build -p shell --target $(WASM_TARGET) --release --manifest-path std/shell/Cargo.toml
	@mkdir -p std/shell/bin
	cp std/shell/target/$(WASM_TARGET)/release/shell.wasm std/shell/bin/shell.wasm
	@SCHEMA_OUT=$$(find std/shell/target/$(WASM_TARGET)/release/build -path '*/shell-*/out/shell_schema.bin' | head -1) && \
		if [ -n "$$SCHEMA_OUT" ]; then \
			cp "$$SCHEMA_OUT" std/shell/bin/shell.schema; \
		fi

# --- Examples ----------------------------------------------------------------
# Note: auction.capnp lives in capnp/ but is compiled by the example crate
# that uses it (via build.rs), not by the host binary.

examples: chess echo counter discovery oracle auction

chess:
	$(MAKE) -C examples/chess

echo:
	$(MAKE) -C examples/echo

counter:
	$(MAKE) -C examples/counter

discovery:
	$(MAKE) -C examples/discovery

oracle:
	$(MAKE) -C examples/oracle

auction:
	$(MAKE) -C examples/auction

# --- Run ---------------------------------------------------------------------

run-kernel: kernel
	cargo run -- run crates/kernel

# --- Clean -------------------------------------------------------------------

clean:
	cargo clean
	rm -f crates/kernel/bin/main.wasm
	rm -f std/shell/bin/shell.wasm std/shell/bin/shell.schema
	$(MAKE) -C examples/chess clean
	$(MAKE) -C examples/echo clean
	$(MAKE) -C examples/counter clean
	$(MAKE) -C examples/discovery clean
	$(MAKE) -C examples/oracle clean
	$(MAKE) -C examples/auction clean

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
