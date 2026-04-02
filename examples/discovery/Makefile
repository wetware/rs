# Discovery — Greeter RPC cell with schema-keyed peer discovery
#
# Usage: make -C examples/discovery

WASM_TARGET := wasm32-wasip2
ROOT := ../..

.PHONY: all clean

all:
	cargo build -p discovery --target $(WASM_TARGET) --release
	@mkdir -p bin boot
	cp $(ROOT)/target/$(WASM_TARGET)/release/discovery.wasm bin/discovery.wasm
	@# Copy compiled schema bytes to boot/main.schema.
	@GREETER_OUT=$$(find $(ROOT)/target/$(WASM_TARGET)/release/build -path '*/discovery-*/out/greeter_schema.bin' | head -1) && \
		if [ -n "$$GREETER_OUT" ]; then \
			cp "$$GREETER_OUT" boot/main.schema; \
			echo "boot/main.schema ($$(wc -c < "$$GREETER_OUT" | tr -d ' ') bytes)"; \
		else \
			echo "WARNING: greeter_schema.bin not found"; \
		fi

clean:
	rm -f bin/discovery.wasm boot/main.schema
