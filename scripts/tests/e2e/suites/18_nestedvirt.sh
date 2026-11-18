#!/usr/bin/env bash
set -euo pipefail

SUITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SUITE_DIR}/../lib/helpers.sh"
init_suite "18_nestedvirt_cold_stress"
log "Suite: Nested KVM Cold-Boot Stress Reproduction"

repo_root="$(cd "${SUITE_DIR}/../../../.." && pwd)"
aenv="${AENV_BIN:-${CARGO_TARGET_DIR:-${repo_root}/target}/debug/aenv}"
[[ -x "$aenv" ]] || die "Build the test client with make build-aenv"
work=$(mktemp -d)
cleanup() {
  _cleanup_e2e
  rm -rf "$work"
}
trap cleanup EXIT
export XDG_CONFIG_HOME="$work"
mkdir -p "$work/aenv"
printf 'url = "%s"\napi_key = "%s"\n' "$AENV_URL" "$AENV_API_KEY" > "$work/aenv/credentials"
chmod 600 "$work/aenv/credentials"

expect_status() {
  assert_status "$HTTP_STATUS" "$1" "$2"
  [[ "$HTTP_STATUS" == "$1" ]] || exit 1
}

guest() {
  timeout "${2:-30}" "$aenv" exec "$sandbox_id" -- sh -eu -c "$1"
}

wait_guest() {
  for ((attempt = 0; attempt < 30; attempt++)); do
    if guest 'true' 5 >/dev/null 2>&1; then return; fi
    sleep 1
  done
  die "Guest command service did not become ready"
}

# Do not accept /dev/kvm alone as proof of acceleration. Ask the running QEMU.
check_kvm() {
  local output
  output=$(guest "printf '%s\\n' '{\"execute\":\"qmp_capabilities\"}' '{\"execute\":\"query-kvm\",\"id\":\"kvm\"}' | /bin/busybox nc -w 2 127.0.0.1 4444")
  printf '%s\n' "$output" | jq -s -e 'any(.[]; .id == "kvm" and .return.enabled == true and .return.present == true)' >/dev/null
  _pass "QEMU reports KVM enabled"
}

check_apf_policy() {
  local output expected value
  local -a values
  guest '! grep -qw no-kvmapf /proc/cmdline'
  expected=$(guest 'getconf _NPROCESSORS_ONLN')
  output=$(guest 'cpuid -r -l 0x40000001')
  mapfile -t values < <(printf '%s\n' "$output" | sed -n 's/.*eax=\(0x[[:xdigit:]]*\).*/\1/p')
  [[ "${#values[@]}" == "$expected" ]] || die "Missing KVM feature leaf for a vCPU"
  for value in "${values[@]}"; do
    (( (value & (1 << 10)) == 0 )) || die "Nested APF VM-exit delivery is still exposed"
    (( (value & ((1 << 4) | (1 << 14))) == ((1 << 4) | (1 << 14)) )) || die "Ordinary APF features are missing"
  done
  _pass "All vCPUs retain ordinary APF with nested APF VM-exit delivery masked"
}

dump_inner_log() {
  guest 'tail -n 80 /run/nestedvirt-probe/inner.log' 5 >&2 || true
}

read_tick() {
  local output
  tick=""; boot_id=""
  output=$(guest '/nestedvirt-guest read-state' "${2:-30}") || return 1
  if [[ "$output" == FAULT* ]]; then
    dump_inner_log
    die "$1: $output"
  fi
  read -r boot_id tick <<< "$output"
}

check_progress() {
  local minimum="$1" expected_boot="$2" phase="$3" first=""
  for ((attempt = 0; attempt < 20; attempt++)); do
    sleep 1
    # Retry transient command failures, but reject invalid restored state.
    if ! read_tick "$phase"; then continue; fi
    if [[ ! "$tick" =~ ^[0-9]+$ || "$boot_id" != "$expected_boot" ]]; then
      dump_inner_log
      die "${phase}: expected boot $expected_boot, observed boot=${boot_id:-missing} tick=${tick:-missing}"
    fi
    if [[ -z "$first" ]]; then first="$tick"; fi
    if ((tick > first && tick > minimum)); then
      _pass "${phase}: same inner boot, counter ${first} -> ${tick} (before capture: ${minimum})"
      check_kvm
      return
    fi
  done
  dump_inner_log
  die "${phase}: inner guest stopped advancing"
}

# Use the ordinary Ubuntu E2E image; install the L2 workload in this test's L1.
api_post /v3/templates "$(jq -nc --arg name "e2e-nested-$(date +%s%N)" '{name:$name,cpuCount:2,memoryMB:1024}')"
expect_status 202 "create nested template"
template_id=$(jq -er '.templateID' <<< "$HTTP_BODY")
build_id=$(jq -er '.buildID' <<< "$HTTP_BODY")
track_template "$template_id"
api_post "/v2/templates/${template_id}/builds/${build_id}" "$(jq -nc --arg image "$E2E_TEMPLATE_USER_IMAGE" '{fromImage:$image}')"
expect_status 202 "build nested template"
wait_for_template_build "$template_id" 180 "$build_id" || die "Nested template build timed out"
sandbox_id=$(create_sandbox "$template_id" 900); _sync_http
expect_status 201 "create L1 sandbox"
track_sandbox "$sandbox_id"
wait_for_sandbox_state "$sandbox_id" running 30 || die "L1 did not start"
wait_guest
guest 'test "$(uname -m)" = x86_64; test -c /dev/kvm'
_pass "L1 exposes /dev/kvm"

# Guest payload: prepare and launch L2 in L1; inner-init runs inside L2.
cat > "$work/nestedvirt-guest.sh" <<'NESTEDVIRT_GUEST'
#!/bin/sh
# Copyright 2026 AgentENV contributors.
# SPDX-License-Identifier: Apache-2.0
set -eu
mode=${1:?Expected prepare, read-state, inner-init or start-inner}
if [ "$#" -gt 0 ]; then shift; fi
case "$mode" in
    prepare)
        test "$(uname -m)" = x86_64
        test -c /dev/kvm
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        # Use the security pocket for the inner kernel and its matching modules.
        apt-get install -y --no-install-recommends qemu-system-x86 busybox-static cpio linux-image-virtual/noble-security cpuid
        root=$(mktemp -d)
        trap 'rm -rf "$root"' EXIT
        mkdir -p "$root/bin" "$root/dev" "$root/proc" "$root/sys"
        mknod "$root/dev/console" c 5 1
        mknod "$root/dev/null" c 1 3
        cp /bin/busybox "$root/bin/busybox"
        ln -s busybox "$root/bin/sh"
        cp /nestedvirt-guest "$root/nestedvirt-guest"
        printf '#!/bin/sh\nexec /bin/sh /nestedvirt-guest inner-init\n' > "$root/init"
        chmod +x "$root/init"
        (cd "$root"; find . -print | cpio -o -H newc) > /inner.cpio
        kernel=$(find /boot -name 'vmlinuz-*' | sort | tail -n 1)
        test -n "$kernel"
        cp "$kernel" /inner-bzImage
        ;;
    read-state)
        # Parse serial output in L1 so polling does not transfer the growing boot log.
        awk '
            { sub(/\r$/, "") }
            /^\[[[:space:]]*[0-9.]+\] / && /Kernel panic - not syncing:|BUG: kernel|general protection fault|Oops:|invalid opcode:/ {
                print "FAULT " $0
                failed = 1
                exit
            }
            /^INNER_BOOT_ID=/ { boot = $0; sub(/^INNER_BOOT_ID=/, "", boot); tick = "" }
            /^INNER_TICK=[0-9]+$/ { tick = $0; sub(/^INNER_TICK=/, "", tick) }
            END { if (!failed && length(boot) == 36 && boot ~ /^[0-9a-f-]+$/ && tick != "") print boot, tick }
        ' /run/nestedvirt-probe/inner.log
        ;;
    inner-init)
        /bin/busybox mount -t proc proc /proc
        /bin/busybox mount -t sysfs sysfs /sys
        echo "INNER_BOOT_ID=$(/bin/busybox cat /proc/sys/kernel/random/boot_id)"
        n=0
        while true; do
            echo "INNER_TICK=$n"
            n=$((n+1))
            /bin/busybox sleep 1
        done
        ;;
    start-inner)
        /bin/busybox ip link set lo up
        # Provide the RTC that the inner kernel probes during boot.
        exec /usr/bin/qemu-system-x86_64 -machine microvm,accel=kvm,rtc=on -cpu host \
            -m 128 -smp 1 -nodefaults -no-reboot -nographic -serial stdio \
            -qmp tcp:127.0.0.1:4444,server=on,wait=off \
            -kernel /inner-bzImage -initrd /inner.cpio \
            -append 'console=ttyS0 panic=1 rdinit=/init' "$@"
        ;;
    *) echo "Unknown guest operation: $mode" >&2; exit 2 ;;
esac
NESTEDVIRT_GUEST
payload=$(base64 < "$work/nestedvirt-guest.sh" | tr -d '\n')
guest "printf '%s' '$payload' | base64 -d > /nestedvirt-guest; chmod +x /nestedvirt-guest; /nestedvirt-guest prepare" 600
check_apf_policy

cat > "$work/nestedvirt-cold-stress.sh" <<'NESTEDVIRT_STRESS'
#!/bin/sh
set -eu

rounds=${1:-100}
out=/run/nestedvirt-cold-stress
mkdir -p "$out"
: > "$out/results.csv"
printf 'round,result,qemu_rc,elapsed_s,boot_id,tick,log_bytes\n' > "$out/results.csv"
uname -a > "$out/l1-uname.txt"
cat /proc/cpuinfo > "$out/l1-cpuinfo.txt"
/usr/bin/qemu-system-x86_64 --version > "$out/qemu-version.txt"
sha256sum /inner-bzImage /inner.cpio > "$out/workload-sha256.txt"
dmesg > "$out/l1-dmesg-before.txt" 2>&1 || true

i=1
while [ "$i" -le "$rounds" ]; do
    round=$(printf '%04d' "$i")
    log="$out/boot-$round.log"
    : > "$log"
    started=$(/bin/busybox date +%s)
    nohup /nestedvirt-guest start-inner > "$log" 2>&1 < /dev/null &
    qemu_pid=$!
    result=timeout
    boot_id=
    tick=
    attempt=0
    while [ "$attempt" -lt 225 ]; do
        if grep -Eq 'Kernel panic - not syncing:|BUG: kernel|general protection fault|Oops:|invalid opcode:' "$log"; then
            result=fault
            break
        fi
        boot_id=$(sed -n 's/^INNER_BOOT_ID=//p' "$log" | tail -n 1)
        tick=$(sed -n 's/^INNER_TICK=//p' "$log" | tail -n 1)
        if [ -n "$boot_id" ] && [ -n "$tick" ] && [ "$tick" -ge 1 ] 2>/dev/null; then
            result=pass
            break
        fi
        if ! kill -0 "$qemu_pid" 2>/dev/null; then
            result=early-exit
            break
        fi
        attempt=$((attempt+1))
        /bin/busybox sleep 0.2
    done

    if kill -0 "$qemu_pid" 2>/dev/null; then
        mkdir -p "$out/proc-$round"
        cp "/proc/$qemu_pid/status" "$out/proc-$round/status" 2>/dev/null || true
        for task in /proc/$qemu_pid/task/*; do
            tid=${task##*/}
            cat "$task/stack" > "$out/proc-$round/stack-$tid" 2>/dev/null || true
            cat "$task/wchan" > "$out/proc-$round/wchan-$tid" 2>/dev/null || true
        done
        printf '%s\n' '{"execute":"qmp_capabilities"}' '{"execute":"query-status","id":"status"}' '{"execute":"query-kvm","id":"kvm"}' \
            | /bin/busybox nc -w 2 127.0.0.1 4444 > "$out/qmp-$round.jsonl" 2>&1 || true
    fi

    if [ "$result" = pass ]; then
        printf '%s\n' '{"execute":"qmp_capabilities"}' '{"execute":"quit"}' \
            | /bin/busybox nc -w 2 127.0.0.1 4444 >/dev/null 2>&1 || true
    fi
    stop_attempt=0
    while kill -0 "$qemu_pid" 2>/dev/null && [ "$stop_attempt" -lt 25 ]; do
        stop_attempt=$((stop_attempt+1))
        /bin/busybox sleep 0.2
    done
    if kill -0 "$qemu_pid" 2>/dev/null; then
        kill "$qemu_pid" 2>/dev/null || true
        /bin/busybox sleep 1
    fi
    if kill -0 "$qemu_pid" 2>/dev/null; then
        kill -9 "$qemu_pid" 2>/dev/null || true
    fi
    qemu_rc=0
    wait "$qemu_pid" 2>/dev/null || qemu_rc=$?
    finished=$(/bin/busybox date +%s)
    log_bytes=$(wc -c < "$log")
    printf '%s,%s,%s,%s,%s,%s,%s\n' "$i" "$result" "$qemu_rc" "$((finished-started))" "$boot_id" "$tick" "$log_bytes" >> "$out/results.csv"
    if [ $((i % 10)) -eq 0 ] || [ "$result" != pass ]; then
        printf 'COLD_STRESS_PROGRESS round=%s result=%s qemu_rc=%s elapsed_s=%s\n' "$i" "$result" "$qemu_rc" "$((finished-started))"
    fi
    sync

    if [ "$result" != pass ]; then
        dmesg > "$out/l1-dmesg-after-failure.txt" 2>&1 || true
        printf '%s\n' "$round" > "$out/failed-round.txt"
        break
    fi
    i=$((i+1))
done
dmesg > "$out/l1-dmesg-after.txt" 2>&1 || true
NESTEDVIRT_STRESS

stress_payload=$(base64 < "$work/nestedvirt-cold-stress.sh" | tr -d '\n')
guest "printf '%s' '$stress_payload' | base64 -d > /nestedvirt-cold-stress; chmod +x /nestedvirt-cold-stress" 30
rounds=${NESTEDVIRT_STRESS_ROUNDS:-100}
guest "/nestedvirt-cold-stress '$rounds'" 1800

artifact_dir=${NESTEDVIRT_REPRO_ARTIFACT_DIR:-$work/repro-artifacts}
mkdir -p "$artifact_dir"
archive=$(guest 'tar -C /run -czf - nestedvirt-cold-stress | base64 -w 0' 120)
printf '%s' "$archive" | base64 -d > "$artifact_dir/l1-cold-stress.tar.gz"
guest 'cat /run/nestedvirt-cold-stress/results.csv' > "$artifact_dir/results.csv"
cat "$artifact_dir/results.csv"
if awk -F, 'NR > 1 && $2 != "pass" { failed = 1 } END { exit !failed }' "$artifact_dir/results.csv"; then
  guest 'failed=$(cat /run/nestedvirt-cold-stress/failed-round.txt); cat "/run/nestedvirt-cold-stress/boot-$failed.log"' >&2 || true
  echo "--- L1 dmesg after failure ---" >&2
  guest 'tail -n 300 /run/nestedvirt-cold-stress/l1-dmesg-after-failure.txt' >&2 || true
  echo "--- runner KVM/kernel diagnostics ---" >&2
  dmesg | tail -n 300 >&2 || true
  for pid in $(pgrep -x firecracker 2>/dev/null || true); do
    echo "--- Firecracker PID $pid ---" >&2
    tr '\0' ' ' < "/proc/$pid/cmdline" >&2 || true
    echo >&2
    for task in /proc/$pid/task/*; do
      tid=${task##*/}
      printf 'tid=%s wchan=' "$tid" >&2
      cat "$task/wchan" >&2 2>/dev/null || true
      cat "$task/stack" >&2 2>/dev/null || true
    done
  done
  die "L2 cold-boot stress reproduced a failure"
fi
_pass "all $rounds L2 cold boots completed without a kernel fault"
suite_summary "18_nestedvirt_cold_stress"
