BR_VERSION  := 2024.02.9
BR_URL      := https://buildroot.org/downloads/buildroot-$(BR_VERSION).tar.gz
BR_TARBALL  := build/buildroot-$(BR_VERSION).tar.gz
BR_DIR      := build/buildroot-$(BR_VERSION)
OUTPUT_DIR  := output
NCPUS       := $(shell nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)

QEMU        := qemu-system-x86_64
QEMU_FLAGS  := \
	-nographic \
	-m 256M \
	-kernel $(OUTPUT_DIR)/bzImage \
	-initrd $(OUTPUT_DIR)/rootfs.cpio.gz \
	-append "console=ttyS0 quiet ip=dhcp" \
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
		echo "  Ubuntu: apt-get install qemu-system-x86"; \
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

overlay/usr/bin/agentd:
	@[ -f ../agentd/target/x86_64-unknown-linux-musl/release/agentd ] || { \
		echo "ERROR: musl binary not found. Build it first:"; \
		echo "  cd ../agentd && cross build --target x86_64-unknown-linux-musl --release"; \
		exit 1; \
	}
	mkdir -p overlay/usr/bin
	cp ../agentd/target/x86_64-unknown-linux-musl/release/agentd $@
	chmod +x $@

$(OUTPUT_DIR)/bzImage: $(BR_DIR) overlay/usr/bin/agentd
	mkdir -p build/output $(OUTPUT_DIR)
	chmod +x overlay/init
	cd $(BR_DIR) && make \
		BR2_EXTERNAL=$(CURDIR) \
		O=$(CURDIR)/build/output \
		BR2_DEFCONFIG=$(CURDIR)/buildroot.config \
		defconfig
	cd $(BR_DIR) && make \
		BR2_EXTERNAL=$(CURDIR) \
		O=$(CURDIR)/build/output \
		-j$(NCPUS)
	cp build/output/images/bzImage $(OUTPUT_DIR)/bzImage
	cp build/output/images/rootfs.cpio.gz $(OUTPUT_DIR)/rootfs.cpio.gz
	@echo "Build complete: $(OUTPUT_DIR)/bzImage + $(OUTPUT_DIR)/rootfs.cpio.gz"

build: $(OUTPUT_DIR)/bzImage

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
	rm -rf $(OUTPUT_DIR) overlay/usr/bin/agentd

distclean: clean
	rm -rf build/
