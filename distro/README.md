# AgentOS distro — Buildroot minimal rootfs

Builds a minimal Linux VM (x86_64 musl + BusyBox) that boots straight to
`agentd`. The kernel + rootfs fit in ~50 MB; the whole VM image pair is
`output/bzImage` + `output/rootfs.cpio.gz`.

## Prerequisites

**Host packages:**

| macOS | Debian/Ubuntu |
|-------|---------------|
| `brew install qemu` | `apt-get install qemu-system-x86 jq` |
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
cd ../agentd
cross build --target x86_64-unknown-linux-musl --release
# binary: target/x86_64-unknown-linux-musl/release/agentd
```

`make build` will copy it into `overlay/usr/bin/agentd` automatically.

## Usage

All targets must be run from this directory (`distro/`).

```sh
# Check that QEMU and secrets are present
make prereqs

# Build (downloads Buildroot ~80 MB, compiles ~30-60 min on first run; subsequent ~2 min)
make build

# Boot interactively (Ctrl-A X to exit QEMU)
make run

# Automated acceptance test (boots, waits for agent_completed in flight.jsonl, exits)
make test

# Clean built artifacts (keeps Buildroot download cache)
make clean

# Full clean including Buildroot source tree
make distclean
```

## Boot sequence

```
QEMU
  -kernel output/bzImage
  -initrd output/rootfs.cpio.gz
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
# Flight events
jq . output/run/flight.jsonl

# Demo greeting written by the agent
cat output/run/greeting.txt

# Console log (test mode only)
cat output/console.log
```

## Directory layout

```
distro/
  Makefile              build / run / test / prereqs / clean
  buildroot.config      Buildroot defconfig (x86_64 musl, busybox, cpio.gz)
  kernel-extras.config  kernel fragment: virtio-net + virtio-9p
  overlay/
    init                /init PID-1 sh script
    usr/bin/agentd      (gitignored; copied by `make build`)
    etc/
      resolv.conf       nameserver 10.0.2.3 (QEMU SLIRP DNS)
      agentd/
        agent.toml      demo agent config (haiku, native tools only)
  build/                Buildroot sources + build tree (gitignored)
  output/               QEMU artifacts: bzImage, rootfs.cpio.gz (gitignored)
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
