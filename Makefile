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
