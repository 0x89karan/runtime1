# Workspace-level convenience targets.
# Crate-level build/test/clippy: run from agentd/ (see CLAUDE.md).

RUST_IMAGE ?= rust:latest

# Run clippy on Linux inside Docker — required before pushing any
# #[cfg(target_os = "linux")] code (fuser, libc, etc.).
# Mirrors the CI working-directory: agentd step exactly.
.PHONY: clippy-linux
clippy-linux:
	docker run --rm \
	  -v "$(CURDIR)":/work \
	  -w /work/agentd \
	  $(RUST_IMAGE) \
	  sh -c "apt-get update -qq && apt-get install -y -qq libfuse-dev pkg-config && rustup component add clippy && cargo clippy --all-targets -- -D warnings"

# Same but also runs the full test suite on Linux.
.PHONY: test-linux
test-linux:
	docker run --rm \
	  -v "$(CURDIR)":/work \
	  -w /work/agentd \
	  $(RUST_IMAGE) \
	  sh -c "apt-get update -qq && apt-get install -y -qq libfuse-dev pkg-config && rustup component add clippy && cargo test"

# Run clippy for aarch64 via cross — required before pushing any code that
# changes #[cfg(target_arch = "x86_64")] or #[cfg(not(target_arch = "x86_64"))]
# behavior (e.g. sandbox DenySpawn gate). Requires Docker + `cross` installed.
# Cross.toml at the repo root pins the image version for `ring` compat.
.PHONY: clippy-aarch64
clippy-aarch64:
	@docker info >/dev/null 2>&1 || { echo "ERROR: Docker must be running (cross uses Docker+QEMU internally)"; exit 1; }
	@command -v cross >/dev/null 2>&1 || { echo "ERROR: cross not installed — run: cargo install cross --locked"; exit 1; }
	(cd agentd && cross clippy --all-targets --target aarch64-unknown-linux-musl -- -D warnings)
	(cd agentctl && cross clippy --all-targets --target aarch64-unknown-linux-musl -- -D warnings)

# Run self-tests for the bundled standard MCP servers (no API key required).
# Assumes python3 is on PATH.
.PHONY: test-harness
test-harness:
	python3 docker/shell_mcp.py --test
	python3 docker/http_mcp.py  --test
	python3 docker/search_mcp.py --test
	python3 docker/cron_mcp.py   --test
	python3 docker/fs_watch_mcp.py --test
	python3 docker/webhook_mcp.py  --test
