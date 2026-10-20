# Nested KVM with a Linux 6.18 guest

An x86_64 AgentENV node can expose `/dev/kvm` inside its sandboxes using
`kvcache-ai/firecracker` at `721f05707a41a3337d90b0d6921129b4885aa49a`
and a Linux 6.18 guest kernel with built-in KVM. That Firecracker checkout
contains the `tools/nestedvirt/` tools and the x86_64 legacy snapshot loader;
they are not located in the AgentENV checkout. The host must expose
VMX or SVM and provide working nested virtualization.

The historical Intel hardware candidate was based on
`c7e1cceb6bf643f239ddac804a535ceb8ac93cb1`, with two outer vCPUs and 512 MiB
outer memory. That commit does not contain the tools below. Its hardware
results do not establish validation of binaries rebuilt from the current pin;
retain the original asset identities and validate the new build separately.

## Install matching assets

Build Firecracker and `cpu-template-helper` together from that source checkout.
Build the guest kernel using `tools/nestedvirt/build-kernel.sh` in the matching
Firecracker checkout. The guest configuration includes KVM, MP-table CPU
discovery, virtio MMIO command-line device discovery, DAMON reclaim and page
reporting. Keep the generated configuration, build commands, compiler versions
and artifact checksums with the deployment.

The guest source archive is `linux-6.18.tar.xz`, with SHA-256
`9106a4605da9e31ff17659d958782b815f9591ab308d03b0ee21aad6c7dced4b`.
The integration candidate uses generated configuration SHA-256
`70699be91477f23a86be3fc15d0d138bd8f3ccbe26def6898570d1406739f55a`.
Record source commits and any local modifications separately from binary
versions. A source export without Git history has a limited delivery identity.
Matching current source files to a commit does not establish the original
compilation inputs or guarantee identical hashes after rebuilding.

## Use the existing node configuration

Configure nested KVM through the existing node TOML selected by `--config` or
`AENV_CONFIG_PATH`. The standard entry point is `config/default.toml`; no
separate nested-KVM configuration is required. Preserve the node's existing
state, storage, network and dependency directories.

Default KVM assets still come from `config/deps_manifest.toml`: Firecracker
`1.15.1-patch-v1` and Linux `vmlinux-6.1.175`. This preparation does not upgrade
those defaults. The nestedvirt branch writes snapshot format `1000.0.0`, while
the current default Firecracker writes `9.0.0`. The pinned checkout can load
x86_64 `9.0.0` state with absent nested state and re-save it as `1000.0.0`;
it does not write v9 snapshots. This format conversion does not establish
hardware compatibility for every old snapshot. Before changing default assets,
validate old snapshot recovery on the target host and publish the matching
Firecracker/helper and kernel assets.
Keep the existing dependency catalog until both requirements are satisfied.

For a dedicated Intel validation node, build the matching assets and edit the
following fields in its existing configuration. These are local artifact
overrides, not release download URLs.

| Existing field | Required setting |
| --- | --- |
| `[firecracker].version` | A distinct local asset identifier, such as `1.15.1-nestedvirt-721f0570`. Use a new identifier if the installed asset bytes change. |
| `[firecracker].binary_path` | The actual executable path, normally `deps_path/firecracker/<version>/firecracker`. |
| `[kernel].image_path` | The actual path to the built, uncompressed Linux 6.18 guest ELF. |
| `[firecracker].boot_args` | Preserve the complete existing value and append `no-kvmapf`. |
| `[pool.block].enabled` | `true`, already enabled in the shipped default configuration. |

Install `cpu-template-helper` from the same build under
`deps_path/firecracker/<version>/cpu-template-helper`. Unlike Firecracker, the
helper is resolved from this version directory even when `binary_path` is set.
Use immutable version directories so an existing cached asset cannot silently
be mistaken for a new build. Provision the other dependencies normally. These
are guest assets; no host kernel upgrade is required for a host that already
provides the needed nested-KVM capabilities.

Disabling outer guest KVM asynchronous page faults with `no-kvmapf` is a
functional workaround verified with the selected Intel assets. The exact
failure mechanism and performance effects remain unproven. AMD and ARM
behavior are unverified. PVM and other architectures keep their existing
asset selections; this guide validates the x86_64 KVM path only.

`--config` selects one TOML file. It does not merge it with another file.
Omitted fields use schema defaults and applicable environment overrides.
`boot_args` replaces the entire string, so retain the existing boot and DAMON
settings when appending `no-kvmapf`. Check environment overrides, particularly
`AENV_HOME_PATH`, `AENV_RUNTIME_PATH` and `AENV_DEPS_PATH`. Isolate the hardware
validation node's state through these existing settings and use non-overlapping
network pools. Do not point the validation node at live production state.

Build **new templates** for nested KVM and launch new sandboxes from them.
Check the kernel version, both guest CPUs, `/dev/kvm` and `/proc/cmdline` in
those sandboxes. Existing templates, fork snapshots and paused sandboxes retain
their captured kernel memory and startup arguments. Changing a kernel path or
boot arguments does not upgrade them. Keeping default dependencies preserves
the existing deployment; it does not establish that the nestedvirt runtime can
restore old snapshots.

To undo the validation deployment, stop its node normally and restore its saved
configuration and asset selections. Use the previous templates with their
compatible runtime. To remove `no-kvmapf` while retaining nested KVM, rebuild
new templates after editing the complete command line. The original setting
stalled inner execution in the tested API path.

## Validate snapshots and the normal API

Run `tools/nestedvirt/raw_smoke.py` from the matching Firecracker checkout.
Then run the AgentENV smoke against a dedicated node using its existing config and a new template. Each
path requires three rounds for each of `unused`, `initialized` and `running`.
QEMU explicitly uses KVM. Two successive execution counter samples must grow
before running-stage capture and after fresh inner startup.

The normal API test creates two fork children, pauses and reconnects the source,
and starts a fresh inner VM in all three sandboxes. Its running stage also
checks that existing inner execution advances across fork and pause/connect.
The initialized stage requires QMP to report KVM enabled/present and a stopped
`prelaunch` instance before capture and after fork and reconnect. Rebuild the
fixture with the matching Firecracker tools, which expose QMP on guest loopback
port 4444. The initialized stage replaces the restored stopped `-S` inner instance before
starting the fresh one. Continuing that same initialized inner instance is
outside this test's coverage.

Prepare the real probe root filesystem using Firecracker's
`tools/nestedvirt/prepare_probe.py`. It includes static BusyBox, real Bash,
QEMU with its shared libraries, an inner kernel and initrd, `/start-inner` and
`/stop-inner`. Use Python 3.11 or later in a dedicated virtual environment.

```sh
python3 -m pip install -r scripts/tests/nestedvirt-requirements.txt
python3 scripts/tests/serve-nestedvirt-fixture.py /absolute/path/nestedvirt-work --port 18091
```

The fixture binds loopback and serves a real OCI image. Configure only the
test node's regctl to use HTTP for this registry, through a dedicated wrapper
passing `--host reg=127.0.0.1:18091,tls=disabled`. Set a proxy domain such as
`nested.local` in `[sandbox_proxy].domains`, bind the test API to loopback, and
use network pools that do not overlap other runtimes. In another terminal
using the same virtual environment, run the following command. Replace
`/absolute/path/node.toml` with the node configuration edited above, including
`no-kvmapf`, and used to start the test server.

```sh
python3 scripts/tests/nestedvirt-api-smoke.py \
  http://127.0.0.1:18090 /absolute/path/test-home/secrets/api-key \
  127.0.0.1:18091/nested:latest /absolute/path/evidence --rounds 3 \
  --config /absolute/path/node.toml
```

Pass the same node configuration to the smoke and the server. `--config` takes
precedence over `AENV_CONFIG_PATH`; otherwise the smoke reads
`config/default.toml`. The selected file must contain the effective complete
boot arguments, including `no-kvmapf`, used by that server.

The test uses the official E2B client for guest commands and reads the managed
API key without printing it. Require successful process exit and all nine
unique phase/round records in `api-results.json`. Inspect `api-commands.jsonl`
for kernel arguments, counter growth and successful guest command completion.
Retain raw results and serial logs with the exact config, source and asset
identities. Stop the fixture and dedicated test node normally after testing.
Rebuild the fixture and template when selected assets or workload files change.

The existing block-pool setting enables shared read-only tools and snapshot
devices. The smoke deletes every sandbox and its template,
including after a failed build or guest command. Cleanup attempts continue after
individual failures and retain the original failure. `api-results.json` is
published only after all cleanup succeeds.

The integration workflow checks smoke syntax and rebuilds the real HTTP fixture
twice to verify removed files disappear. The raw and API nine-case matrices run
on a dedicated nested-KVM host; these hosted CI checks do not exercise nested KVM.

Performance, precise APF completion mechanics, AMD runtime behavior and cluster
CPU-template intersections require separate evidence before making those claims.

## Test recovery across Firecracker versions

`orchestrator_v9_snapshot_upgrade` is an opt-in x86_64 KVM integration test in
`tests/integration/orchestrator.rs`. It runs the lifecycle test in two separate
processes: the first creates and pauses a real sandbox with a v9-writing
Firecracker; the second restores it with the current runtime. Between processes,
it verifies the saved state is v9 and replaces only the persisted executable
path with a nonexistent path. After recovery it checks guest envd access and
pauses again, requiring the new state file to be `1000.0.0`.

Prepare two node configurations on an isolated test host: `v9-node.toml` selects
the old v9-writing runtime and `current-node.toml` selects the pinned Firecracker
above. Keep the same state, storage, network, guest kernel, boot arguments and
guest assets in both configurations; use absolute asset paths. Provision the
matching helpers and other dependencies before running. This test exercises old
snapshot recovery, not enabling nested KVM inside an existing old guest.

After the normal integration-test host/dependency setup, build the daemon with
`make build-ublk PROFILE=debug`. Run from the AgentENV checkout (replace the
example paths):

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

The test is checked into the repository and compiled by the integration-test
job. It is ignored during the default test run because that job currently
provisions only the default runtime, not the pinned v1000 runtime. A configured
KVM CI runner can run the same explicit command. Missing configuration or an
incorrect snapshot version fails the requested test rather than skipping it.
