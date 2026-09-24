TARGET := armv7-unknown-linux-musleabihf
BIN := target/$(TARGET)/release/mistarr
DIST := dist/mistarr-armv7.tar.gz
# Falls back to the pip ziglang wheel's bundled zig when none is on PATH.
ZIGDIR := $(shell python3 -c "import ziglang, os; print(os.path.dirname(ziglang.__file__))" 2>/dev/null)

.PHONY: check web cross release clean

check:
	cargo fmt --all --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test --workspace
	cd web && npm ci && npm run check && npm run lint && npm run build && npm run size
	sh scripts/principles-gate.sh

web:
	cd web && npm ci && npm run build

cross: web
	PATH="$(ZIGDIR):$$PATH" cargo zigbuild --release --target $(TARGET) -p mistarr-server
	file $(BIN) | grep -q "statically linked"

release: cross
	mkdir -p dist
	rm -rf dist/stage
	mkdir -p dist/stage
	cp $(BIN) dist/stage/mistarr
	cp scripts/mistarr.sh dist/stage/mistarr.sh
	tar -C dist/stage -czf $(DIST) mistarr mistarr.sh
	rm -rf dist/stage

clean:
	cargo clean
	rm -rf dist web/dist
