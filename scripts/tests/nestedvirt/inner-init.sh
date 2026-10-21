#!/bin/sh
# Copyright 2026 AgentENV contributors.
# SPDX-License-Identifier: Apache-2.0
set -eu
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
n=0
while true; do
    echo "INNER_TICK=$n"
    n=$((n+1))
    /bin/busybox sleep 1
done
