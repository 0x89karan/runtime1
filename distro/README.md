# AgentOS distro — Buildroot minimal rootfs

Builds a minimal Linux VM that boots straight to `agentd` as PID 1. Supports
two architectures:

| Arch | QEMU | Kernel | Acceleration |
|------|------|--------|--------------|
| x86_64 (default) | `qemu-system-x86_64` | `output/bzImage` | none (KVM opt-in) |
| aarch64 | `qemu-system-aarch64` | `output/aarch64/Image` | HVF on Apple Silicon; KVM or TCG on Linux |

## Prerequisites

**Host packages:**

| macOS | Debian/Ubuntu |
|-------|---------------|
| `brew install qemu` | `apt-get install qemu-system-x86 qemu-system-arm jq` |
| `brew install jq` | |

**Build tools** (only needed for `make build`, not `make run`/`make test`):

| macOS | Debian/Ubuntu |
|-------|---------------|
| Xcode CLT (`xcode-select --install`) | `apt-get install make gcc g++ patch cpio unzip bc rsync python3 wget` |

**Secrets file:**

```sh
mkdir -p ~/.agentos-secrets
printf 'ANTHROPIC_API_KEY=sk-ant-...\n' > ~/.agentos-secrets/agentos.env
chmod 600 ~/.agentos-secrets/agentos.env
```

The `/init` script sources this file and passes `ANTHROPIC_API_KEY` to agentd
through the environment. No key is ever written to disk inside the VM.

**agentd musl binary** (pre-built, not tracked in git):

```sh
# x86_64 (default)
cross build --target x86_64-unknown-linux-musl --release

# aarch64 (Apple Silicon / ARM)
cross build --target aarch64-unknown-linux-musl --release
```

`make build` (or `make build ARCH=aarch64`) copies the correct binary into
`overlay/usr/bin/agentd` automatically.

## Usage

All targets must be run from this directory (`distro/`).

```sh
# x86_64 (default)
make prereqs
make build
make run          # Ctrl-A X to exit QEMU
make test         # automated acceptance test
make clean

# aarch64 — Apple Silicon Mac with HVF acceleration
make prereqs ARCH=aarch64
make build ARCH=aarch64
make run ARCH=aarch64
make clean ARCH=aarch64

# Full clean including Buildroot source tree (all arches)
make distclean
```

## Apple Silicon quickstart

```sh
# 1. Install QEMU (includes qemu-system-aarch64)
brew install qemu

# 2. Build the aarch64 cross binary (from repo root)
cross build --target aarch64-unknown-linux-musl --release

# 3. Build the aarch64 distro (first run ~30-60 min; ccache makes reruns ~2 min)
cd distro
make build ARCH=aarch64

# 4. Boot with HVF acceleration
make run ARCH=aarch64
# Output lands in distro/output/aarch64/run/flight.jsonl
# Ctrl-A X to exit QEMU
```

The Makefile automatically detects macOS and passes `-accel hvf -cpu host` to
`qemu-system-aarch64`. On Linux, it uses KVM if `/dev/kvm` exists, or falls
back to TCG (`-cpu cortex-a72`).

> **Note:** `make build ARCH=aarch64` and `make build` (x86_64) use separate
> Buildroot output trees (`build/output-aarch64/` vs `build/output-x86_64/`)
> and separate output directories (`output/aarch64/` vs `output/`), so both
> arches can coexist on disk without interfering.

## Boot sequence

```
QEMU (x86_64: qemu-system-x86_64 / aarch64: qemu-system-aarch64 -M virt [-accel hvf])
  -kernel output/bzImage                   (x86_64)
         output/aarch64/Image              (aarch64)
  -initrd output/rootfs.cpio.gz            (x86_64)
          output/aarch64/rootfs.cpio.gz    (aarch64)
  -append "console=ttyS0 ..."             (x86_64)
          "console=ttyAMA0 ..."           (aarch64)
  -virtfs secrets0 ──► /run/secrets/  (read: ~/.agentos-secrets/)
  -virtfs output0  ──► /run/output/   (write: output/run/ or output/test-run/)
  -netdev user / -device virtio-net-pci  (NAT; DNS at 10.0.2.3)

/init (PID 1, busybox sh)
  mount proc/sysfs/devtmpfs
  mount 9p secrets0 → /run/secrets
  mount 9p output0  → /run/output
  source /run/secrets/agentos.env
  cd /run/output
  exec /usr/bin/agentd /etc/agentd/agent.toml

agentd (becomes PID 1 after exec)
  perceive → infer → act → observe
  writes flight.jsonl to /run/output (= host output/run/ or output/test-run/)
  exits 0

kernel panic → QEMU exits (with -no-reboot)
```

## Viewing output

After `make run` (Ctrl-A X) or `make test`:

```sh
# x86_64
jq . output/run/flight.jsonl
cat output/run/greeting.txt
cat output/console.log        # test mode only

# aarch64
jq . output/aarch64/run/flight.jsonl
cat output/aarch64/console.log
```

## Directory layout

```
distro/
  Makefile                      build / run / test / prereqs / clean  [ARCH=x86_64|aarch64]
  buildroot.config              Buildroot defconfig (x86_64)
  buildroot.aarch64.config      Buildroot defconfig (aarch64)
  kernel-extras.config          kernel fragment: virtio-net + virtio-9p (x86_64)
  kernel-extras.aarch64.config  kernel fragment: adds PL011 UART + VIRTIO_MMIO (aarch64)
  overlay/
    init                /init PID-1 sh script (arch-agnostic)
    usr/bin/agentd      (gitignored; copied by `make build`)
    etc/
      resolv.conf       nameserver 10.0.2.3 (QEMU SLIRP DNS)
      agentd/
        agent.toml      demo agent config (haiku, native tools only)
  build/
    output-x86_64/      Buildroot build tree (gitignored)
    output-aarch64/     Buildroot build tree, aarch64 (gitignored)
  output/               x86_64 QEMU artifacts: bzImage, rootfs.cpio.gz (gitignored)
  output/aarch64/       aarch64 QEMU artifacts: Image, rootfs.cpio.gz (gitignored)
```

## Troubleshooting

**`make test` prints `FAIL: no completion event`**
: Check `output/console.log` for boot errors. Common cause: `agentos.env` is
  missing or `ANTHROPIC_API_KEY` is invalid.

**9p mount fails inside VM**
: Ensure QEMU version ≥ 6. Check that `-virtfs` appears in the QEMU command
  (`run-qemu.sh --test` or `make -n test` to print without running).

**Network unreachable inside VM**
: `ip=dhcp` in the kernel append should configure the interface automatically.
  If the Anthropic API call fails, verify your key is valid on the host first.

**Buildroot build fails on macOS**
: Some Buildroot packages need Linux-specific tools. Use Docker:
  ```sh
  docker run --rm -v $(pwd):/work -w /work debian:bookworm \
    bash -c 'apt-get update -qq && apt-get install -y make gcc g++ patch cpio \
      unzip bc rsync python3 wget pkg-config && make build'
  ```
