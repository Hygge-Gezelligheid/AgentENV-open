# CLEAR comparison contract

Status: proposed controls for subsequent work; no new performance trial is started by this document. Existing measurements and failures remain intact. The original confidence gates and seven workload cells are unchanged.

## Behavior to implement

Each successful CLEAR establishes the baseline for the next dirty-memory export. Ordinary exports include newly reported dirty ranges, explicit zero updates and the runtime's conservatively dirty pages. Retain the existing periodic compaction policy: a merged output can contain earlier updates. Keeping bounded chains avoids changing the format or introducing another reader mechanism. Report incremental export bytes and compaction rewrite bytes separately. Do not describe a merged artifact as a latest-only delta.

Complete restoration must survive source termination, capture cleanup and earlier-record deletion. Retained-history space and latest-only retention are separate outcomes. The current implementation has passed the measured retained-history space checks; latest-only referenced bytes can be larger. Record deletion is not evidence of blob garbage collection.

## Two different questions

| Study | Question | Fixed inputs | Intended differences | Use |
| --- | --- | --- | --- | --- |
| A: whole implementation | Does CLEAR integration meet storage and latency requirements over repeated captures? | Seed, RAM/vCPU size, deterministic workload, generation count, runtime/kernel, driver, endpoint definitions and host policy | Original implementation versus code candidate, including produced layer layout and lifecycle | Original acceptance |
| B: reader/layout attribution | What contribution comes from reader code and layer layout for the same saved VM state? | VM-state bytes, full logical memory bytes, rootfs/tools/drives, common API server/runtime, build settings, cache profile and endpoint | Reader version and memory layout as explicitly named factors | Diagnosis only |

A cannot isolate CLEAR, reader code or layer count individually. B cannot establish whole-lifecycle acceptance or replace a failed A endpoint. ARM/debug-server controls and earlier batches retain their own identities and are never pooled with either new cohort.

## A: preserve the original endpoint

Keep the seven cells: 512 MiB idle/rotating/hotspot/dense for eight generations; 4096 MiB rotating/dense for eight generations; 512 MiB rotating for 96 generations. Keep the fixed confirmation counts 120/60/60/30/30/30/30. Any changed candidate requires a new, separately identified confirmation; the current six failed cells are not overwritten or extended after inspection.

The historical raw `warm` endpoint is named **post-save first restore** in explanations. Reproduce its exact sequence: finish snapshot POST; collect the existing space/pause measurements; issue the first restore POST; record readiness when the successful public response is completely read; run the first guest checksum as a separate endpoint. There is no explicit input eviction or complete prewarm. Cache residency is not assumed equal. Preserve the running source VM during these measurements.

Targeted-cold follows that restore and deletion, using the existing flush/eviction path and <=5% memory-layer residency condition. It leaves process/index/metadata caches normal. Save/pause endpoints and the observation-by-observation combined endpoints retain their existing boundaries. Workload setup and checksum preparation stay outside save timing for both variants.

A new endpoint or a residency-instrumented driver is a new protocol version. Apply it to both variants and report it separately. It must not silently replace post-save first restore or inherit the earlier samples.

## B: isolate one input and two explicit factors

Use the retained 96-generation candidate snapshot as the fixed VM state. Before measurement, create a flattened memory representation once and verify all virtual RAM bytes against the original 33-layer representation. Keep the VM-state file and every non-memory artifact identical. Record original and flat blob manifests. A matching payload checksum alone is insufficient to establish complete memory equivalence.

Use one common API server and Firecracker binary. Build two release daemons with the same compiler, lockfile, features, profile and environment: baseline reader and current reader. A source manifest must prove that the release-relevant differences are confined to `local.rs` and `readonly.rs`. This diagnoses the combined reader changes; it does not distinguish NOWAIT from parallel mapping reads.

The design is reader {baseline, current} x memory layout {33 layers, flattened}, with 30 independent randomized complete blocks per cache profile, fixed before running. Each observation uses its own node state, runtime directory and daemon. A common untimed base-template launch may initialize the daemon/tools path, but the target snapshot must not be restored beforehand.

Keep two separately named file-cache profiles: referenced local files resident (>=99% verified), and targeted memory files cold (<=5% verified). File warming, not an untimed target restore, prepares the first profile. Rootfs/tools preparation must match across all four cells. Both profiles measure the first target open in the fresh node; neither is equated with A's post-save or after-warm-restore cache state. A later repeated-device/index-hot observation would be another endpoint.

Freeze all timing boundaries and placement rules before execution. Report readiness, checksum and their observed combined duration independently. If a tracing/profiling tool is used, its cohort is diagnostic and its perturbed times are excluded from uninstrumented estimates.

## Preflight and reporting

Before a cohort starts, save immutable manifests for every program, shared dependency, seed, artifact, driver, analyzer and configuration. Validate actual daemon spawn paths, not build-cache filenames. Preserve a canonical configuration comparison with only allowed path/variant differences. Serialize trials; record CPU/NUMA placement and shared-host load without altering other users' services. Any new placement policy is a separately declared protocol applied to every cell.

Validate generation counts, restore positions, payload checksums, complete-trial markers and actual layer counts. Cache-precondition failures, unsuccessful requests and incomplete observations are retained and prevent acceptance of that cohort. Do not silently retry, exclude outliers, change sample counts, select a favorable subset or pool different driver/program/cache definitions.

Use the same frozen analysis for both variants. Resample complete independent paired blocks 10000 times; never treat correlated generations as independent replicas. Ratios divide the two sample medians, not per-pair ratios. Compute combined durations per observation before calculating their median. Publish raw numeric observations alongside the source and program identities.

Original A gates remain: every latency endpoint's one-sided 95% upper ratio <=1.05 for p50 and <=1.10 for p95, and the primary space upper mean-ratio bound below one, in every complete cell. B can guide a small code change, but cannot waive these gates. A runtime change is kept only with relevant correctness coverage and a reproducible end-to-end benefit; a microbenchmark win alone is insufficient.
