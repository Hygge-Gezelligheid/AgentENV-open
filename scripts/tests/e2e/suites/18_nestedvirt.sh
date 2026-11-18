#!/usr/bin/env bash
set -euo pipefail

SUITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SUITE_DIR}/../lib/helpers.sh"
init_suite "18_nestedvirt"
log "Suite: Nested KVM Snapshot Recovery"

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
  guest 'if pid=$(cat /run/nestedvirt-probe/inner.pid) && [ -r "/proc/$pid/status" ]; then
      sed -n "/^State:/s/^/QEMU /p" "/proc/$pid/status"
    else
      echo "QEMU process unavailable (PID ${pid:-unknown})"
    fi
    tail -n 80 /run/nestedvirt-probe/inner.log' 5 >&2 || true
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
  [[ -n "$first" ]] || die "${phase}: could not read L2 state from L1"
  die "${phase}: inner guest stopped advancing from tick $first"
}

# Allow deployments to use the prepared fixture once it is available in their registry.
fixture_image=${E2E_NESTEDVIRT_IMAGE:-}
if [[ -n "$fixture_image" && ! "$fixture_image" =~ @sha256:[0-9a-f]{64}$ ]]; then
  die "E2E_NESTEDVIRT_IMAGE must include its manifest digest (@sha256:...)"
fi
api_post /v3/templates "$(jq -nc --arg name "e2e-nested-$(date +%s%N)" '{name:$name,cpuCount:2,memoryMB:1024}')"
expect_status 202 "create nested template"
template_id=$(jq -er '.templateID' <<< "$HTTP_BODY")
build_id=$(jq -er '.buildID' <<< "$HTTP_BODY")
track_template "$template_id"
api_post "/v2/templates/${template_id}/builds/${build_id}" "$(jq -nc --arg image "${fixture_image:-$E2E_TEMPLATE_USER_IMAGE}" '{fromImage:$image}')"
expect_status 202 "build nested template"
wait_for_template_build "$template_id" 180 "$build_id" || die "Nested template build did not become ready"
sandbox_id=$(create_sandbox "$template_id" 900); _sync_http
expect_status 201 "create L1 sandbox"
track_sandbox "$sandbox_id"
wait_for_sandbox_state "$sandbox_id" running 30 || die "L1 did not start"
wait_guest
guest 'test "$(uname -m)" = x86_64; test -c /dev/kvm'
_pass "L1 exposes /dev/kvm"

# Keep the QEMU invocation and state checks under the control of the test checkout.
cat > "$work/nestedvirt-guest.sh" <<'NESTEDVIRT_GUEST'
#!/bin/sh
# Copyright 2026 AgentENV contributors.
# SPDX-License-Identifier: Apache-2.0
set -eu
mode=${1:?Expected prepare, read-state or start-inner}
if [ "$#" -gt 0 ]; then shift; fi
case "$mode" in
    prepare)
        export DEBIAN_FRONTEND=noninteractive
        apt-get -o Acquire::Retries=2 -o APT::Update::Error-Mode=any update
        # Use the security pocket for the inner kernel and its matching modules.
        apt-get -o Acquire::Retries=2 install -y --no-install-recommends qemu-system-x86 busybox-static cpio linux-image-virtual/noble-security cpuid
        root=$(mktemp -d)
        trap 'rm -rf "$root"' EXIT
        mkdir -p "$root/bin" "$root/dev" "$root/proc" "$root/sys"
        mknod "$root/dev/console" c 5 1
        mknod "$root/dev/null" c 1 3
        cp /bin/busybox "$root/bin/busybox"
        ln -s busybox "$root/bin/sh"
        install -m 0755 /nestedvirt-init "$root/init"
        (cd "$root"; find . -print | cpio -o -H newc) > /inner.cpio
        install -m 0644 /boot/vmlinuz /inner-bzImage
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
    start-inner)
        /bin/busybox ip link set lo up
        # Provide the RTC for boot probing. Limit kernel output on the emulated UART
        # to warnings and errors so serial writes do not delay the first heartbeat.
        exec /usr/bin/qemu-system-x86_64 -machine microvm,accel=kvm,rtc=on -cpu host \
            -m 128 -smp 1 -nodefaults -no-reboot -nographic -serial stdio \
            -qmp tcp:127.0.0.1:4444,server=on,wait=off \
            -kernel /inner-bzImage -initrd /inner.cpio \
            -append 'console=ttyS0 loglevel=5 panic=1 rdinit=/init' "$@"
        ;;
    *) echo "Unknown guest operation: $mode" >&2; exit 2 ;;
esac
NESTEDVIRT_GUEST
payload=$(base64 < "$work/nestedvirt-guest.sh" | tr -d '\n')
guest "printf '%s' '$payload' | base64 -d > /nestedvirt-guest; chmod +x /nestedvirt-guest"
if [[ -z "$fixture_image" ]]; then
  init_payload=$(base64 < "$repo_root/images/nestedvirt/init" | tr -d '\n')
  guest "printf '%s' '$init_payload' | base64 -d > /nestedvirt-init; /nestedvirt-guest prepare" 600
fi
guest 'test -s /inner-bzImage; test -s /inner.cpio'
check_apf_policy
guest 'mkdir -p /run/nestedvirt-probe; nohup /nestedvirt-guest start-inner > /run/nestedvirt-probe/inner.log 2>&1 < /dev/null & echo $! > /run/nestedvirt-probe/inner.pid'
# Bound cold boot by elapsed time. Inspect once more after the final sleep.
boot_deadline=$((SECONDS + 180))
while true; do
  if read_tick "cold boot" 5 && [[ "$tick" =~ ^[0-9]+$ && -n "$boot_id" ]]; then break; fi
  if ((SECONDS >= boot_deadline)); then
    dump_inner_log
    die "L2 boot markers did not appear within 180s"
  fi
  sleep 1
done
original_boot="$boot_id"
check_progress 0 "$original_boot" "before capture"
before_snapshot="$tick"

api_post "/sandboxes/${sandbox_id}/snapshots" "$(jq -nc --arg name "e2e-nested-snapshot-$(date +%s%N)" '{name:$name}')"
expect_status 201 "capture running L2 in outer snapshot"
snapshot_id=$(jq -er '.snapshotID' <<< "$HTTP_BODY")
snapshot_name=$(jq -er '.names[0]' <<< "$HTTP_BODY")
track_template "$snapshot_id"
delete_sandbox "$sandbox_id"
expect_status 204 "delete original L1"
api_get "/sandboxes/${sandbox_id}"
expect_status 404 "original L1 is gone"

sandbox_id=$(create_sandbox "$snapshot_name" 300); _sync_http
expect_status 201 "restore L1 from snapshot"
track_sandbox "$sandbox_id"
wait_for_sandbox_state "$sandbox_id" running 30 || die "Restored L1 did not start"
wait_guest
check_apf_policy
# There is deliberately no start-inner between capture and these checks.
check_progress "$before_snapshot" "$original_boot" "snapshot restore"
before_pause="$tick"
api_post "/sandboxes/${sandbox_id}/pause"
expect_status 204 "pause restored L1"
wait_for_sandbox_state "$sandbox_id" paused 30 || die "L1 did not pause"
api_post "/sandboxes/${sandbox_id}/connect" '{"timeout":300}'
expect_status 201 "resume L1"
wait_for_sandbox_state "$sandbox_id" running 30 || die "L1 did not resume"
wait_guest
check_apf_policy
check_progress "$before_pause" "$original_boot" "pause/resume"
suite_summary "18_nestedvirt"
