# Wetware build system
#
# Builds std/ components and places artifacts at <component>/boot/main.wasm.
# Publish to IPFS with: ww push std/

WASM_TARGET := wasm32-wasip2

.PHONY: all host std kernel shell mcp examples chess echo counter discovery oracle auction mindshare clean run-kernel
.PHONY: container-build container-run container-dev container-clean

all: std examples host

# --- Host --------------------------------------------------------------------

host:
	cargo build --release

# --- Std components ----------------------------------------------------------

std: kernel shell mcp

kernel:
	cargo build -p kernel --target $(WASM_TARGET) --release --manifest-path std/kernel/Cargo.toml
	@mkdir -p std/kernel/bin
	cp std/kernel/target/$(WASM_TARGET)/release/kernel.wasm std/kernel/bin/main.wasm

shell:
	cargo build -p shell --target $(WASM_TARGET) --release --manifest-path std/shell/Cargo.toml
	@mkdir -p std/shell/bin
	cp std/shell/target/$(WASM_TARGET)/release/shell.wasm std/shell/bin/shell.wasm
	@SCHEMA_OUT=$$(find std/shell/target/$(WASM_TARGET)/release/build -path '*/shell-*/out/shell_schema.bin' | head -1) && \
		if [ -n "$$SCHEMA_OUT" ]; then \
			cp "$$SCHEMA_OUT" std/shell/bin/shell.schema; \
		fi

mcp:
	cargo build -p mcp --target $(WASM_TARGET) --release --manifest-path std/mcp/Cargo.toml
	@mkdir -p std/mcp/bin
	cp std/mcp/target/$(WASM_TARGET)/release/mcp.wasm std/mcp/bin/mcp.wasm
	cp std/mcp/target/$(WASM_TARGET)/release/mcp.wasm std/mcp/bin/main.wasm

# --- Examples ----------------------------------------------------------------
# Note: auction.capnp lives in capnp/ but is compiled by the example crate
# that uses it (via build.rs), not by the host binary.

examples: chess echo counter discovery oracle auction mindshare

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

mindshare:
	$(MAKE) -C examples/mindshare

# --- Run ---------------------------------------------------------------------

run-kernel: kernel
	cargo run -- run std/kernel

# --- Clean -------------------------------------------------------------------

clean:
	cargo clean
	rm -f std/kernel/bin/main.wasm
	rm -f std/shell/bin/shell.wasm std/shell/bin/shell.schema
	rm -f std/mcp/bin/mcp.wasm std/mcp/bin/main.wasm
	$(MAKE) -C examples/chess clean
	$(MAKE) -C examples/echo clean
	$(MAKE) -C examples/counter clean
	$(MAKE) -C examples/discovery clean
	$(MAKE) -C examples/oracle clean
	$(MAKE) -C examples/auction clean
	$(MAKE) -C examples/mindshare clean

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
