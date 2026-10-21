#!/bin/sh
# Copyright 2026 AgentENV contributors.
# SPDX-License-Identifier: Apache-2.0
set -eu
pid=$(/bin/busybox cat "${1:?PID_FILE}")
test "$(/bin/busybox readlink /proc/"$pid"/exe)" = /usr/bin/qemu-system-x86_64
kill -TERM "$pid"
attempt=0
while kill -0 "$pid" 2>/dev/null; do
    processes=$(/bin/busybox ps -o pid,stat)
    state=$(printf '%s\n' "$processes" | /bin/busybox awk -v target="$pid" '$1 == target {print $2}')
    case "$state" in
        Z*|"") break ;;
    esac
    attempt=$((attempt + 1))
    test "$attempt" -lt 100
    /bin/busybox sleep 0.1
done
echo "QEMU_STOPPED=$pid"
