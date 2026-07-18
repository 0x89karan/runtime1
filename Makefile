# Workspace-level convenience targets.
# Crate-level build/test/clippy: run from agentd/ (see CLAUDE.md).

RUST_IMAGE ?= rust:latest

# Build the full Docker image locally — native arm64 on Apple Silicon, no QEMU.
# Second run is fast (~2 min) when deps haven't changed (cargo registry cache hit).
# After building: AGENTOS_IMAGE=agentos:dev docker compose up cos
.PHONY: dev-image
dev-image:
	DOCKER_BUILDKIT=1 docker build --target runtime-full -t agentos:dev .

# Build the Rust-only core image — faster, for agentd/agentctl-only changes.
# Note: the cos and agent compose services need the full image (Python MCP harness).
.PHONY: dev-image-core
dev-image-core:
	DOCKER_BUILDKIT=1 docker build --target runtime-core -t agentos:dev-core .

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
	(cd sandbox && cross clippy --all-targets --target aarch64-unknown-linux-musl -- -D warnings)

# Proves docker-compose.yml's cos/agent services are unaffected by the
# Dockerfile's default CMD (ux.9 flipped it shell -> cockpit): both services
# set an explicit `command:` that overrides the image CMD regardless of what
# it is. This does not exercise the built image's actual CMD — it only
# guards the compose YAML itself against someone later removing an explicit
# command: line and unknowingly inheriting cockpit mode in cos/agent.
.PHONY: compose-config-check
compose-config-check:
	@for svc in cos agent; do \
	  if docker compose config 2>/dev/null | awk -v s="  $$svc:" '$$0==s{f=1; print; next} f && /^  [a-zA-Z]/{f=0} f{print}' | grep -A1 "command:" | grep -q "^[[:space:]]*- $$svc$$"; then \
	    echo "OK: $$svc service still sets command: $$svc"; \
	  else \
	    echo "FAIL: $$svc service is missing its explicit command: $$svc"; exit 1; \
	  fi; \
	done

# Run self-tests for ALL bundled MCP servers (no API key required).
# Mirrors CI's sidecar-tests job (ci.1): globs docker/*_mcp.py and requires
# rc==0 AND the "self-test PASSED" stderr marker — marker alone passes a
# sidecar that prints PASSED then crashes; rc alone false-passes a flagless
# server EOFing on /dev/null stdin. Uses timeout/gtimeout when available
# (stock macOS has neither — `brew install coreutils` for gtimeout).
.PHONY: test-harness
test-harness:
	@fail=0; TMOUT_CMD=""; \
	command -v timeout >/dev/null 2>&1 && TMOUT_CMD="timeout 60"; \
	[ -z "$$TMOUT_CMD" ] && command -v gtimeout >/dev/null 2>&1 && TMOUT_CMD="gtimeout 60"; \
	for f in docker/*_mcp.py; do \
	  extra_env=""; case "$$f" in *semantic_kb*) extra_env="MOCK_EMBEDDINGS=1" ;; esac; \
	  rc=0; out=$$($$TMOUT_CMD env $$extra_env python3 "$$f" --test </dev/null 2>&1) || rc=$$?; \
	  if [ $$rc -eq 0 ] && printf '%s' "$$out" | grep -q "self-test PASSED"; then \
	    echo "PASS: $$f"; \
	  else \
	    echo "FAIL: $$f (rc=$$rc)"; printf '%s\n' "$$out" | tail -5; fail=1; \
	  fi; \
	done; exit $$fail
