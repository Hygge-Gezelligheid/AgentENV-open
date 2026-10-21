#!/bin/sh
# Copyright 2026 AgentENV contributors.
# SPDX-License-Identifier: Apache-2.0
set -eu
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
test -c /dev/null
test -c /dev/kvm
echo OUTER_READY
exec /bin/busybox sh
