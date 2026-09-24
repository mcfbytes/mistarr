TARGET := armv7-unknown-linux-musleabihf
BIN := target/$(TARGET)/release/mistarr
DIST := dist/mistarr-armv7.tar.gz
E2E_LOGS ?= $(CURDIR)/target/e2e-logs
# Falls back to the pip ziglang wheel's bundled zig when none is on PATH.
ZIGDIR := $(shell python3 -c "import ziglang, os; print(os.path.dirname(ziglang.__file__))" 2>/dev/null)

.PHONY: check e2e web cross release clean

check:
	cargo fmt --all --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test --workspace
	cd web && npm ci && npm run check && npm run lint && npm run build && npm run size
	sh scripts/principles-gate.sh
	sh scripts/tests/run.sh

# docs/TESTING.md layer 2, against whichever of transmission-daemon and rtorrent is installed.
e2e:
	mkdir -p $(E2E_LOGS)
	MISTARR_E2E_LOGS=$(E2E_LOGS) cargo test -p mistarr-server --test e2e -- --nocapture --test-threads=1

web:
	cd web && npm ci && npm run build

# Prepends ZIGDIR only when it resolved to something, so an empty value
# never puts "." first in PATH.
cross: web
	PATH="$(if $(ZIGDIR),$(ZIGDIR):,)$$PATH" cargo zigbuild --release --target $(TARGET) -p mistarr-server
	file $(BIN) | grep -q "statically linked"

# Ships the server binary and its script only; mistarr-fixture stays a dev tool.
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
