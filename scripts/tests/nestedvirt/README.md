# Nested KVM test tools

All commands below run from the AgentENV repository root on a dedicated Linux
x86_64 Intel nested-KVM host. Install Python 3.11+, QEMU x86 system binaries,
static BusyBox at `/bin/busybox`, cpio, e2fsprogs and Bash. Allow access to
`/dev/kvm`. Use a fresh disposable work directory with at least 5 GiB free.

Supply the binaries explicitly:

- `--inner-kernel`: x86 bzImage used by QEMU inside the sandbox.
- `--kernel`: uncompressed outer guest vmlinux ELF with built-in KVM, used by
  the raw Firecracker test. The AgentENV API test uses the node's kernel config.
- `--firecracker`: the nested-KVM Firecracker executable under test.

No Firecracker source checkout or kernel build directory is needed.

## Prepare the image and test raw snapshot recovery

```sh
python3 scripts/tests/nestedvirt/prepare_probe.py /absolute/path/nestedvirt-work \
  --inner-kernel /absolute/path/bzImage
python3 scripts/tests/nestedvirt/raw_smoke.py /absolute/path/nestedvirt-work \
  --firecracker /absolute/path/firecracker --kernel /absolute/path/vmlinux \
  --rounds 3
```

Preparation creates `probe-root/` and a 256 MiB `probe-root.ext4`. It includes
QEMU and its shared libraries, BusyBox, Bash, the inner kernel/initrd, and guest
`/start-inner` and `/stop-inner` commands. `start-inner.sh` launches QEMU with
`accel=kvm` and a loopback QMP socket; `inner-init.sh` emits `INNER_TICK` counters.
`outer-init.sh` supplies the serial shell for the raw test.

The raw test captures and restores three stages: before userspace KVM use,
after QEMU initializes KVM with its vCPU stopped (`-S`), and during inner guest
execution. It checks QMP state or counter growth after recovery, then starts a
fresh inner VM. Require successful script exit and nine unique PASS records in
`evidence/raw-results.json` for three rounds; retain serial logs and snapshots.
The stopped-stage test replaces the restored inner VM; it does not continue it.

## Test AgentENV fork and pause/resume

The same `probe-root/` feeds `../serve-nestedvirt-fixture.py`, which creates a
local OCI image and substitutes `../nestedvirt-fixture-init.sh` for the outer
init. `../nestedvirt-api-smoke.py` builds a template, creates two fork children,
pauses/reconnects the source, and checks inner execution in those sandboxes.
It also starts a fresh inner VM after recovery. The runner deletes its own
sandboxes and template when finished.

Use the [node setup and API commands](../../../docs/src/configuration/nested-kvm.md#validate-snapshots-and-the-normal-api)
to configure the dedicated server, loopback registry, SDK and kernel release
check. Keep that registry process running until the API smoke finishes. These
tools do not start or reconfigure an existing AgentENV service.

## Lightweight checks

AgentENV's integration workflow checks Python/shell syntax and runs
`scripts/tests/test-nestedvirt-fixture.py` to verify OCI fixture rebuilding.
These checks do not require KVM. The raw and API hardware matrices are explicit
commands on a nested-KVM host; they are not run by the hosted syntax job.
