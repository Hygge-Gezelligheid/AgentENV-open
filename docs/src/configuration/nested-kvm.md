# Nested KVM: setup and testing

This guide tests x86_64 KVM sandboxes containing a QEMU guest. Run commands from
the AgentENV repository root on a dedicated Intel nested-KVM host. Hardware
coverage so far is Intel; AMD needs a separate run.

## 1. Prepare the node

Use a host with working nested virtualization and access to `/dev/kvm`. Install
Python 3.11+, QEMU x86 system binaries, static BusyBox at `/bin/busybox`, cpio,
e2fsprogs and Bash. Use a fresh test work directory with at least 5 GiB free.

Install the nested-KVM Firecracker and matching `cpu-template-helper` from the
same revision (current source pin: `721f05707a41a3337d90b0d6921129b4885aa49a`).
Supply two kernel files: an uncompressed **vmlinux ELF for the outer sandbox**,
with built-in KVM, MP-table CPU discovery and virtio MMIO command-line discovery;
and a **bzImage for QEMU's inner guest**. Kernel building is a separate task.
The default dependency catalog still selects Firecracker `1.15.1-patch-v1` and
Linux `vmlinux-6.1.175`, so configure the test assets explicitly.

Edit the test node's existing TOML configuration:

| Field | Value |
| --- | --- |
| `[firecracker].version` | A unique identifier for the installed build. |
| `[firecracker].binary_path` | Absolute path to the nested-KVM Firecracker. |
| `[kernel].image_path` | Absolute path to the outer vmlinux ELF. |
| `[firecracker].boot_args` | Keep the complete existing string and append `no-kvmapf`. |
| `[pool.block].enabled` | `true`. |

Place the matching helper at `deps_path/firecracker/<version>/cpu-template-helper`;
the helper uses this location even with an explicit Firecracker `binary_path`.
Use a new version directory when replacing assets. `no-kvmapf` is the workaround
used in the Intel validation; retain existing boot and DAMON arguments.

Start a dedicated AgentENV server with this config. Isolate its state using
`AENV_HOME_PATH` and `AENV_RUNTIME_PATH`, select its dependencies with
`AENV_DEPS_PATH`, and use non-overlapping network pools. Bind the API to loopback
(the commands below use port 18090) and set `[sandbox_proxy].domains`, for
example to `["nested.local"]`. Configure the test node's regctl wrapper to pass
`--host reg=127.0.0.1:18091,tls=disabled` for the local image registry.

The runner expects the server to be running. Pass the same TOML to the server
and test: `--config` selects one file, with schema defaults and environment
overrides. Ensure that file contains the effective complete boot arguments.
Build fresh templates: changing kernel paths or boot arguments does not update
the kernel memory and arguments captured in existing snapshots.

## 2. Prepare the image and run the API test

In a Python virtual environment, prepare the image and start its registry:

```sh
python3 -m pip install -r scripts/tests/nestedvirt-requirements.txt
python3 scripts/tests/nestedvirt/prepare_probe.py /absolute/path/nestedvirt-work \
  --inner-kernel /absolute/path/bzImage
python3 scripts/tests/serve-nestedvirt-fixture.py /absolute/path/nestedvirt-work --port 18091
```

Preparation bundles QEMU, its libraries, Bash, BusyBox, the inner kernel/initrd,
and `/start-inner` and `/stop-inner` into `probe-root/` and `probe-root.ext4`.
The registry packages that root as an OCI image with the AgentENV fixture init.
`start-inner.sh` launches QEMU with `accel=kvm`; `inner-init.sh` emits a counter.

Keep the registry running. In another terminal using the same environment:

```sh
python3 scripts/tests/nestedvirt-api-smoke.py \
  http://127.0.0.1:18090 /absolute/path/test-home/secrets/api-key \
  127.0.0.1:18091/nested:latest /absolute/path/evidence --rounds 3 \
  --config /absolute/path/node.toml --kernel-release EXPECTED_UNAME_R
```

Replace `EXPECTED_UNAME_R` with the outer kernel's exact `uname -r` value.
The runner reads the API key file, builds a two-vCPU/512-MiB template, creates
a source sandbox and two fork children, then pauses and reconnects the source.
It tests each of these stages three times:

| Stage | Recovery check |
| --- | --- |
| `unused`: no inner VM yet | A fresh inner VM can start. |
| `initialized`: QEMU started with `-S` | QMP still reports KVM enabled and the guest stopped in `prelaunch`. |
| `running`: inner guest executing | Its counter continues advancing across fork and pause/resume. |

Each stage also starts and stops a fresh inner VM in all three sandboxes.
The initialized case replaces the stopped inner VM; resuming that same inner
VM is not covered. QMP listens on guest loopback port 4444.

Require successful script exit and nine unique phase/round records in
`api-results.json`. `api-commands.jsonl` records guest commands, kernel release,
boot arguments and counter growth. Results are published after sandbox/template
cleanup succeeds. Retain evidence with the source revisions, config and asset
checksums; rebuild the fixture when its inputs change. Stop the registry and
test server after the run.

## 3. Optional focused tests

**Direct Firecracker snapshot recovery.** Using the prepared image, this test
saves the outer VM's state and memory, stops it, and loads the snapshot in a new
Firecracker process. It checks the same three inner stages and fresh startup.

```sh
python3 scripts/tests/nestedvirt/raw_smoke.py /absolute/path/nestedvirt-work \
  --firecracker /absolute/path/firecracker --kernel /absolute/path/vmlinux \
  --rounds 3
```

Require successful exit and nine records in `evidence/raw-results.json`.
Per-VM directories retain serial logs and snapshots.

**Old v9 snapshot recovery.** `orchestrator_v9_snapshot_upgrade` in
`tests/integration/orchestrator.rs` uses the current AgentENV test binary with
old and new Firecracker processes. It saves a v9 sandbox, invalidates its saved
executable path, restores with the configured current runtime, checks envd and
saves again as v1000. Testing an upgrade from an old AgentENV binary is separate.

Prepare `v9-node.toml` and `current-node.toml` with their respective runtimes and
matching helpers. Keep state, storage, network, guest kernel, boot arguments and
other guest assets the same, using absolute paths. After the normal integration
host/dependency setup, build the daemon with `make build-ublk PROFILE=debug`:

```sh
AENV_HOME_PATH=/absolute/path/isolated-test/home \
AENV_RUNTIME_PATH=/absolute/path/isolated-test/run \
AENV_DEPS_PATH=/absolute/path/test-deps \
AENV_UBLK_DAEMON_BINARY_PATH="$PWD/target/debug/uvm-ublk-daemon" \
AENV_CONFIG_PATH=/absolute/path/current-node.toml \
AENV_TEST_V9_CONFIG=/absolute/path/v9-node.toml \
scripts/run-with-capabilities.sh cargo test -p agentenv \
  --test orchestrator_integration orchestrator::orchestrator_v9_snapshot_upgrade \
  -- --exact --ignored --nocapture
```

**Hosted CI** checks script syntax and OCI fixture rebuilding. The hardware
matrices above run explicitly on a nested-KVM host. The v9 test is compiled but
ignored by default because it requires both runtime versions.
