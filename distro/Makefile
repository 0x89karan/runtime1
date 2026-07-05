BR_VERSION  := 2024.02.9
BR_URL      := https://buildroot.org/downloads/buildroot-$(BR_VERSION).tar.gz
BR_TARBALL  := build/buildroot-$(BR_VERSION).tar.gz
BR_DIR      := build/buildroot-$(BR_VERSION)
NCPUS       := $(shell nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)

# ── Architecture selection ────────────────────────────────────────────────────
# Usage: make [target] ARCH=aarch64   (default: x86_64)
ARCH ?= x86_64
$(if $(filter-out x86_64 aarch64,$(ARCH)),$(error ARCH must be 'x86_64' or 'aarch64' — got '$(ARCH)'),)

ifeq ($(ARCH),aarch64)
  BR_CONFIG    := $(CURDIR)/buildroot.aarch64.config
  KERNEL_IMAGE := Image
  QEMU         := qemu-system-aarch64
  QEMU_MACHINE := -M virt
  CONSOLE      := ttyAMA0
  MUSL_TARGET  := aarch64-unknown-linux-musl
  OUTPUT_DIR   := output/aarch64
else
  BR_CONFIG    := $(CURDIR)/buildroot.config
  KERNEL_IMAGE := bzImage
  QEMU         := qemu-system-x86_64
  QEMU_MACHINE :=
  CONSOLE      := ttyS0
  MUSL_TARGET  := x86_64-unknown-linux-musl
  OUTPUT_DIR   := output
endif

# ── Acceleration (aarch64 only) ───────────────────────────────────────────────
UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)
ifeq ($(ARCH),aarch64)
  ifeq ($(UNAME_S),Darwin)
    ifeq ($(UNAME_M),arm64)
      # Apple Silicon: use macOS Hypervisor.framework via HVF (arm64 host only)
      QEMU_ACCEL := -accel hvf
      QEMU_CPU   := -cpu host
    else
      # Intel Mac: HVF does not support aarch64 guests; fall back to TCG
      QEMU_ACCEL := -accel tcg
      QEMU_CPU   := -cpu cortex-a72
    endif
  else
    # Linux: use KVM if /dev/kvm is accessible; fall back to TCG with compatible CPU
    ifeq ($(shell test -w /dev/kvm && echo kvm || echo tcg),kvm)
      QEMU_ACCEL := -accel kvm
      QEMU_CPU   := -cpu host
    else
      QEMU_ACCEL := -accel tcg
      # -cpu host rejects with TCG; use a known-good aarch64 model
      QEMU_CPU   := -cpu cortex-a72
    endif
  endif
else
  QEMU_ACCEL :=
  QEMU_CPU   :=
endif

QEMU_FLAGS  := \
	-nographic \
	-m 256M \
	$(QEMU_MACHINE) \
	$(QEMU_CPU) \
	$(QEMU_ACCEL) \
	-kernel $(OUTPUT_DIR)/$(KERNEL_IMAGE) \
	-initrd $(OUTPUT_DIR)/rootfs.cpio.gz \
	-append "console=$(CONSOLE) quiet ip=dhcp" \
	-netdev user,id=net0 \
	-device virtio-net-pci,netdev=net0 \
	-virtfs local,path=$(HOME)/.agentos-secrets,mount_tag=secrets0,security_model=none,id=secrets0 \
	-virtfs local,path=$(HOME)/.agentos-memory,mount_tag=memory0,security_model=none,id=memory0

.PHONY: all prereqs build run test clean distclean

all: build

prereqs:
	@command -v $(QEMU) >/dev/null 2>&1 || { \
		echo "ERROR: $(QEMU) not found."; \
		echo "  macOS:  brew install qemu"; \
		echo "  Ubuntu: apt-get install qemu-system-x86 qemu-system-arm"; \
		exit 1; \
	}
	@[ -f $(HOME)/.agentos-secrets/agentos.env ] || { \
		echo "ERROR: $(HOME)/.agentos-secrets/agentos.env missing."; \
		echo "  mkdir -p $(HOME)/.agentos-secrets"; \
		echo "  printf 'ANTHROPIC_API_KEY=sk-ant-...\n' > $(HOME)/.agentos-secrets/agentos.env"; \
		exit 1; \
	}
	@grep -q ANTHROPIC_API_KEY $(HOME)/.agentos-secrets/agentos.env || { \
		echo "ERROR: ANTHROPIC_API_KEY not set in $(HOME)/.agentos-secrets/agentos.env"; \
		exit 1; \
	}
	@mkdir -p $(HOME)/.agentos-memory
	@echo "prereqs OK."

$(BR_TARBALL):
	mkdir -p build
	curl -L --progress-bar -o $@ $(BR_URL)

$(BR_DIR): $(BR_TARBALL)
	tar xzf $< -C build/

# Real-file prerequisites ensure Make re-copies when the source binary changes
# (e.g. after cross-compiling for a different ARCH without running `make clean`).
../target/$(MUSL_TARGET)/release/agentd:
	@echo "ERROR: musl binary not found. Run from repo root:"
	@echo "  cross build --target $(MUSL_TARGET) --release -p agentd -p agentctl"
	@false

../target/$(MUSL_TARGET)/release/agentctl:
	@echo "ERROR: musl binary not found. Run from repo root:"
	@echo "  cross build --target $(MUSL_TARGET) --release -p agentd -p agentctl"
	@false

overlay/usr/bin/agentd: ../target/$(MUSL_TARGET)/release/agentd
	mkdir -p overlay/usr/bin
	cp $< $@
	chmod +x $@

overlay/usr/bin/agentctl: ../target/$(MUSL_TARGET)/release/agentctl
	mkdir -p overlay/usr/bin
	cp $< $@
	chmod +x $@

overlay/etc/agentd/templates/.gitkeep:
	mkdir -p overlay/etc/agentd/templates
	cp ../templates/*.template.toml overlay/etc/agentd/templates/
	touch $@

$(OUTPUT_DIR)/$(KERNEL_IMAGE): $(BR_DIR) overlay/usr/bin/agentd overlay/usr/bin/agentctl overlay/etc/agentd/templates/.gitkeep
	mkdir -p build/output-$(ARCH) $(OUTPUT_DIR)
	chmod +x overlay/init
	cd $(BR_DIR) && make \
		BR2_EXTERNAL=$(CURDIR) \
		O=$(CURDIR)/build/output-$(ARCH) \
		BR2_DEFCONFIG=$(BR_CONFIG) \
		defconfig
	cd $(BR_DIR) && make \
		BR2_EXTERNAL=$(CURDIR) \
		O=$(CURDIR)/build/output-$(ARCH) \
		-j$(NCPUS)
	cp build/output-$(ARCH)/images/$(KERNEL_IMAGE) $(OUTPUT_DIR)/$(KERNEL_IMAGE)
	cp build/output-$(ARCH)/images/rootfs.cpio.gz $(OUTPUT_DIR)/rootfs.cpio.gz
	@echo "Build complete: $(OUTPUT_DIR)/$(KERNEL_IMAGE) + $(OUTPUT_DIR)/rootfs.cpio.gz"

build: $(OUTPUT_DIR)/$(KERNEL_IMAGE)

run: prereqs build
	mkdir -p $(CURDIR)/$(OUTPUT_DIR)/run
	$(QEMU) $(QEMU_FLAGS) \
		-virtfs local,path=$(CURDIR)/$(OUTPUT_DIR)/run,mount_tag=output0,security_model=none,id=output0

test: prereqs build
	mkdir -p $(CURDIR)/$(OUTPUT_DIR)/test-run $(CURDIR)/$(OUTPUT_DIR)/test-memory
	@rm -f $(CURDIR)/$(OUTPUT_DIR)/test-run/flight.jsonl
	@echo "Booting AgentOS for acceptance test (timeout 120s)..."
	@timeout 120 $(QEMU) $(QEMU_FLAGS) \
		-no-reboot \
		-virtfs local,path=$(CURDIR)/$(OUTPUT_DIR)/test-run,mount_tag=output0,security_model=none,id=output0 \
		-virtfs local,path=$(CURDIR)/$(OUTPUT_DIR)/test-memory,mount_tag=memory0,security_model=none,id=memory0 \
		-serial file:$(CURDIR)/$(OUTPUT_DIR)/console.log \
		2>/dev/null || true
	@echo "Checking flight log..."
	@jq -e 'select(.kind == "agent_completed" or .kind == "budget_exceeded")' \
		$(CURDIR)/$(OUTPUT_DIR)/test-run/flight.jsonl 2>/dev/null | head -1 > /dev/null \
		&& echo "PASS: agentd ran to completion." \
		|| { \
			echo "FAIL: no completion event in flight.jsonl."; \
			echo "--- console.log ---"; \
			cat $(CURDIR)/$(OUTPUT_DIR)/console.log 2>/dev/null || echo "(empty)"; \
			exit 1; \
		}

clean:
	rm -rf $(OUTPUT_DIR) overlay/usr/bin/agentd overlay/usr/bin/agentctl overlay/etc/agentd/templates/.gitkeep

distclean: clean
	rm -rf build/
