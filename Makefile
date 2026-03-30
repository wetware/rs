# Wetware build system
#
# Builds std/ components and places artifacts at <component>/boot/main.wasm.
# Publish to IPFS with: ww push std/

WASM_TARGET := wasm32-wasip2

.PHONY: all host std kernel shell examples chess echo counter schema-inject clean run-kernel
.PHONY: container-build container-run container-dev container-clean
.PHONY: ai-setup

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

examples: chess echo counter

chess:
	$(MAKE) -C examples/chess

echo:
	$(MAKE) -C examples/echo

counter:
	$(MAKE) -C examples/counter

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
	$(MAKE) -C examples/chess clean
	$(MAKE) -C examples/echo clean
	$(MAKE) -C examples/counter clean

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

# --- AI tool setup -----------------------------------------------------------
# Links .agents/prompt.md to where each AI tool expects its config.
# Run once after cloning.  Top-level symlinks are gitignored.

ai-setup:
	@echo "Which AI coding tools do you use?"
	@echo "  1) Claude Code"
	@echo "  2) Codex (OpenAI)"
	@echo "  3) Cursor"
	@echo "  4) GitHub Copilot"
	@echo ""
	@read -p "Enter numbers separated by spaces (e.g. 1 3): " choices; \
	for c in $$choices; do \
		case $$c in \
			1) ln -sf .agents/prompt.md CLAUDE.md && echo "  linked CLAUDE.md";; \
			2) ln -sf .agents/prompt.md AGENTS.md && echo "  linked AGENTS.md";; \
			3) ln -sf .agents/prompt.md .cursorrules && echo "  linked .cursorrules";; \
			4) mkdir -p .github && ln -sf ../.agents/prompt.md .github/copilot-instructions.md && echo "  linked .github/copilot-instructions.md";; \
			*) echo "  unknown option: $$c";; \
		esac; \
	done; \
	echo "Done."
