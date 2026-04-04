# Braindump — symmetric p2p context sharing for LLMs
#
# Usage: make -C examples/braindump

WASM_TARGET := wasm32-wasip2

.PHONY: all clean

all:
	cargo build --target $(WASM_TARGET) --release \
		--manifest-path Cargo.toml
	@mkdir -p bin
	cp target/$(WASM_TARGET)/release/braindump.wasm bin/braindump.wasm
	@# Copy compiled schema bytes next to the WASM binary.
	@BRAINDUMP_OUT=$$(find target/$(WASM_TARGET)/release/build -path '*/braindump-*/out/braindump_schema.bin' | head -1) && \
		if [ -n "$$BRAINDUMP_OUT" ]; then \
			cp "$$BRAINDUMP_OUT" bin/braindump.schema; \
		else \
			echo "WARNING: braindump_schema.bin not found"; \
		fi

clean:
	rm -f bin/braindump.wasm bin/braindump.schema
	cargo clean --manifest-path Cargo.toml
