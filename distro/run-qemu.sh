#!/bin/sh
# run-qemu.sh — launch AgentOS in QEMU for interactive use.
# The Makefile's `run` and `test` targets call this indirectly; this script
# exists for manual invocation and documentation.
#
# Prerequisites: make build && make prereqs
# Usage: sh run-qemu.sh [--test]
#
# --test: add -no-reboot; QEMU exits when agentd finishes (for CI).

set -e
cd "$(dirname "$0")"

QEMU=qemu-system-x86_64
OUTPUT=output

COMMON_FLAGS="
  -nographic
  -m 256M
  -kernel ${OUTPUT}/bzImage
  -initrd ${OUTPUT}/rootfs.cpio.gz
  -append 'console=ttyS0 quiet ip=dhcp'
  -netdev user,id=net0
  -device virtio-net-pci,netdev=net0
  -virtfs local,path=${HOME}/.agentos-secrets,mount_tag=secrets0,security_model=none,id=secrets0
"

if [ "${1:-}" = "--test" ]; then
    mkdir -p "${OUTPUT}/test-run"
    # shellcheck disable=SC2086
    exec ${QEMU} ${COMMON_FLAGS} \
        -no-reboot \
        -virtfs "local,path=$(pwd)/${OUTPUT}/test-run,mount_tag=output0,security_model=none,id=output0" \
        -serial "file:$(pwd)/${OUTPUT}/console.log"
else
    mkdir -p "${OUTPUT}/run"
    # shellcheck disable=SC2086
    exec ${QEMU} ${COMMON_FLAGS} \
        -virtfs "local,path=$(pwd)/${OUTPUT}/run,mount_tag=output0,security_model=none,id=output0"
fi
