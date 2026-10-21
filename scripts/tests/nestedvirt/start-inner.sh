#!/bin/sh
# Copyright 2026 AgentENV contributors.
# SPDX-License-Identifier: Apache-2.0
set -eu
/bin/busybox ip link set lo up
exec /usr/bin/qemu-system-x86_64 -machine microvm,accel=kvm -cpu host -m 128 -smp 1 -nodefaults -no-reboot -nographic -serial stdio -qmp tcp:127.0.0.1:4444,server=on,wait=off -kernel /inner-bzImage -initrd /inner.cpio -append 'console=ttyS0 panic=1 rdinit=/init' "$@"
