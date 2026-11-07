# Consecutive snapshot comparison

`run_clear_comparison.py` runs serial, randomized baseline/candidate pairs
through the public API. Each trial gets a fresh repository catalog, node home,
and runtime. Seed blobs are immutable hard links; each trial counts its full
input footprint once. There is one active measurement server. Sandbox writes
and correctness checks run outside snapshot timings.

Prepare an isolated Linux KVM/ublk host with the normal AgentENV dependencies.
Use a CLEAR-capable Firecracker for both variants. Build the original upstream
baseline and the candidate in separate checkouts/target directories. Place the
following inputs under an experiment directory:

```text
bin/baseline/server
bin/candidate/server
bin/uvm-ublk-daemon
runtime-target/x86_64-unknown-linux-musl/release/firecracker
hardware/deps/kernel/vmlinux-6.18/vmlinux.bin
hardware/deps/tools/0.1.1/tools.ext4
hardware/home/snapshot-store/repository/
seed-512.json
seed-4096.json
```

Create the two seed templates with the ignored
`clear::publish_offline_seed_template` integration test, using an isolated
`AENV_CONFIG_PATH`/`AENV_HOME_PATH`, a prepared local public image via
`AENV_CLEAR_TEST_IMAGE_CONFIG`, and `AENV_CLEAR_SEED_MEMORY_MIB=512` or `4096`.
Set `AENV_CLEAR_SEED_OUTPUT` to the corresponding JSON path above. The benchmark
uses published templates, so timed trials do not pull or convert OCI images.
Both variants use the same curated seed records and blobs.

```bash
python3 scripts/benchmarks/run_clear_comparison.py \
  --root /path/to/experiment --checkout /path/to/candidate-checkout \
  --label comparison --pairs 30
python3 scripts/benchmarks/analyze_clear_comparison.py \
  /path/to/experiment/comparison-comparison \
  --output /path/to/experiment/analysis.json
```

The API key stays in the node's private key file and is never included in sample
records. Use an unused local API port 18971 and network ranges 10.244/16 and
10.245/16. The capability runner delegates the normal non-root runtime
capabilities. Runtime binaries and all three measurement drivers are hashed
into `protocol.json` before the first trial. Keep these inputs unchanged until
the batch finishes. First-pair and failed-trial state are retained; successful
later-pair home/work directories are reclaimed after their server exits.

The workload has a tmpfs payload covering half of guest RAM. Rotating and
hotspot writes change 1% of RAM per generation; dense writes replace the entire
payload; idle performs no payload writes. This is a closed-loop workload:
there is one write/check/save sequence per generation, rather than a fixed
wall-clock write rate. Defaults cover 512 MiB idle/rotating/hotspot/dense and
4096 MiB rotating/dense, each with 8 generations, plus512 MiB rotating with 96 generations.

Measurements include:

- Snapshot API latency and lower/upper bounds on the Firecracker pause window,
  derived from its timestamped pause/resume completion logs.
- Restore API readiness, the first complete payload checksum read, and their
  combined latency, for warm and `cold_snapshot_files` modes. The latter uses
  targeted `fsync`/`posix_fadvise(DONTNEED)` on experiment snapshot data files;
  `mincore` must show at most 5% of latest memory-layer pages resident before
  timing. It retains normal process, metadata, and index caches.
- Repository logical bytes and total allocated node/work/serial bytes, counting
  hard links once per device/inode. Total growth subtracts the complete seeded
  input state and includes local capture directories and the running source's
  retained baseline. Common binaries, kernels, and fixed dependency images are
  outside snapshot growth accounting.
- Actual repository occupancy after deleting earlier snapshot records. This
  includes any unreferenced managed blobs retained by the existing repository;
  deleting records does not establish that blob garbage collection occurred.

Every trial verifies the latest payload through restore and again after
source deletion and earlier-record deletion. The separate hardware integration
suite restores every generation and compares complete guest memory.

The analyzer requires every planned generation, pair, and restore mode. It
resamples whole paired trials 10000 times, keeping correlated generations in
their original chain. Each latency endpoint reports a candidate/baseline p50,
p95, and mean ratio with confidence bounds; chain totals are also reported.
Confirmation requires at least 30 pairs and a one-sided 95% upper ratio at most 1.05
for p50 and 1.10 for p95. Space reduction requires its mean-ratio upper bound
below 1. Failed or incomplete trials invalidate the batch. Small admission
batches remain exploratory even when their point estimates look favorable.
