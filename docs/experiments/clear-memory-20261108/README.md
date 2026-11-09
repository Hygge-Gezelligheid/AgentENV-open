# CLEAR memory baseline experiment

Status on 2026-11-08: all seven fixed confirmation cells completed, with payload/deletion checks and primary retained-history space reduction passing. Six cells fail one or more unchanged latency gates; only the 4 GiB dense cell passes every latency gate. Performance acceptance is **not established**. This PR preserves experimental evidence; the implementation is reviewed separately in the code PR.

## Implementation

After a complete local capture, AgentENV owns the memory parents independently of capture/publication cleanup, calls `PUT /vm/dirty-memory-ranges/clear` while the VM is paused, adopts that baseline, then resumes before repository publication. Subsequent ordinary memory layers contain the runtime-reported dirty ranges since the preceding successful CLEAR. Runtime conservative dirty pages are retained. Existing periodic compaction can produce a merged layer; complete snapshots still reference their parents.

GET/CLEAR errors, cancellation and ambiguous responses poison dirty tracking and prevent an unsafe resume. Publication failure after a successful CLEAR leaves the independently owned baseline usable. Dirty tracking disabled retains residency-based capture behavior. The snapshot record and layer formats are unchanged.

The read changes are confined to OverlayBD:

- Independent mapped fragments read into disjoint destination slices, with at most 32 in flight. All borrowed-buffer operations drain before propagating an error. The production read implementation has fewer lines than the original serial loop.
- Buffered contextual reads first try `preadv2(RWF_NOWAIT)`. A complete cache hit returns immediately. Positive partial hits retain their copied prefix and asynchronously read only the remaining tail. Misses and zero-length results use the original full-range asynchronous read. Unsupported operation is remembered per open file. This adds roughly forty production lines, one atomic flag and a focused real io-uring regression; no data cache, extra copy buffer or configuration.

The code audit also removed repeated disk-layer adoption and repeated parent hashing. Rootfs/drive sealed files have one live owner; captures obtain independent links or sparse copies. An index-rewrite prototype, coalescing prototype and 4 KiB-only NOWAIT alternative were measured and removed because they did not improve the relevant end-to-end behavior.

The five handwritten runtime files have a net increase of 201 lines before their independent `cfg(test)` modules: client +27, snapshot helpers +42, sandbox lifecycle +100, LocalFile +42 and mapped reads -10. Counts include comments and blank lines. The baseline ownership and failure state belong to the capture lifecycle; the reader changes stay in OverlayBD.

## Fixed programs and environment

Original upstream baseline: `05323abea960ab9348208d774c320d3782651544`. Frozen confirmation runtime code: `367b29fcf989fa7a0ffcc65564451f7be6b280a1`; all 21 changed files matched the source manifest before measurement. Later report/export changes do not alter those runtime binaries. Earlier results explicitly identify the preceding `9b99228` candidate.

| Program | SHA-256 |
| --- | --- |
| Original server | `0c169ed06aeb7b9505eceaf2a82f243de2ab764588996d7713231e3d2249573a` |
| Original daemon | `741c0b3fb814f2c371bbf838b1c129cc896e658520ff47868f422c91e5f4daf3` |
| Current server | `674a724a59f9520895dd3ae226f73e9302b9681750184abeb40fae09a3919b20` |
| Current daemon | `e345d97e3d777c4a347967aa2374d0a89f992006ced6e1c16c250f2cce1e814d` |
| Intel Firecracker | `b6b24891922968a785d7dde9a2d013ed5eef4390054af3bd81353a476188dbf7` |
| ARM Firecracker | `5c865a0cd9d149b37663937f4421e63243713c2247485dc257c15bcc719cece4` |

Both variants use Firecracker `736390868665d97230545915f371b9d65965a65d`, built without production source modifications. A CLEAR-capable runtime is required. The existing same-version binary cache can contain an older runtime; refresh it before trying this branch. Runtime release packaging is a prerequisite, not a tested deployment result of this PR.

Performance host: Ubuntu 24.04.4, kernel 7.0.0-30-generic, two Intel Xeon Gold 5317 processors, 48 logical CPUs, about 128 GiB RAM, ext4 on NVMe. ARM correctness host: Ubuntu 24.04.5, kernel 6.8.0-139-generic, Neoverse-N2, 128 logical CPUs, about 256 GiB RAM. Builds used independent Rust 1.99.1. Defaults, shared services and host kernel were preserved.

Guests use kernel 6.18, 2 vCPUs, envd 0.5.15, tools 0.1.1 and a prepared public Ubuntu Noble image. Performance covers POSIX raw memory layers. OSS correctness is checked separately. There is no blanket performance claim for every backend, compression mode or host.

## Protocol and unchanged acceptance

[Controlled comparison plan](#controlled-comparison-plan) defines the controls for subsequent whole-implementation and reader/layout studies. It preserves the original endpoints and gates, separates cache profiles, and distinguishes incremental export bytes from periodic compaction rewrites. No new trials were started while preparing it.

Trials run serially in randomized within-pair order. Each pair has fresh repositories, catalogs, API processes and runtime state, matching server/daemon builds, and identical immutable seeds. The source VM remains running during restore measurements. Input hashes and order are frozen before a batch. Setup, workload writes and checksum checks precede save timing; the first checksum after restore is itself a timed endpoint.

The tmpfs payload is half of guest RAM. Rotating/hotspot writes change 1% of RAM per generation; dense replaces the payload; idle performs no payload writes. Eight-generation cells cover 512 MiB idle/rotating/hotspot/dense and 4096 MiB rotating/dense. The long chain uses 512 MiB rotating for 96 generations.

Every original latency endpoint must have a one-sided 95% upper candidate/baseline ratio at most **1.05 for p50** and **1.10 for p95**. The analyzer resamples complete paired trials 10000 times, preserving generations from each chain. At least 30 independent pairs are required per cell. Primary space measures require an upper mean-ratio bound below one. All seven complete cells must pass; failed or interrupted trials are retained and invalidate their cell.

Targeted-cold mode flushes and evicts experiment snapshot data files and verifies latest memory-layer residency at most 5% with mincore. Process, metadata and index caches remain normal; this is not a globally cold host. No global cache drop is used.

The larger independent confirmation fixes pair counts at **120/60/60/30/30/30/30** in the cell order above. Short-latency cells get more samples because the preceding 30-pair study left wide tail confidence bounds. Previous failures remain separate; there is no selective exclusion or pooling. The confirmation uses default CPU/NUMA placement. A separate five-pair affinity experiment did not eliminate warm-read variation and is not used to replace the primary measurements.

## Earlier current-code 30-pair idle result

Candidate/original ratios below show point estimate followed by the one-sided 95% upper bound. This is the complete `367b29f` formal3 idle cell, separate from the completed larger confirmation.

| Endpoint | p50 ratio (upper) | p95 ratio (upper) | Original gate |
| --- | ---: | ---: | --- |
| Save API | 0.558 (0.603) | 0.494 (0.518) | Pass |
| Pause upper bound | 0.796 (0.833) | 0.831 (0.853) | Pass |
| Whole-chain save total | 0.515 (0.543) | 0.508 (0.522) | Pass |
| Whole-chain pause total | 0.791 (0.810) | 0.785 (0.806) | Pass |
| Warm restore ready | 1.028 (1.066) | 1.033 (1.091) | Fail |
| Warm first full read | 1.011 (1.068) | 1.027 (1.222) | Fail |
| Warm ready + read | 1.013 (1.033) | 1.077 (1.136) | Fail |
| Targeted-cold restore ready | 0.851 (0.886) | 0.802 (0.890) | Pass |
| Targeted-cold first full read | 0.962 (1.041) | 1.009 (1.123) | Fail |
| Targeted-cold ready + read | 0.891 (0.925) | 0.858 (0.897) | Pass |

All point estimates meet the margins; several confidence bounds do not. This establishes neither full noninferiority nor a failure-free full workload matrix. The other six formal3 cells were not run after this failure.

## Independent 120-pair idle confirmation

The predeclared larger idle cell completed all 120 pairs (240 trials), with all payload/deletion checks passing. Save/pause, every targeted-cold latency endpoint and primary space reduction passed. Three warm endpoints still fail their confidence gates. All point estimates remain inside the margins; this is not full acceptance. No additional replicas are being added to this cell after inspecting this result. Other cells retain their previously fixed counts; completed results are reported below.

| Endpoint | p50 ratio (upper) | p95 ratio (upper) | Original gate |
| --- | ---: | ---: | --- |
| Save API | 0.563 (0.609) | 0.478 (0.493) | Pass |
| Pause upper bound | 0.793 (0.827) | 0.823 (0.838) | Pass |
| Whole-chain save total | 0.508 (0.528) | 0.496 (0.505) | Pass |
| Whole-chain pause total | 0.791 (0.806) | 0.804 (0.813) | Pass |
| Warm restore ready | 1.036 (1.055) | 1.073 (1.108) | Fail |
| Warm first full read | 1.037 (1.098) | 1.016 (1.087) | Fail |
| Warm ready + read | 1.046 (1.065) | 1.056 (1.092) | Fail |
| Targeted-cold restore ready | 0.815 (0.837) | 0.797 (0.845) | Pass |
| Targeted-cold first full read | 0.982 (1.047) | 0.982 (1.035) | Pass |
| Targeted-cold ready + read | 0.889 (0.910) | 0.915 (0.926) | Pass |

Allocated growth mean ratio: 0.460 (upper 0.478); repository logical-byte ratio: 0.834 (upper 0.841). The retention boundary below still applies.

## Independent 60-pair rotating and hotspot confirmations

Both cells completed all 60 pairs (120 trials each), with all payload/deletion checks passing and primary space reduced. Every targeted-cold latency gate passed. Warm readiness still fails in both cells; hotspot also fails warm first-read tail and combined gates. Unlike the idle result, the hotspot warm-ready point estimate itself exceeds the 5% margin. No samples or gates were changed after seeing these results.

| Cell / endpoint | p50 ratio (upper) | p95 ratio (upper) | Original gate |
| --- | ---: | ---: | --- |
| Rotating: warm ready | 1.032 (1.062) | 1.076 (1.140) | Fail |
| Rotating: warm first full read | 0.963 (1.008) | 0.914 (1.001) | Pass |
| Rotating: warm ready + read | 0.983 (1.015) | 0.976 (1.041) | Pass |
| Hotspot: warm ready | 1.060 (1.086) | 1.028 (1.112) | Fail |
| Hotspot: warm first full read | 0.994 (1.036) | 1.080 (1.142) | Fail |
| Hotspot: warm ready + read | 1.020 (1.052) | 1.058 (1.137) | Fail |

Allocated growth mean ratios are 0.416 (upper 0.429) for rotating and 0.553 (upper 0.574) for hotspot. Repository logical-byte mean ratios are 0.718 (upper 0.726) and 0.832 (upper 0.842). These are retained-history results, with the same retention boundary below.

## Independent 30-pair dense confirmation

The 512 MiB dense cell completed all 30 pairs with payload/deletion checks passing. Primary allocated growth fell by about 8%: mean ratio 0.921 (upper 0.926). Repository logical-byte mean ratio was 0.932 (upper 0.937). Warm readiness failed with p50 1.064 (upper 1.094) and p95 1.135 (upper 1.209). Targeted-cold first-read p50 also failed its confidence gate: 1.021 (upper 1.099). Every other latency gate passed, including warm combined p50 0.996 (upper 1.029) and cold combined p50 0.990 (upper 1.023).

Latest-only retention remains a material boundary: after old-record deletion this dense cell references 3.657 times the original total managed-layer bytes including seeds, or 7.147 times the additional referenced bytes above seeds. Whole-history storage improvement does not establish latest-only storage improvement. The remaining cells are now complete; their results and endpoint audit appear below.

## Completed 4 GiB and 96-generation confirmations

Each remaining cell completed 30 independent pairs. Saved statistics were independently reproduced from all 30 complete paired measurements with the original 10000 whole-pair bootstrap. No timing was rerun or pooled with diagnostic studies.

| Cell | Allocated growth mean ratio (upper) | Original latency gates |
| --- | ---: | --- |
| 4096 MiB rotating, eight generations | 0.333 (0.345) | Fail: warm-ready p50 upper 1.055 |
| 4096 MiB dense, eight generations | 0.974 (0.976) | All pass |
| 512 MiB rotating, 96 generations | 0.125 (0.127) | Fail: warm ready, first-read p50 and combined p50 |

Latest-only referenced bytes remain larger: 4 GiB dense retains 6.661 times the original total bytes including seeds, or 7.762 times the additional bytes above seeds. The long chain retains 2.019/3.417 times respectively. These are retained references, not garbage-collection measurements.

## Control and endpoint audit

The 20.51% readiness increase compares **the same 96-generation workload in both implementations**, not 96 generations against eight. Each of 30 paired trials starts from the same 512 MiB seed, performs 96 rotating writes of about 1% of guest RAM, and restores generation 95 (zero-based) through the same public API driver. The source VM remains running. Every trial has all 96 saves and exactly one restore in each recorded cache mode; all paired payload checksums match. Baseline source is `05323abe`; candidate runtime is `367b29f`. Matching baseline/candidate server-daemon pairs are used, with identical pinned Firecracker, guest kernel, seed, driver and endpoint boundaries. Order is randomized within each pair.

| Endpoint, 30 observations per implementation | Original median ms | Candidate median ms | Ratio |
| --- | ---: | ---: | ---: |
| Post-save first restore: ready | 296.394 | 357.190 | 1.2051 |
| Post-save first restore: first checksum | 916.723 | 997.440 | 1.0880 |
| Post-save first restore: ready + checksum | 1228.790 | 1367.342 | 1.1128 |
| Targeted-cold restore: ready | 612.278 | 529.286 | 0.8645 |
| Targeted-cold restore: ready + checksum | 2668.396 | 2186.006 | 0.8192 |

Ratios divide the two sample medians; they are not medians of per-pair ratios. Combined medians are computed from each observation's combined duration, rather than by adding separate medians. Readiness spans the POST request through repository resolution, VM setup and envd readiness until the public launch response returns. The first checksum is a separate API/guest-command measurement after that response.

The historical raw label `warm` means **first restore after saving, without explicit eviction**. It does not establish equal or complete page residency. There was no complete untimed restore before that endpoint, and input memory-file residency was not recorded before it. Consequently this is a whole-implementation, post-save behavior comparison. It cannot by itself isolate a reader regression or establish a fully cached comparison. The targeted-cold endpoint separately evicts snapshot files and verifies memory-layer residency at most 5%; metadata/process caches remain normal. Both original acceptance margins remain unchanged.

Generation count and layer count are distinct. The retained first pair has two original memory layers versus 33 candidate layers after 96 captures. The original `snapshot_to_dir` always inherits the immutable launch snapshot: without CLEAR, its newly exported layer contains cumulative dirty pages since launch, so one base plus one cumulative layer restores the final state. The candidate instead inherits `memory_baseline`, updated after each successful CLEAR; each new export contains changes since that CLEAR and needs the preceding chain. The existing suffix budget is 32 layers with this one-layer stable base, giving up to 33 total layers before the next suffix compaction. That budget was already present upstream; 33 is not a CLEAR API requirement. This is an output of cumulative versus incremental storage and compaction, not a different generation-count input. Only the first pair's repositories were retained, so this measured layer count is not claimed for all 30 pairs. More layers can add file-open, index and fragmented-read work; the observed latency ratio does not prove which component caused it. The two separately running guests also have independent CPU/device/OS state despite identical seeds and payload checksums.

Host load and placement were not isolated: the protocol uses default CPU/NUMA placement on the shared Intel host. Randomized pairing reduces order bias but does not guarantee identical cache or scheduling conditions. The earlier fixed-byte, explicitly prewarmed layout controls and ARM controls with a common debug API server answer different questions; their times are not pooled with this primary matrix or substituted for its endpoints.

The frozen analyzer differs from the later published analyzer only by the addition of exported `paired_measurements`. Its quantile and bootstrap function ASTs are identical; all saved statistics were reproduced. [Frozen protocol, analyzer and audit](formal4-control-audit.json.gz) preserve this distinction. No new performance experiment was started during this audit.

A read-only post-hoc audit of all 60 long-chain trials matched the source VM and three restores in each trial's existing Firecracker logs, yielding 120 measured restore event spans. The median first timestamped Firecracker file event to resume completion was 13.386 ms original versus 13.607 ms candidate for the post-save restore. Per-observation readiness minus that span had medians 283.515/343.304 ms. API absolute start/end timestamps were not recorded, so this remainder combines work before the first logged event and after resume; it cannot locate the cause. Log spans use wall time while the original endpoint uses monotonic duration, and are diagnostic only.

The source's initial-resume-to-final-capture-pause interval had medians 154.732/56.805 seconds. This includes earlier capture pauses and is not guest CPU time. It confirms that equal generation counts do not equalize the source's elapsed history. That difference is an outcome of the whole workflow, not proof of a reader defect. A fixed complete VM-state input is still necessary for attribution. [The audit bundle](formal4-control-audit.json.gz) includes every derived row and the derivation source; original numeric measurements, analysis and acceptance gates are unchanged.

Subsequent input/build preparation verified all 536870912 logical memory bytes between the retained 33-layer image and a flattened single layer, preserving the saved VM-state files. Two reader builds share 872 source files with exactly two differences (local.rs, readonly.rs), matching compiler settings, release profiles and io-uring features. The initial shared-target build attempt copied identical binaries and was rejected before any trial. Source timestamp refresh plus Cargo JSON checks required the library and executable to be rebuilt and supplied the actual executable paths. The accepted binary hashes reproduce the original frozen formal4 baseline/current readers exactly. All attempts, build manifests and preparation sources are retained in the existing audit bundle. Those preparation steps did not launch VMs or performance trials. Subsequent untimed runtime checks are recorded below.

Eight subsequent **untimed** direct-target checks passed: two readers x two layouts x two cache profiles. Actual API/daemon/Firecracker processes ran as ubuntu and matched the frozen executable hashes. Both layouts restored the same payload checksum with both readers. Memory-file residency immediately before target open was 100% for resident cases and 0% for targeted-cold cases; captured owned processes had terminated after cleanup. Every node used fresh state and explicitly disabled network, block and Firecracker process pools. The initial check failed before restore because a test-only work path exceeded the Unix socket path limit; it is retained, and the corrected attempt used short task-specific paths. These are correctness/control checks, not timing results or a replacement for the six failed formal latency cells.

The subsequent unified driver used the plan's identical untimed seed launch/delete in every fresh node to initialize the daemon/tools path while keeping the target's first open. All eight exact-driver preflight cases passed. The 64 GiB raw tools file was hashed before and after each full batch, rather than reread completely before every observation. The completed prospective reader/layout cohort follows below.

## Prospective fixed-state reader/layout diagnosis

This separately declared B cohort completed **30 randomized complete blocks per cache profile, 240 observations total**, with no failed, retried or excluded observations. Both readers restore the same saved 512 MiB VM state with byte-equivalent 33-layer and flattened single-layer memory. The common API server is the current `674a724a...` binary; baseline/current daemons are `741c0b3f...`/`e345d97e...`. This isolates reader/layout factors and does not replace the whole-implementation A study.

Each observation has a fresh node/API/daemon and the same untimed seed prelude. Network, block and Firecracker pools are disabled. Rootfs/state/kernel/program file preparation matches; memory-file residency immediately before first target open was 100% for every resident observation and 0% for every targeted-cold observation. Every actual process hash/UID, canonical configuration, payload checksum, clock ordering and cleanup check passed. The frozen protocol, complete rows, eight exact-driver preflights, supporting imported sources and analysis are in [the existing audit bundle](formal4-control-audit.json.gz), under `fixed_state_reader_layout_20261108`.

| Cache profile | Reader | Memory layout | Ready median ms | First payload checksum median ms | Observed ready + checksum median ms |
| --- | --- | --- | ---: | ---: | ---: |
| resident | baseline-reader | 33 layers | 368.022 | 880.662 | 1244.693 |
| resident | baseline-reader | 1 layer | 315.445 | 916.585 | 1239.907 |
| resident | current-reader | 33 layers | 353.352 | 852.283 | 1193.351 |
| resident | current-reader | 1 layer | 291.920 | 853.041 | 1136.435 |
| targeted-cold | baseline-reader | 33 layers | 1280.328 | 2002.895 | 3268.512 |
| targeted-cold | baseline-reader | 1 layer | 687.595 | 1714.356 | 2388.452 |
| targeted-cold | current-reader | 33 layers | 572.144 | 1663.832 | 2230.063 |
| targeted-cold | current-reader | 1 layer | 539.142 | 1555.682 | 2086.176 |

The reader changes help on the same 33-layer input: current/baseline readiness p50 ratio is 0.960 (one-sided 95% upper 0.998) for resident files and 0.447 (upper 0.466) for targeted-cold files. Corresponding observed combined p50 ratios are 0.959 (upper 0.981) and 0.682 (upper 0.703).

A material layout cost remains. With the current reader, 33-layer/single-layer resident readiness is **1.210** (p50 upper 1.274; p95 ratio 1.193, upper 1.227), while the first checksum p50 ratio is 0.999 (upper 1.042). The resident combined p50 ratio is 1.050 (upper 1.085). Targeted-cold current-reader layout ratios are 1.061 for readiness (upper 1.099) and 1.069 for combined time (upper 1.102). These are fixed-state diagnostic ratios, not a new passing original acceptance result.

Clock-aligned post-hoc event diagnostics locate most resident layout overhead before the first timestamped Firecracker file event: current-reader component medians are 191.659/131.620 ms (33/1 layers), 13.269/12.489 ms from that first event through resume completion, and 149.596/146.998 ms from resume through the public response. The first file event is **not process spawn**; source order places memory-device acquisition before logger configuration, so this first interval includes image/device preparation. These boundaries do not identify the responsible function. Component medians do not sum to the total median. Maximum wall-versus-monotonic discrepancy was 0.003108 ms and maximum alignment-bracket width 10.618 microseconds.

Six subsequent, separately declared resident-input probes used the unchanged current programs and read existing API-server histogram counters around the first target create. Each target added exactly one `create_raw_overlaybd` observation. Memory-device creation RPC medians were **128.591 ms for 33 layers versus 74.802 ms for one layer** (three fresh nodes/layout). Every payload, cache and actual-process check passed. These diagnostic probes are excluded from the 240-observation cohort and all acceptance calculations. They narrow the remaining cost to device creation, which includes image/index preparation and ublk registration, but do not establish the responsible subfunction. Protocol, source, all six counter deltas and complete cleanup markers are retained under `fixed_state_preparation_metrics_20261108` in the existing audit bundle.

Six further, separately declared CPU-sampling probes attached only to the verified frozen current daemon (`cpu-clock:u`, 1999 Hz, DWARF stacks). Before the first Firecracker file event, the three 33-layer windows contained 76/80/81 user samples, of which 58/57/53 (76.3%/71.3%/65.4%) were self samples in `SegmentMapping` tree operations, `MutableIndex::insert` or `ReadOnlyIndex::merge`. The single-layer windows had 23/22/19 samples, including 12/14/14 in those operations. Every window had zero lost samples. This implicates readonly index preparation as a CPU hotspot; sample fractions are not wall-time fractions, and the window also includes common rootfs preparation. Raw stack data remains private. Source, protocol, aggregate symbols and all six probes are retained under `fixed_state_preparation_cpu_20261108`. The initial wrapper incorrectly rejected its deliberate SIGINT stop; that failed attempt is retained separately, and the corrected six probes use readable-file validation and explicit control acknowledgements. These samples are excluded from B and A.

All 24 contrast/endpoint statistics were independently reproduced from raw rows using the frozen original quantile/bootstrap functions and 10000 whole-block resamples, within 1e-12 (maximum absolute difference 1.11e-15). All primary formal4 numeric files remain unchanged; six of seven original latency cells still fail.

A subsequent one-file candidate replaced `ReadOnlyIndex::merge` tree replay with ordered vector scans. This separate frozen study used the current tree reader (`e345d97e...`) versus candidate (`7fdf6430...`), the same 33-layer and flattened complete VM state, the same common API/Firecracker, and 30 randomized complete eight-cell blocks (240 observations). The compiler, features and profiles match; exactly one of 872 source files differs. Eight exact-driver preflight checks, 409 OverlayBD unit tests (four ignored), package Clippy and an independent full 512 MiB memory oracle passed. All 240 payload/configuration/process/cache/cleanup checks passed, with 100% resident or 0% cold memory pages; no observation was retried or excluded.

| Index-candidate study: readiness median (ms) | Current tree | Vector candidate | Candidate/current p50 ratio (one-sided upper) |
| --- | ---: | ---: | ---: |
| Resident, 33 layers | 354.214 | 352.956 | 0.996 (1.036) |
| Resident, one layer | 322.171 | 299.177 | 0.929 (0.972) |
| Targeted-cold, 33 layers | 571.608 | 570.707 | 0.998 (1.015) |
| Targeted-cold, one layer | 544.385 | 533.337 | 0.980 (1.037) |

The candidate does not demonstrate a useful readiness improvement on the 33-layer target. It was removed from the code branch; its patch, build receipt, protocol, all raw observations and all 24 contrast/endpoint calculations remain in `fixed_state_linear_index_comparison_20261108`. Independent recomputation agrees within 1e-12 (maximum difference 5.55e-16). The CPU hotspot alone did not establish that this rewrite would reduce end-to-end latency. Single-layer gains do not resolve the long-chain regression. These results are separate from B and A; the original six failed latency cells remain unchanged.

Six subsequent resident-input probes use an isolated current-reader build with timing spans in three existing modules; those markers are excluded from product code. Actual program identities, payloads and cleanup pass for all six. Each target has distinct rootfs and memory-device spans; the following table uses the memory spans only (three observations/layout). These instrumented values are diagnostic, with no confidence or acceptance claim. Component medians do not sum to the total median.

| Preparation stage, median ms | 33 layers | One layer |
| --- | ---: | ---: |
| Open lower files and adapt formats | 17.463 | 0.244 |
| Load indexes after metadata | 4.147 | 1.003 |
| Merge readonly indexes | 33.476 | 7.651 |
| Complete image open | 56.270 | 9.206 |
| Build ublk device | 27.224 | 27.223 |
| Start ublk device | 12.957 | 11.827 |
| Wait until device is readable | 20.597 | 20.584 |

A separate CPU microbenchmark loads the same retained input once, outside timing: 53,100 valid input mappings produce 11,691 output mappings. In 31 alternating runs, original tree versus sequential-vector merge medians are 15.245/6.980 ms. A later three-way, rotated-order microbenchmark has medians 14.700/6.981/2.670 ms for tree/sequential-vector/balanced-pair merge, with exact mapping equality in every run. Its synthetic cases also match. These warmed function measurements use different process/allocation/cache conditions from daemon preparation and cannot replace those spans or predict readiness gains. Full sources, patches, all values and build identities are retained in `fixed_state_preparation_spans_20261108` and `fixed_state_real_index_micro_20261108`. The diagnostics motivate narrower source work; no new candidate is retained on this evidence alone.

A further isolated candidate changes two existing reader files: balanced-pair readonly index merging in `lsmt/index.rs`, and reuse of the existing NOWAIT decision for ordinary cached reads in `backend/local.rs`. Reference mapping tests, the explicit cached/fallback/EOF test, OverlayBD units and package Clippy passed. All 512 MiB still match the independent original memory digest, and the flattened blob digest is unchanged. Its same-driver eight-case preflight and separate 240-observation cohort completed without failures, retries or exclusions. Every payload/configuration/process/cache/cleanup check passed; resident/cold memory pages were 100%/0%. The code PR is unchanged.

| Two-file candidate: medians (ms) | Current ready | Candidate ready | Current first read | Candidate first read |
| --- | ---: | ---: | ---: | ---: |
| Resident, 33 layers | 358.539 | 331.635 | 855.065 | 859.317 |
| Resident, one layer | 298.298 | 295.948 | 847.789 | 1094.749 |
| Targeted-cold, 33 layers | 589.661 | 555.802 | 1694.146 | 1667.883 |
| Targeted-cold, one layer | 572.903 | 512.962 | 1867.348 | 1511.341 |

The 33-layer readiness p50 ratio is 0.925 (one-sided upper 0.957) resident and 0.943 (upper 0.982) cold. However, resident single-layer first-read noninferiority fails: p50 ratio 1.291 (upper 1.360), mean ratio 1.091 (upper 1.159). All raw observations remain included; their substantial variation has no established cause. The candidate remains isolated pending diagnosis and is not presented as an accepted implementation. A separate task-file capability probe confirms an 8192-byte NOWAIT cache hit returns correct data and a cold read returns EAGAIN; that probe is not a trace of daemon fast-path execution. The complete sources, two-file patch, immutable program identities, all observations and independently reproduced 24 contrast statistics (maximum difference 8.88e-16) are retained under `fixed_state_preparation_candidate_20261108`. The original seven-cell acceptance remains unchanged and unmet.

A separate 30-pair resident-flat CPU diagnostic retains 60 complete observations, using the same programs and checksum. All identity/cache/payload/cleanup checks pass; both vCPU threads have stable identities throughout each counter interval, and daemon/server thread sets are stable. Median running CPU time is 740.614/722.754 ms across the two vCPUs and 334.862/333.159 ms in the current/candidate daemons. Across all 60 observations, wall-time correlation with vCPU running time is 0.981 and with daemon running time 0.953; these descriptive associations do not establish the cause. Counter-read brackets add a median 21.629 ms, and process verification adds an unmeasured delay after readiness. This perturbed cohort's first-read medians (912.741/883.084 ms) cannot replace the uninstrumented result above.

The [kernel scheduler-stat documentation](https://www.kernel.org/doc/html/latest/scheduler/sched-stats.html) distinguishes running CPU time from runqueue waiting. Collection of the latter was disabled when checked after the batch; zero wait fields are excluded from interpretation. User ticks already include guest time, so the derivation does not add guest ticks again ([Linux proc_pid_stat documentation](https://www.kernel.org/pub/linux/docs/man-pages/book/man-pages-6.10.pdf)). Endpoint CPU IDs and whole-VM NUMA page counts also do not identify hot-page locality or intermediate migrations. Sources, all 60 raw observations, coverage checks and the reproducible derivation are retained under `fixed_state_first_read_cpu_20261108`. No host scheduling settings were changed, and the resident-flat noninferiority failure remains outstanding.

## First-read work counters, separate diagnostic cohort

The completed PMU study contains 30 randomized resident-flat pairs (60 fresh nodes), comparing current reader `e345d97` with the isolated two-file preparation candidate `e0542ee`. It uses the same saved VM state, common API server `674a724`, Firecracker, two vCPUs, payload and verified resident memory input. All 60 checksum, program/UID, coverage and cleanup checks pass; source/protocol hashes remain unchanged. A separate two-observation instrumentation preflight passed before the cohort. No observations were excluded or repeated.

| Instrumented work median | Current reader | Preparation candidate |
| --- | ---: | ---: |
| Guest instructions, both vCPU threads | 941,962,357 | 939,379,165.5 |
| Guest cycles, both vCPU threads | 1,149,960,429 | 1,166,659,968.5 |
| Reader instructions, all stable threads | 128,908,595 | 130,157,742 |
| Reader cycles, all stable threads | 306,259,715.5 | 304,892,526.5 |

`cycles:G,instructions:G` count guest execution on the two owned vCPU threads; `:H` counts the owned reader's host execution. Events start disabled, with enable/disable acknowledgements around the checksum. Active counters require >=99% coverage; inactive reader threads have zero runtime and remain included as zero. The interpretation follows Linux v6.8 [event modifiers](https://github.com/torvalds/linux/blob/v6.8/tools/perf/Documentation/perf-list.txt) and [perf-stat controls](https://github.com/torvalds/linux/blob/v6.8/tools/perf/Documentation/perf-stat.txt).

These are perturbed observations: counter windows include control overhead and vCPU counts include guest background work. Similar median work does not establish a cause of elapsed-time variation, measure CPU frequency, or waive the previously failed uninstrumented resident-flat gate. The cohort is not pooled with any timing or acceptance study. All raw numeric observations, native CSV strings, preflight, analysis and source are in `formal4-control-audit.json.gz` under `fixed_state_first_read_pmu_20261108`. Independent integer CSV accumulation reproduces all 60 work totals and all 48 work summary values. No guest memory or sampled stack bytes are published.

## Single-file memory suffix candidate, correctness only

A separate candidate extracted from code-review head `955be00` changes only the existing `src/sandbox/firecracker/overlaybd_snapshot.rs` (872-file source manifest, identical file set). It passes a memory-specific suffix budget of four into the existing compaction helper; the disk budget remains 32 and stable parents remain intact. The current E345 reader is unchanged. More frequent compaction may trade restore preparation for additional save/rewrite work; no performance benefit is claimed yet. The policy was fixed before these checks and has not been applied to code PR #5.

Rust 1.99.1 checks pass: 17 snapshot-module unit tests (one cross-filesystem test ignored), workspace format check, all-target/all-feature workspace Clippy with warnings denied, and a release server build (`1b2ea712`). Existing tests now assert the exact four-layer suffix cycle through 96 unit generations and the stable-parent case. Native Linux checks pass using frozen E345 daemon and the same CLEAR runtime: complete byte comparisons for four generations at both 512 MiB and 4 GiB; CLEAR rejection/lost-response/timeout; and 96 consecutive captures followed by all 96 restores and a final restore after deleting the preceding 95 capture directories. The native 96-generation test completed with zero failures and no owned process survivors.

Two preparation failures are retained separately: the first isolated unit environment lacked the storage-service configuration; the first integration attempt lacked the image-conversion configuration before entering the capture loop. The corrected environments use the same frozen test binaries and unchanged test bodies. This integration correctness test uses its existing registry-image setup, so its elapsed time is not performance evidence. No acceptance cohort has started for this candidate. ARM and the original seven workload cells remain outstanding, with counts and gates unchanged. The single-file patch, source/build identities, harness sources and native receipts are embedded under `memory_layer_budget_candidate_20261109` in the existing audit archive.

## Memory suffix policy pilot, no acceptance inference

This completed pilot compares **core `955be00` with suffix 32** against the isolated **suffix 4** patch, with the same frozen E345 reader. The control is not original upstream `05323`. Both servers were freshly built from the same 872-file archive with identical compiler, lockfile, features and profiles; exactly one existing source file differs. The pilot was fixed at one randomized pair in each of three workloads. All six complete observations, expected generation counts, matching payload checksums, canonical configurations, actual process UID/hash/path checks and targeted-cold residency <=5% passed. Final program/script and shared 64 GiB tools hashes are unchanged; no observations were excluded or retried.

The initial attempt stopped before workload setup because its verifier incorrectly required a single Firecracker process; it contains zero save/restore samples and is retained. An untimed two-group audit found one configured two-vCPU VM plus four preboot pool processes in each group, all verified and cleaned up. Both original `05323` and core `955be00` have the same enabled Firecracker-pool defaults. The original whole-implementation runner disables network/block pools but omits `pool.firecracker`; its historical live pool occupancy was not recorded. The new pilot preserves that configuration and strictly verifies the same one-running/four-preboot state in both groups before workload setup. This differs from the explicitly pool-disabled fixed-state reader studies; their data remain separate.

The workload, timing and space source is byte-identical to frozen `clear_api_trial.py` (`f65c9b4`). The new runner adds only pre-workload process verification and post-run metadata checks. Analysis reuses the original frozen metric-extraction body and quantile function (`2c5892f`); an independent raw-row reconstruction matches all 252 summary values. Values below are descriptive: each workload has only one independent pair, so there are no confidence intervals or acceptance decisions. Columns show suffix 32 control / suffix 4 candidate.

| Workload | Final memory layers | Chain save seconds | Pause upper p95 ms | Post-save ready ms | Candidate/control repository bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| 512 MiB idle, 8 captures | 9 / 5 | 0.629 / 0.681 | 42.728 / 61.592 | 322.812 / 266.461 | 1.064 |
| 512 MiB rotating, 96 captures | 33 / 5 | 14.669 / 26.726 | 89.968 / 420.192 | 370.617 / 316.863 | 2.516 |
| 4096 MiB dense, 8 captures | 9 / 5 | 37.903 / 40.705 | 1977.839 / 3475.250 | 407.394 / 417.404 | 1.014 |

The long input has the predicted 33/5 physical layers. Its lower readiness accompanies substantially more compaction: chain save time increases from 14.669 to 26.726 seconds and pause upper p95 from 89.968 to 420.192 ms; repository bytes are 2.516 times the suffix 32 control. The four-layer policy is therefore not retained as-is. Source inspection confirms memory compaction currently runs before CLEAR/resume; moving work on immutable memory files after resume is a prospective implementation change requiring fresh correctness and performance checks. This pilot does not establish space or latency relative to the original implementation, replace any failed formal4 endpoint, or change the original seven-cell gates. All raw numbers, build/source manifests, configurations, zero-sample failure, pool preflight and analysis source are embedded under `memory_suffix_policy_pilot_20261109` in the existing audit archive. Code PR #5 remains unchanged.

## Post-resume memory finalization candidate, correctness coverage

A follow-up candidate keeps the four-layer memory suffix policy and moves ordinary capture's immutable-file compaction after source resume. Paused export still retains a complete private baseline before CLEAR. Finalization compacts owned files, gives the unpublished capture its own link/copy, calculates descriptors and atomically writes both image configurations before publication. Both representations describe the same cleared checkpoint. Explicit pause and dirty-tracking-disabled capture retain synchronous compaction; snapshot format and external APIs remain unchanged.

The isolated archive differs from core `955be00` in exactly two existing runtime files (`sandbox.rs`, `overlaybd_snapshot.rs`) and the existing snapshot integration file, with no added source paths. Server SHA-256 is `92c63245665610ec5880fce6bbfaf3a6982383cfd388a107f321fed51a438820`; reader E345 is unchanged. Code PR #5 has not adopted this candidate.

Rust 1.99.1 format checks, workspace all-target/all-feature Clippy, release build and discovery of all five integration regressions pass. Snapshot units pass 18 tests with one cross-filesystem test ignored. Both synchronous and deferred paths reconstruct a synthetic 64 KiB memory image through 96 unit generations, with zero or one stable parent; an injected compaction failure preserves the old owned configurations. Native Intel POSIX checks pass: complete byte comparisons across eight generations at each of 512 MiB and 4 GiB (16 comparisons); CLEAR rejection/lost-response/timeout; and all five incrementality regressions. The 96-generation integration now uses ordinary capture/finalization, checks its RAM-file witness at every restore, then restores the last after deleting the preceding 95 capture directories. Fresh-cache/source-deletion coverage now spans eight captures, and publication failure follows a compaction boundary. Fork/encoded-pause coverage includes dirty tracking both enabled and disabled. All owned native processes exited.

The existing audit archive stores the final patch, source/build identities, command receipts and native summaries under `post_resume_memory_candidate_20261109`. ARM, OSS and original-implementation performance acceptance remain outstanding for this candidate. These correctness checks make no timing claim and do not change the earlier failed acceptance cells.

## Original versus post-resume candidate pilot, no acceptance inference

A separate pilot compares the original upstream implementation (server `0c169ed`, reader `741c0b3`) with the post-resume candidate (server `92c6324`, reader `e345d97`). This is a whole-implementation comparison, distinct from the preceding policy-only suffix32/suffix4 comparison. It uses the same original workload and timing/space definitions, kernel, Firecracker, seed and tools inputs, configuration and process-pool precondition. Both server builds identify Rust 1.99.1. All six observations complete; actual process UID/hash/path, checksums, canonical configurations, cold residency, cleanup and final input/tool hashes pass. Each workload has exactly one independent pair, without retries, exclusions, confidence intervals or acceptance inference.

Columns show original / post-resume candidate. Readiness is the first restore after saving without explicit eviction; it is not guaranteed fully resident.

| Workload | Memory layers | Chain save seconds | Post-save ready ms | First checksum ms | Candidate/original repository bytes | Candidate/original allocated state growth |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 512 MiB idle, 8 captures | 2 / 5 | 1.104 / 0.839 | 276.601 / 346.512 | 251.355 / 276.682 | 0.967 | 0.967 |
| 512 MiB rotating, 96 captures | 2 / 5 | 123.293 / 27.913 | 303.700 / 327.840 | 1013.369 / 1116.909 | 0.342 | 0.355 |
| 4096 MiB dense, 8 captures | 2 / 5 | 69.088 / 39.934 | 295.758 / 267.158 | 5300.422 / 4291.532 | 0.972 | 1.092 |

The candidate has lower chain-save time in these observations, but idle/long-chain post-save readiness increases 25.3%/7.9%, long-chain first checksum increases 10.2%, and dense allocated state growth increases from 18,339,020,800 to 20,026,798,080 bytes. It is not accepted or adopted in code PR #5. Code inspection finds superseded unpublished raw-export links retained after compaction; attributing the measured excess bytes requires an ownership/allocation check or regression, not subtraction alone. No causal explanation for the remaining readiness/read cost is established. All 252 descriptive statistics and 126 ratios independently reconstruct exactly from raw observations. Sources, declaration, complete rows, actual configurations and validation receipts are embedded under `post_resume_original_pilot_20261109`. The original seven-cell acceptance counts and gates remain unchanged; earlier six failed timing cells remain failed.

## Superseded raw-export ownership regression

An added assertion in the existing 96-generation unit test fails on the preceding implementation: compaction replaces the newest raw export in the image configuration but leaves its owned path on disk. The isolated post-resume candidate also retains the private hard link to that export. The correction removes only owned superseded raw paths. Synchronous capture removes the raw export after successful compaction; deferred finalization removes its two owned paths only after both configurations are written and the blocking worker has returned. A cancelled worker therefore cannot unlink paths used by a following capture. Failed compaction preserves the old configurations and raw paths. This is targeted ownership cleanup, not blob garbage collection or a change to the layer format.

The suffix4/post-resume candidate with this correction changes one existing file relative to its preceding isolated source; relative to core `955be00`, the same three existing paths differ. ARM snapshot units pass 18 tests with one cross-filesystem test ignored, and the exact new assertion fails before the correction. Native ARM checks pass two complete-memory tests (eight generations at each of 512 MiB and 4 GiB), CLEAR faults, and all five incremental integrations including 96 captures/restores and capture cleanup. Actual observed test/daemon/Firecracker identities match frozen hashes under the ordinary test UID; captured owned processes exit. These runs use isolated correctness state with the Firecracker pool disabled, and are not timing evidence.

Production workspace Clippy passes with warnings denied. All-target/all-feature ARM Clippy encounters an unchanged `config.rs` test import used only on x86; the original strict failure is retained, the file matches the frozen baseline, and an explicit `unused-imports` exception allows the remaining checks to complete. No unrelated source file is changed. Formatting, integration discovery and release build pass. An initial image-cache-copy callback error occurs before any test or VM starts and is retained separately.

The patch, before/after regression records, source manifests, build commands, native summaries, identity checks and tool sources are stored under `owned_memory_cleanup_candidate_20261109` in the existing audit archive. Complete logs and guest memory stay on the task host. Attribution of the earlier pilot's exact excess allocated bytes and original performance acceptance remain outstanding; correctness of this cleanup does not establish either.

## Minimal cleanup retained in the code review

[Code commit `8b330eb`](https://github.com/Hygge-Gezelligheid/AgentENV-open/commit/8b330eb0d0669484a3dac322366e362ee78a0131) extracts only the synchronous owned raw-export cleanup into code PR #5. The original suffix32 policy and paused capture sequence are unchanged; the four-layer and post-resume policies remain isolated. The production change is ten lines in the existing memory helper. An assertion extends its existing 96-generation unit regression, and the existing native full-memory helper now runs 36 generations per memory size to cross the 32-layer suffix boundary after its generation-one restart. It requires a real compaction and checks that the superseded export is absent. The commit touches only two already-modified files; the complete code PR still changes nine existing paths and adds no Rust or Markdown files.

All 872 local source files match the exact ARM build/test manifest. Before the production correction, the new 96-generation assertion fails. After correction, all 17 snapshot units pass with one cross-filesystem test ignored. Formatting, production workspace Clippy, all-target/all-feature ARM Clippy with the previously documented baseline import exception, and discovery of all five integrations pass. Frozen native programs pass 72 complete byte comparisons (36 at each of 512 MiB and 4 GiB), CLEAR faults and all five integrations including 96 restores and deletion of earlier captures. Captured owned processes exit and program/source hashes remain unchanged. Results and exact patch are in `core_owned_export_cleanup_20261109` in the existing audit archive.

Intel is unavailable during this follow-up. Its earlier correctness and original performance results remain historical evidence for their frozen source; they are not relabeled as executions of the new commit. Original latency acceptance and exact allocated-byte attribution remain unestablished, and no new confirmation cohort is run. This commit resolves a directly reproduced file-lifetime defect while leaving the original acceptance requirements open.

## Immutable-index coalescing, exact controls

An isolated candidate adds one call to the existing `compress_raw_index` after zero normalization in `ReadOnlyIndex::new`. It merges adjacent logical/physical ranges from the same layer once when building the immutable index. Mutable index replay, file format and ordered layer references are unchanged. The constructor is shared by single-file opening, multi-layer merging and premerged artifact loading, avoiding repeated work in every read. This touches one existing index module and extends the existing storage test file; it adds no Rust or Markdown file.

The new observable regression fails before the call: four contiguous mappings cause four underlying reads instead of one. After the call, contiguous data uses one read; physical gaps, logical holes, different layer tags and explicit zero extents retain the required two reads. All five cases also verify contents, partial ranges and EOF destination tails. The complete OverlayBD suite passes 409 tests with four ignored; the explicit io-uring cached-read/fallback check, formatting and strict crate all-target/all-feature Clippy pass. New frozen native programs with the candidate reader pass 72 complete memory comparisons across real compaction boundaries, CLEAR faults and all five integrations including 96 captures/restores and earlier-capture deletion.

Both ARM release readers are built in the **same source directory**, before and after the one-line production change, with identical Rust 1.99.1, flags, features (`default`, `full`, `io-uring`) and profiles (opt3, debug0). The control is the current code review's reader at `8b330eb`, not original upstream `05323`. Control/candidate hashes are `b1f4d67a2073ac89509071c60abe9a73317a92d22105409c001c5fce514f2c79` / `0c4f80d041d5ce7764eb2c28f325121afbfd541f102451488850aaff94dba1cd`.

A single ARM rotating/96-capture fixture has 33 memory layers. A reference-library tool checks every 536,870,912 logical byte against its flattened representation; both have virtual SHA-256 `3f6cc95f6fc502ff3463c986a3cbe8afe1e127532e23dc74cbcb5c6209106878`. The flat blob is 347,570,176 bytes with digest `sha256:c9b9307a09ef94b167d31cf42cb01bb5bc9bf5148fdcdd9406d6a593eee24f61`. The same snapshot ID, full VM state and all other record fields are preserved; only `committed.memory_layers` changes. All 217 other nonmanaged repository files are byte-identical. Structural counts show the existing compacted lower can reduce from 3210 to 539 mappings, the merged index from 4065 to 3755, and the flattened index from 3755 to 71. These are structural observations, not latency estimates. Fixture preparation timings are excluded from comparisons.

The same frozen driver first passes all eight reader/layout/cache combinations without recording latency endpoints. Actual process UID/hash/path, one configured two-vCPU VM, canonical configurations, payload, cleanup and before/after input hashes (including the 64 GiB tools image) match. All three pools are explicitly disabled in both groups. Resident trials perform the same untimed target restore/checksum prelude, then read every target memory-file byte and verify 100% residency. Cold trials perform that same prelude, then use the existing targeted eviction and verify memory residency at most 5%. Every measured case uses a fresh node. Program-identity checks occur outside the create-response and first-checksum clocks.

The prospective comparison is fixed at 30 complete randomized eight-cell blocks (two readers, two layouts, two cache profiles), 240 measured restores, seed `2026110902`. It uses one common debug API server, the same kernel/Firecracker/dependencies and identical saved state. Legacy ARM helper hashes are recorded explicitly; they differ from formal4 helpers and are not pooled with them. Original frozen quantile and paired whole-block bootstrap functions are reused with 10000 resamples. The study attributes reader/layout effects on ARM; it cannot replace original seven-cell confirmation or establish whole-lifecycle space/pause/save performance.

All 240 observations finish with every cache/configuration/program/payload/cleanup check passing and all final input hashes unchanged. No observation is retried or excluded. An independent parser reconstructs all 24 reader/layout contrasts and 288 point/confidence fields within `1e-12` (maximum absolute error `4.44e-16`). Values below compare the current reader against the one-line candidate on the **same fixed ARM input**; they are not original-versus-CLEAR confirmation values.

| Layout/cache | Ready median ms, control / candidate | First checksum median ms, control / candidate | First-checksum p50 ratio upper | First-checksum p95 ratio upper |
| --- | ---: | ---: | ---: | ---: |
| 33 layers, resident | 269.879 / 265.045 | 380.876 / 377.102 | 1.0072 | 1.0198 |
| 33 layers, targeted cold | 946.799 / 935.037 | 1180.678 / 1174.387 | 1.0095 | 1.0232 |
| One layer, resident | 276.184 / 268.512 | 396.216 / 392.277 | 1.0061 | 1.0244 |
| One layer, targeted cold | 577.102 / 566.232 | 482.504 / 496.607 | 1.0437 | **1.1083** |

Eleven of twelve reader/endpoint contrasts meet the diagnostic 5%/10% upper-ratio gates. Cold one-layer first checksum does not establish tail noninferiority: its p50 point ratio is 1.0292, p95 point ratio 1.0252, and p95 one-sided 95% upper ratio 1.1083 exceeds 1.10. The upper bound is not an observed 10.8% slowdown. Cold one-layer combined median is 1063.691 / 1060.614 ms, but that cannot waive a separate first-checksum endpoint. Readiness improves modestly; the candidate is not adopted in code PR #5, which remains nine existing paths at `8b330eb`. No gates or sample counts are changed and no replacement cohort is run. Original confirmation remains unmet.

After the frozen cohort, the different-layer test is strengthened so its numeric physical offsets are contiguous across the layer boundary; the tag is then independently necessary to prevent merging. The strengthened test, 17 snapshot units, formatting and workspace checks pass, with the existing ARM test-import exception stated earlier. Rebuilt reader and native programs are byte-identical to those used for timing and native validation. This test-only revision does not alter or retake measurements. Exact patches, both source revisions, raw rows, preflight/admission, full input hashes, native summaries, independent verification and runnable sources are retained under `fixed_state_index_coalescing_arm_20261109` in the existing audit archive. Full guest memory and full run logs remain private.

## Positive NOWAIT prefix read

Code review commit `6fae1868e4ef6a238df97aa2cd35e3d6d1a81f42` changes only the already-modified `storage/overlaybd/src/backend/local.rs`. A positive short NOWAIT result previously fell through to a full-range asynchronous reread. The new branch keeps those bytes and reads only the remaining tail. Complete cache hits, zero-result misses, unsupported-operation fallback and direct I/O retain their paths. There is no new helper, configuration, Rust file or Markdown file.

The existing ignored io-uring regression now changes a synthetic file prefix in the runtime's submission hook, after NOWAIT has copied it and before the asynchronous request reaches the kernel. Before the fix the copied prefix is overwritten and the new assertion fails; after the fix the original prefix, returned count and untouched EOF tail are preserved. The test explicitly requires filesystem NOWAIT support. All 408 storage units pass (four ignored), the explicit cached/fallback check passes, and formatting plus strict changed-crate all-target/all-feature Clippy pass.

A separate **non-timed** probe traces the current reader, before the fix, on the same full 96-generation ARM snapshot used above. It holds the API/runtime/kernel/dependencies fixed, verifies complete residency or targeted eviction, and checks actual program UID/hash, the guest checksum, unchanged layer hashes and cleanup. `strace -v -s0` preserves request lengths while suppressing buffer contents. Only numeric lengths, results and error codes are exported; full traces stay private.

| Current reader input | NOWAIT calls | Positive partial calls | Prefix bytes that the old fallback reads again |
| --- | ---: | ---: | ---: |
| 33 layers, resident | 5410 | 0 | 0 |
| 33 layers, targeted cold | 5411 | 60 | 7,305,216 |
| One layer, resident | 5107 | 0 | 0 |
| One layer, targeted cold | 5104 | 54 | 4,872,192 |

An independent parser reconstructs all four sets of counts from 21,032 numeric call records. This establishes that positive partial reads occur in real cold recovery; it does **not** attribute the original 20.51% post-save readiness gap to them. No positive partial read occurs in the two fully resident probes. These are four instrumented observations, with no latency endpoints, confidence intervals or performance-acceptance decision. The initial abbreviated-iovec setup lacks lengths and is retained separately, not counted in this table.

The final source, including the clarified test prerequisite, is frozen and checked against all 872 local source files. New native programs and the actual corrected reader pass 17 snapshot units (one ignored), CLEAR faults and all five incremental integrations including 96 restores. Complete memory comparisons pass for 36 generations at each of 512 MiB and 4 GiB: 72 comparisons across real compaction boundaries. Actual native program hashes and UID are checked and all captured owned processes exit. Reader hash: `e38aa78a286fb7e9a3182ed4f1970e35ebd6c11357e924e30353df465726a050`.

Exact patches, source/build manifests, numeric calls, independent verifier and native summaries are in `partial_nowait_tail_read_20261109` in the existing audit archive. Original confirmation binaries, observations, gates and six failing latency cells remain unchanged. No new latency cohort is run for this commit.

## Allocation audit of the post-resume pilot

This section reexamines the **unfixed isolated suffix4/post-resume prototype**, not the current code review. The original single Intel dense pair used 4 GiB and eight saves; it is not a confirmation cohort. All values below are allocated bytes. Only the growth row subtracts the common seeded node allocation of 447,086,592 bytes.

| Measurement boundary | Original | Unfixed prototype | Prototype minus original |
| --- | ---: | ---: | ---: |
| Repository after save8 | 18,785,632,256 | 18,252,926,976 | -532,705,280 |
| Outside repository after save8 | 475,136 | 2,220,957,696 | +2,220,482,560 |
| Total node growth after save8 | 18,339,020,800 | 20,026,798,080 | +1,687,777,280 |
| Total state after source/old-record cleanup and latest-read check | 18,785,865,728 | 18,253,164,544 | -532,701,184 |

The reported excess decomposes exactly: a 532,705,280-byte repository reduction plus a 2,220,482,560-byte increase outside the repository equals 1,687,777,280 extra bytes overall. The prototype's outside-repository allocation jumps at generation4, the first compaction, and remains elevated through generation7. The final boundary also deletes older records and checks the latest snapshot, so it is not a source-deletion-only observation.

Source ownership explains a candidate mechanism. Captures are subdirectories of a shared live root, and `FirecrackerCaptureArtifacts` holds a path and root lease rather than deleting each subdirectory when publication returns. The root guard removes its tree when the last lease ends. POSIX publication hard-links immutable local layers where possible. A raw export omitted from the final compacted layer list can therefore retain an allocated inode in the live capture directory without ever entering the repository.

A separate synthetic filesystem proof uses the existing isolated prototype's memory helper, one stable parent, eight dense 1 MiB generations, retained capture directories and same-filesystem publication links. It changes only the superseded-export unlink operation, in the same source directory and toolchain. Its census deduplicates device/inode and counts **memory `*.commit` files only**; it is not a new whole-node space or latency comparison.

| Synthetic model after eight generations | Cleanup suppressed | Existing cleanup enabled |
| --- | ---: | ---: |
| Published memory-commit allocation | 8,503,296 | 8,503,296 |
| Unpublished memory-commit allocation | 1,060,864 | 0 |
| Unpublished inode count | 1 | 0 |
| Published snapshots fully readable after live-root removal | 8 | 8 |

With cleanup suppressed, capture4 retains the raw inode: it initially has two private names, then one after baseline replacement. Removing the live root releases it while published allocation remains unchanged. The new assertion fails with cleanup suppressed and passes with existing cleanup enabled; related prototype snapshot units, formatting and production Clippy also pass. This demonstrates the file-lifetime mechanism. It does **not** prove the exact filename/inode or 2,220,482,560-byte attribution in the retained Intel sample, which remains inaccessible by SSH. Empty/removed ARM native snapshot directories cannot replace that sample.

The corresponding cleanup is already retained in code PR#5; no additional runtime code is added in this audit. Exact aggregate reconstruction, synthetic test patch, source variants, numeric inode rows and independent verifier are under `allocation_ownership_audit_20261109` in the existing archive. All preceding namespaces and original measurements remain unchanged. Original latency acceptance remains unmet.

## Masked ancestor compaction, correctness only

This unretained candidate addresses layer preparation at the producer. Code PR #5 stays at `6fae186`; no new timed cohort is run and original acceptance remains unmet.

All 33 layers in the same retained ARM 96-generation input contribute visible data. Whole-file pruning cannot reduce that stack. A byte-equivalent alternative keeps the stable base and newest raw delta, and merges only older mappings still visible beneath the newest data or explicit zeros. The fixed input then has three layers. All 536,870,912 logical bytes match, SHA256 `3f6cc95f6fc502ff3463c986a3cbe8afe1e127532e23dc74cbcb5c6209106878`; all 33 original blob identities and the saved VM-state record remain unchanged.

The existing single-file compactor also retains source-layer tags in output mappings, preventing adjacent extents from coalescing. Four production lines in the existing `storage/overlaybd/src/lsmt/file/helper.rs` normalize output tags before index compression. One assertion in the existing merged-stack test fails before correction and passes after. No reader-constructor coalescing is added.

| Same fixed input/output rule | Existing producer | Normalized producer |
| --- | ---: | ---: |
| Retained layers | 3 | 3 |
| Compacted older data bytes | 285,655,040 | 285,655,040 |
| Resulting merged-index entries | 3,755 | 1,838 |
| Older-parent file bytes | 285,708,288 | 285,675,520 |

These are structural counts and file sizes, not latency or retained-history space estimates. The latest raw delta is preserved byte-for-byte; merged ancestors contain history.

The runtime candidate uses the existing finalization step after an ordinary tracked capture resumes, with a fixed memory-only suffix budget 3. It keeps the newest raw delta separate, preserves the stable prefix and deletes superseded owned links only after both config writes succeed. Depending on the capture, the suffix has two or three layers; this is not a universal three-layer total. Rootfs policy remains 32, and a pre-CLEAR guard rejects stacks above the reader's 255-layer maximum.

The candidate differs from current code PR #5 in five existing files: memory helper, sandbox lifecycle, existing integration suite, storage writer helper and existing storage tests. If retained, the complete code PR would touch ten existing paths with zero added Rust or Markdown files. No configuration key, module or reader mechanism is introduced. Source/control manifests, exact patches, build settings and both release profiles are frozen; the matched control is current `6fae186`, not original upstream.

Checks pass: 408 storage units/four ignored; 21 snapshot units/one ignored; mixed-codec tests; formatting; strict changed-crate all-target/all-feature and production workspace all-feature Clippy. Native checks pass 72 full-memory comparisons (36 generations at each of 512 MiB/4 GiB), CLEAR faults and all five incremental integrations including 96 restores. Tests cover zero overrides, completely masked ancestors, stable holes, prefix ownership, capture cleanup, failure preservation, idempotence and layer bounds. Program UID/hash, unchanged 872-file source, 33 input blobs and owned-process cleanup are verified. Earlier compilation/fixture failures and intended regression failures remain in their receipts; none counts as a successful run.

`masked_memory_compaction_20261109` in the existing archive contains exact scripts, source revisions, numeric summaries, hashes and a runnable independent verifier. It reconstructs the structural counts and checks saved identities; it does not rerun private full-memory comparisons or establish performance. Current Intel/OSS and original confirmation coverage are not refreshed. No candidate code is adopted on correctness evidence alone.

## Space and retention boundary

Space counts repository logical bytes and allocated node/work/serial bytes, deduplicating hard links by device/inode. Growth subtracts seeded input state and includes the running source's private baseline. Common binaries and fixed dependency images are excluded. The measured filesystem is ext4. The separate transient control runtime directories contained no regular files in the completed batch; writable device data lives under the counted work directories.

| Idle, 30 pairs | Original mean MiB | Current mean MiB | Ratio (upper) |
| --- | ---: | ---: | ---: |
| Repository, all measured history and seeds | 610.69 | 508.81 | 0.833 (0.850) |
| Allocated node growth above seeded input | 184.88 | 83.01 | 0.449 (0.490) |
| Referenced managed layers after old-record deletion, including seeds | 460.81 | 508.57 | 1.104 (1.117) |
| Additional referenced layers above seeds after old-record deletion | 34.70 | 82.46 | 2.377 (2.619) |

The measured history uses less space. Keeping only the latest snapshot has a different result: it still references overlapping older deltas until compaction. Deleting records does not establish that the repository garbage-collected unreferenced blobs. Space reduction is therefore demonstrated for the measured retained history, not for every retention policy.

## Reader attribution with fixed bytes

These diagnostics hold the input snapshot fixed and are not formal acceptance trials.

An earlier full 512 MiB comparison established byte equivalence between nine layers and a flattened single layer, retaining CPU/device/rootfs state. With the original serial reader, median cold readiness was 659.267 versus 412.366 ms (five samples/layout). Bounded overlapping reads reduced those to 396.706 versus 388.709 ms.

The current reader was then measured in 12 randomized pairs on the same equivalent layouts:

| Current reader, identical bytes | Nine layers, median ms | One layer, median ms | Ratio |
| --- | ---: | ---: | ---: |
| Warm ready | 223.24 | 215.45 | 1.036 |
| Warm ready + first full read | 474.76 | 478.94 | 0.991 |
| Targeted-cold ready | 343.48 | 337.18 | 1.019 |
| Targeted-cold ready + first full read | 664.16 | 653.00 | 1.017 |

This control shows a much smaller layout cost after the reader fixes. It does not prove the whole capture/restore lifecycle meets the confidence gates. On a separate fixed nine-layer idle artifact, unrestricted NOWAIT reduced warm readiness by 12.1% and cold combined latency by 6.9% versus the preceding committed reader (12 randomized blocks/reader). The 4 KiB-only variant had no additional benefit and was removed.

A subsequent one-pair diagnostic used `strace -f -c --seccomp-bpf`, filtering `read`, `readv`, `pread64`, `preadv2`, `io_uring_enter` and `io_uring_setup`, around each API server and its descendants. On byte-identical nine-layer/flattened inputs, aggregate `preadv2` counts were 15278/13189 and `io_uring_enter` counts were 6902/4372; both layouts passed checksums. This includes startup, untimed warm-up, warm and cold restores and repeated full checksums. It shows additional read/submission calls can occur with unchanged logical bytes, but tracing perturbs execution and guest background activity. Its times are excluded from acceptance; one traced pair cannot quantify the ordinary latency penalty. The diagnostic ran between completed formal cells and left no owned processes before the original coordinator resumed.

An additional 33-layer control had mixed results: first restore in a fresh API process had warm combined +14.2% and cold combined -14.3%; after an untimed restore plus complete checksum before each timed warm restore, warm combined was -2.1% and cold combined -1.8%. Both cache definitions and negative outcomes were preserved. These results motivated fresh-chain confirmation rather than a claim based on one favorable microbenchmark.

## Additional ARM reader controls

Two further candidates were measured and removed. Replacing the result array with an asynchronous fold reduced code by three lines but made cached fragmented reads about 2–6% slower in the clean microbenchmark. Using `join_all` for groups of at most 32 fragments added three lines and improved 64 KiB/16-fragment reads by about 17–18%. Single-mapping and larger-group controls stayed close to the existing reader. Each variant used four interleaved processes, each with 30 blocks of 128 reads per case; all byte checks passed. An earlier fold sequence overlapped test compilation and is preserved separately as exploratory data.

The small-group candidate then passed 408 OverlayBD library tests (four ignored), formatting of the changed file, package/all-target/all-feature Clippy, and native ARM checks using matching rebuilt programs: five CLEAR integrations including 96 captures/restores, encoded pause, lifecycle, attached drives and the two complete 512 MiB/4 GiB memory oracles. Those checks establish correctness, not a speed improvement.

A fixed rotating-eight-generation snapshot was compared in 12 randomized pairs using an identical **debug API server** and old/new **release daemons**, with a complete untimed restore/checksum before warm measurements and targeted file eviction before cold measurements. Every checksum passed, but no restoration gain was demonstrated:

| Small-group candidate / existing reader | Median ratio | One-sided 95% upper |
| --- | ---: | ---: |
| Warm ready | 1.022 | 1.080 |
| Warm ready + full read | 1.012 | 1.049 |
| Targeted-cold ready | 1.053 | 1.075 |
| Targeted-cold ready + full read | 1.035 | 1.054 |

Both candidates were removed; runtime code remains `367b29f`. These ARM attribution trials do not replace the original seven-cell upstream comparison or establish its noninferiority gates. [Raw controls](reader-controls-arm.json.gz) include both clean and confounded microbenchmarks, candidate patches, diagnostic harness sources, program hashes, the 12 paired restore observations and their bootstrap seed/iterations.

Separate CPU counters on eight fresh nodes compared first/second restores after priming the base VM and warming input memory files. Median daemon CPU time was 76.05/78.32 ms, context switches 244/213.5 and page faults 199/99. These aggregate counters do not identify the remaining source of readiness latency. Sampling interrupts were unavailable on this ARM host; only non-sampling counters are reported. Instrumented wall times are excluded from acceptance. All ten affected ARM Rust files were verified restored, with no diagnostic API, daemon or recorder processes left.

## Observed request geometry and small-read attribution

Three fresh ARM nodes restored the fixed nine-layer rotating snapshot and passed full payload checksums. A diagnostic daemon only added queue thread names to existing JSON request logs. Request selection followed the owned Firecracker process ancestry and its memory block device; rootfs/tool requests were excluded. Logging perturbs execution, so these observations make no latency claim.

| Phase, all three nodes | Requests | Nonzero mapped fragments | Same-file physically contiguous groups | Requests over 32 fragments |
| --- | ---: | ---: | ---: | ---: |
| Restore readiness | 1448 | 6625 | 5011 | 15 |
| First full checksum | 334 | 601 | 484 | 0 |

Most requests were 256 KiB: about 74% during readiness and 88% during the first checksum. Grouping could theoretically reduce fragment read submissions by 24.4%/19.5%, without reading extra bytes, if a suitable vectored interface existed. These are potential submission counts, not a measured speedup. The existing reader has no such interface; no new interface or concurrency-limit change was retained on this evidence.

A separate cached-metadata probe compared 512-byte reads with direct NOWAIT calls, using 50 paired observations per file count and a fresh Tokio runtime each time. For nine files, original/direct-call medians were 0.363/0.015 ms; for 33 files, 0.821/0.048 ms. This includes first blocking-pool expansion, unlike the preceding warm sequential probe. All bytes matched. The small absolute difference on this host does not explain the Intel readiness regression, and no extra owned-read fast path was added.

[Complete numeric data and sources](reader-geometry-arm.json.gz) include synthetic logical/physical mapping numbers, every observed request, static grouping counts, kernel block totals, all 400 metadata observations, program hashes and an independent verifier. That verifier reproduced every observed/static grouping count from the exported mappings. The failed initial VM-identification probe and failed first metadata-probe compilation are recorded separately from valid measurements. All ten affected runtime Rust files and the logging source were restored; no diagnostic API, daemon or recorder remained. Runtime code is still `367b29f`.

## Correctness and actual coverage

| Check | Intel | ARM |
| --- | --- | --- |
| OverlayBD library, io-uring enabled | 408 passed; 4 ignored by default | 408 passed; 4 ignored by default |
| New real io-uring cached/partial/EOF/async-fallback regression | Explicit pass | Explicit pass |
| 96 captures and every-generation restore; source/earlier artifacts removed | Pass | Pass |
| Publication failure, fresh POSIX cache, fork/encoded pause, dirty tracking enabled/disabled | Pass | Pass |
| Concurrent atomic memory/disk writes and lifecycle/attached-drive regressions | Pass | Pass |
| Full 512 MiB and 4 GiB byte oracle, four generations each, including restore | Pass | Pass |
| Rejected/lost/timeout CLEAR response | Terminal, remains paused; baseline restores | Same |
| OSS publication/recovery through fresh resolver/cache after deletion | Pass | Not rerun |
| Fresh-node old/new restore and recapture, matched old/new daemons | Pass | Not rerun |
| Formatting | Pass | Pass |
| Workspace/all-target/all-feature Clippy, warnings denied | Pass | Not claimed: unrelated existing cfg(test) imports |
| OverlayBD all-target/all-feature and integration-target Clippy | Included above | Pass |

The actual new debug daemons were independently rebuilt and verified in spawn logs: Intel `d62cfbd39afac5005f9b775fbde21fb241d6c154599442714b3cdae302ee7b42`, ARM `784d21b5ff7b4cc00e1075124e15d0f496e75c57bae8feb76fc82c3454290e5e`. Explicit cross-filesystem ownership tests passed on the preceding candidate; this reader-only change does not alter ownership helpers. Generated Firecracker schema/client documentation was regenerated with the repository target.

The full aggregate unit/integration targets and Go suites were not rerun. Validation focused on the changed Rust paths and native snapshot behavior. Go code is unchanged. No AMD result is claimed. The isolated repository-pinned S3 fixture was removed before performance measurement.

## Retained negative and incomplete evidence

The first formal batch stopped on a reproducible original-baseline process/file-read hang. Its completed 30-pair idle cell, before the parallel read fix, had cold readiness +25.7% and cold combined +14.1%. Repeated restore probes stalled, including after a byte-equivalent flattening. Some guest endpoints remained live. Correct `username=root` file requests and an explicitly authenticated root process also timed out. A guest-only SysRq request succeeded and showed `ext4lazyinit`, `envd` and a shell blocked in ext4 buffer/folio I/O waits. [Selected diagnostic frames](baseline-blocked-tasks.json) retain no register or memory dump. A second diagnostic iteration failed in its request wrapper before VM creation; it contributes no product result, and cleanup verified no diagnostic processes remained. The original coordinator resumed unchanged. Root cause remains unresolved; the VM artifact is retained privately, not included with public measurement data.

A test-only original-baseline oracle later compared complete 512 MiB memory for 12 captures, including a restore after the first. All bytes matched. Its workload and intervening full-memory reads differ from the failed trial; this does not fix or explain the bad artifact. Original source was restored after compiling the probe.

The preceding `9b99228` formal2 candidate completed 30 pairs each for idle, rotating and hotspot. All three passed primary space reduction and failed one or more latency gates. Dense was interrupted by the user's pause and remains incomplete. Independent index-rewrite/coalescing prototypes had no useful end-to-end gain and were removed. A paused-network probe had no actual memory mutations and failed its activity precondition; it exposed no mismatch and is not counted as a passing regression. The complete Firecracker API pause loop freezes device-event processing, and block snapshot preparation drains asynchronous I/O.

## Evidence and reproduction

[evidence.json](evidence.json) indexes deterministic compressed JSON by SHA-256. The formal reports include all numeric measurements grouped by independent paired trial. Absolute task paths are normalized; numeric data is unchanged. No API keys, node secrets, VM memory files or catalogs are included.

- [Independent 120-pair idle confirmation and raw paired measurements](formal4-idle.json.gz)
- Independent [60-pair rotating](formal4-rotating.json.gz), [60-pair hotspot](formal4-hotspot.json.gz) and [30-pair dense](formal4-dense.json.gz) confirmations with raw paired measurements
- [Earlier current-code 30-pair idle report](formal3-idle.json.gz)
- [Earlier idle](formal2-idle.json.gz), [rotating](formal2-rotating.json.gz), [hotspot](formal2-hotspot.json.gz)
- [Identical-byte layout control](equivalent-layout.json.gz)
- [Three-reader fixed idle control](reader-idle.json.gz)
- [Completed formal3 protocol](formal3-protocol.json) and [fixed larger confirmation protocol](confirmation-protocol.json)
- [Preserved interrupted dense cell](formal2-dense-interrupted.json.gz)
- [Long-chain first-restore reader control](reader-long.json.gz) and [explicitly prewarmed control](reader-long-prewarmed.json.gz)
- [Additional ARM reader controls, candidate patches and harness sources](reader-controls-arm.json.gz)
- [Observed ARM request geometry, synthetic mappings and metadata attribution](reader-geometry-arm.json.gz)
- Completed [4 GiB rotating](formal4-4096-rotating.json.gz), [4 GiB dense](formal4-4096-dense.json.gz) and [96-generation rotating](formal4-long.json.gz) cells
- [Frozen-program control and endpoint audit](formal4-control-audit.json.gz)

Follow [the benchmark preparation guide](../../../scripts/benchmarks/README.md) to regenerate fresh seeds and trials. Run each case independently with its fixed pair count. Preserve every protocol and failed/incomplete sample. Exact current runtime hashes are above.

To recompute the saved ratios without VM artifacts:

```bash
python3 - <<'PYCODE'
import gzip, json, math, sys
sys.path.insert(0, 'scripts/benchmarks')
from analyze_clear_comparison import ratios
for name in ('formal3-idle', 'formal4-idle', 'formal4-rotating', 'formal4-hotspot',
             'formal4-dense', 'formal4-4096-rotating', 'formal4-4096-dense', 'formal4-long'):
    path = f'docs/experiments/clear-memory-20261108/{name}.json.gz'
    report = json.load(gzip.open(path, 'rt'))
    for index, workload in enumerate(report['workloads']):
        pairs = workload['paired_measurements']
        for metric, saved in workload['metrics'].items():
            recalculated = ratios([(p['baseline'][metric], p['candidate'][metric]) for p in pairs],
                                  report['bootstrap_iterations'], 20261107 + index)
            for statistic in ('p50', 'p95', 'mean'):
                for field in ('ratio', 'upper_95_one_sided'):
                    assert math.isclose(recalculated[statistic][field], saved[statistic][field],
                                        rel_tol=1e-12, abs_tol=1e-12)
print('Paired ratios reproduced')

controls = json.load(gzip.open('docs/experiments/clear-memory-20261108/reader-controls-arm.json.gz', 'rt'))
study = controls['restore_join']
paired = {reader: {row['block']: row for row in study['measurements'] if row['reader'] == reader}
          for reader in ('original', 'join')}
for metric, saved in study['statistics']['metrics'].items():
    recalculated = ratios([([paired['original'][i][metric]], [paired['join'][i][metric]])
                           for i in range(study['protocol']['pairs'])],
                          study['bootstrap_iterations'], study['bootstrap_seed'])
    for statistic in ('p50', 'p95', 'mean'):
        for field in ('ratio', 'upper_95_one_sided'):
            assert math.isclose(recalculated[statistic][field], saved[statistic][field],
                                rel_tol=1e-12, abs_tol=1e-12)
print('ARM diagnostic ratios reproduced')

audit = json.load(gzip.open('docs/experiments/clear-memory-20261108/formal4-control-audit.json.gz', 'rt'))
for namespace, old, new, comparison, old_label, new_label in (
    ('fixed_state_reader_layout_20261108', 'baseline-reader', 'current-reader',
     'current/baseline reader', 'baseline reader', 'current reader'),
    ('fixed_state_linear_index_comparison_20261108', 'tree-index', 'linear-index',
     'linear/tree index', 'tree index', 'linear index'),
    ('fixed_state_preparation_candidate_20261108', 'tree-index', 'prepared-reader',
     'prepared/current reader', 'current reader', 'prepared reader'),
):
    study = audit[namespace]
    points = {(r['cache_profile'], r['block'], r['reader'], r['layout']): r for r in study['measurements']}
    assert len(points) == len(study['measurements']) == 240
    contrasts = (
        (comparison + ', layered', (old, 'layered'), (new, 'layered')),
        (comparison + ', flat', (old, 'flat'), (new, 'flat')),
        ('layered/flat, ' + old_label, (old, 'flat'), (old, 'layered')),
        ('layered/flat, ' + new_label, (new, 'flat'), (new, 'layered')),
    )
    for profile, result in study['analysis']['profiles'].items():
        for label, denominator, numerator in contrasts:
            for metric, saved in result['contrasts'][label].items():
                recalculated = ratios([([points[(profile, b) + denominator][metric]],
                                        [points[(profile, b) + numerator][metric]]) for b in range(30)],
                                      10000, 20261108)
                for statistic in ('p50', 'p95', 'mean'):
                    for field in ('ratio', 'upper_95_one_sided'):
                        assert math.isclose(recalculated[statistic][field], saved[statistic][field],
                                            rel_tol=1e-12, abs_tol=1e-12)
    print(namespace, '24 complete-block contrast statistics reproduced')
PYCODE
```

To independently reproduce request grouping without VM memory files, unpack the synthetic numeric input and saved verifier into a temporary directory:

```bash
python3 - <<'PYCODE'
import gzip, json, pathlib, subprocess, sys, tempfile
bundle = json.load(gzip.open('docs/experiments/clear-memory-20261108/reader-geometry-arm.json.gz', 'rt'))
with tempfile.TemporaryDirectory() as directory:
    root = pathlib.Path(directory)
    (root / 'tools').mkdir()
    for key, name in [('observed_with_layout', 'read-groups-observed-with-layout.json'),
                      ('requests', 'requests.json'), ('static', 'read-groups-static.json')]:
        (root / name).write_text(json.dumps(bundle['data'][key]))
    verifier = root / 'tools/verify-read-geometry.py'
    verifier.write_text(bundle['sources']['verify-read-geometry.py'])
    subprocess.run([sys.executable, str(verifier)], check=True)
PYCODE
```

To reproduce the PMU diagnostic from the embedded CSVs and frozen metadata:

```bash
python3 - <<'PYCODE'
import gzip, json, pathlib, subprocess, sys, tempfile
bundle = json.load(gzip.open('docs/experiments/clear-memory-20261108/formal4-control-audit.json.gz', 'rt'))
study = bundle['fixed_state_first_read_pmu_20261108']
with tempfile.TemporaryDirectory() as directory:
    root = pathlib.Path(directory)
    (root / 'protocol.json').write_text(study['protocol_json'])
    (root / 'progress.json').write_text(json.dumps(study['progress']))
    (root / 'protocol.sha256').write_text(study['analysis']['protocol_sha256'] + '\n')
    (root / 'observations.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in study['observations']))
    script = root / 'analyze-first-read-cpu.py'
    script.write_text(study['sources']['analyze-first-read-cpu.py'])
    subprocess.run([sys.executable, str(script), '--pmu', str(root)], check=True)
    assert json.loads((root / 'analysis.json').read_text()) == study['analysis']
PYCODE
```

To reproduce both pilots' metric extraction and descriptive summaries independently:

```bash
python3 - <<'PYCODE'
import gzip, json, pathlib, subprocess, sys, tempfile
bundle = json.load(gzip.open('docs/experiments/clear-memory-20261108/formal4-control-audit.json.gz', 'rt'))
for namespace in ['memory_suffix_policy_pilot_20261109', 'post_resume_original_pilot_20261109']:
    study = bundle[namespace]
    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        raw = root / 'pilot-v2-results.json'
        raw.write_text(json.dumps(study['complete_results']))
        frozen = root / 'frozen-analyzer.py'
        frozen.write_text(bundle['frozen_analyzer'])
        script = root / 'analyze-memory-budget-pilot.py'
        script.write_text(study['sources']['analyze-memory-budget-pilot.py'])
        subprocess.run([sys.executable, str(script), str(raw), str(frozen)], check=True)
        assert json.loads((root / 'pilot-v2-analysis.json').read_text()) == study['analysis']
PYCODE
```

To independently reproduce the complete ARM coalescing control's paired statistics:

```bash
python3 - <<'PYCODE'
import gzip, json, pathlib, subprocess, sys, tempfile
bundle = json.load(gzip.open('docs/experiments/clear-memory-20261108/formal4-control-audit.json.gz', 'rt'))
study = bundle['fixed_state_index_coalescing_arm_20261109']
with tempfile.TemporaryDirectory() as directory:
    root = pathlib.Path(directory)
    raw = root / 'results.json'
    raw.write_text(json.dumps(study['results']))
    script = root / 'verify-coalescing-control.py'
    script.write_text(study['sources']['verify-coalescing-control.py'])
    subprocess.run([sys.executable, str(script), str(raw)], check=True)
    result = json.loads((root / 'independent-validation.json').read_text())
    assert result['complete'] and result['statistic_fields_checked'] == 288
    assert result['max_absolute_error'] <= 1e-12
PYCODE
```

To independently verify the positive-prefix numeric calls and correctness receipts:

```bash
python3 - <<'PYCODE'
import gzip, json, pathlib, subprocess, sys, tempfile
bundle = json.load(gzip.open('docs/experiments/clear-memory-20261108/formal4-control-audit.json.gz', 'rt'))
data = bundle['partial_nowait_tail_read_20261109']
with tempfile.TemporaryDirectory() as directory:
    root = pathlib.Path(directory)
    raw = root / 'results.json'
    raw.write_text(json.dumps(data))
    script = root / 'verify.py'
    script.write_text(data['numeric_verifier_source'])
    subprocess.run([sys.executable, str(script), str(raw)], check=True)
PYCODE
```

To reconstruct the pilot allocation split and verify the isolated inode mechanism:

```bash
python3 - <<'PYCODE'
import gzip, json, pathlib, subprocess, sys, tempfile
bundle = json.load(gzip.open('docs/experiments/clear-memory-20261108/formal4-control-audit.json.gz', 'rt'))
proof = bundle['allocation_ownership_audit_20261109']
with tempfile.TemporaryDirectory() as directory:
    root = pathlib.Path(directory)
    raw = root / 'audit.json'
    raw.write_text(json.dumps(bundle))
    script = root / 'verify.py'
    script.write_text(proof['independent_verifier_source'])
    subprocess.run([sys.executable, str(script), str(raw)], check=True)
PYCODE
```

To verify the masked-ancestor structural counts, source/control identities and native summaries:

```bash
python3 - <<'PYCODE'
import gzip, json, pathlib, subprocess, sys, tempfile
bundle = json.load(gzip.open('docs/experiments/clear-memory-20261108/formal4-control-audit.json.gz', 'rt'))
data = bundle['masked_memory_compaction_20261109']
with tempfile.TemporaryDirectory() as directory:
    root = pathlib.Path(directory)
    raw = root / 'proof.json'
    raw.write_text(json.dumps(data))
    script = root / 'verify.py'
    script.write_text(data['independent_verifier_source'])
    subprocess.run([sys.executable, str(script), str(raw)], check=True)
PYCODE
```

## Controlled comparison plan

Status: A retains the original seven-cell protocol and failures; the separately declared B cohort is now complete and reported above. This plan does not itself launch trials. The original confidence gates and seven workload cells are unchanged.

### Behavior to implement

Each successful CLEAR establishes the baseline for the next dirty-memory export. Ordinary exports include newly reported dirty ranges, explicit zero updates and the runtime's conservatively dirty pages. Retain the existing periodic compaction policy: a merged output can contain earlier updates. Keeping bounded chains avoids changing the format or introducing another reader mechanism. Report incremental export bytes and compaction rewrite bytes separately. Do not describe a merged artifact as a latest-only delta.

Complete restoration must survive source termination, capture cleanup and earlier-record deletion. Retained-history space and latest-only retention are separate outcomes. The current implementation has passed the measured retained-history space checks; latest-only referenced bytes can be larger. Record deletion is not evidence of blob garbage collection.

### Two different questions

| Study | Question | Fixed inputs | Intended differences | Use |
| --- | --- | --- | --- | --- |
| A: whole implementation | Does CLEAR integration meet storage and latency requirements over repeated captures? | Seed, RAM/vCPU size, deterministic workload, generation count, runtime/kernel, driver, endpoint definitions and host policy | Original implementation versus code candidate, including produced layer layout and lifecycle | Original acceptance |
| B: reader/layout attribution | What contribution comes from reader code and layer layout for the same saved VM state? | VM-state bytes, full logical memory bytes, rootfs/tools/drives, common API server/runtime, build settings, cache profile and endpoint | Reader version and memory layout as explicitly named factors | Diagnosis only |

A cannot isolate CLEAR, reader code or layer count individually. B cannot establish whole-lifecycle acceptance or replace a failed A endpoint. ARM/debug-server controls and earlier batches retain their own identities and are never pooled with either new cohort.

### A: preserve the original endpoint

Keep the seven cells: 512 MiB idle/rotating/hotspot/dense for eight generations; 4096 MiB rotating/dense for eight generations; 512 MiB rotating for 96 generations. Keep the fixed confirmation counts 120/60/60/30/30/30/30. Any changed candidate requires a new, separately identified confirmation; the current six failed cells are not overwritten or extended after inspection.

The historical raw `warm` endpoint is named **post-save first restore** in explanations. Reproduce its exact sequence: finish snapshot POST; collect the existing space/pause measurements; issue the first restore POST; record readiness when the successful public response is completely read; run the first guest checksum as a separate endpoint. There is no explicit input eviction or complete prewarm. Cache residency is not assumed equal. Preserve the running source VM during these measurements.

Targeted-cold follows that restore and deletion, using the existing flush/eviction path and <=5% memory-layer residency condition. It leaves process/index/metadata caches normal. Save/pause endpoints and the observation-by-observation combined endpoints retain their existing boundaries. Workload setup and checksum preparation stay outside save timing for both variants.

A new endpoint or a residency-instrumented driver is a new protocol version. Apply it to both variants and report it separately. It must not silently replace post-save first restore or inherit the earlier samples.

### B: isolate one input and two explicit factors

Use the retained 96-generation candidate snapshot as the fixed VM state. Before measurement, create a flattened memory representation once and verify all virtual RAM bytes against the original 33-layer representation. Keep the VM-state file and every non-memory artifact identical. Record original and flat blob manifests. A matching payload checksum alone is insufficient to establish complete memory equivalence.

Use one common API server and Firecracker binary. Build two release daemons with the same compiler, lockfile, features, profile and environment: baseline reader and current reader. A source manifest must prove that the release-relevant differences are confined to `local.rs` and `readonly.rs`. This diagnoses the combined reader changes; it does not distinguish NOWAIT from parallel mapping reads.

The design is reader {baseline, current} x memory layout {33 layers, flattened}, with 30 independent randomized complete blocks per cache profile, fixed before running. Each observation uses its own node state, runtime directory and daemon. A common untimed base-template launch may initialize the daemon/tools path, but the target snapshot must not be restored beforehand.

Keep two separately named file-cache profiles: referenced local files resident (>=99% verified), and targeted memory files cold (<=5% verified). File warming, not an untimed target restore, prepares the first profile. Rootfs/tools preparation must match across all four cells. Both profiles measure the first target open in the fresh node; neither is equated with A's post-save or after-warm-restore cache state. A later repeated-device/index-hot observation would be another endpoint.

Freeze all timing boundaries and placement rules before execution. A new attribution driver must record monotonic durations and wall-clock alignment brackets at request start and complete-response readiness for every variant; these fields do not retroactively redefine the original endpoints. Report readiness, checksum and their observed combined duration independently. If a tracing/profiling tool is used, its cohort is diagnostic and its perturbed times are excluded from uninstrumented estimates.

### Preflight and reporting

Before a cohort starts, save immutable manifests for every program, shared dependency, seed, artifact, driver, analyzer and configuration. Validate actual daemon spawn paths, not build-cache filenames. Preserve a canonical configuration comparison with only allowed path/variant differences. Serialize trials; record CPU/NUMA placement and shared-host load without altering other users' services. Any new placement policy is a separately declared protocol applied to every cell.

Validate generation counts, restore positions, payload checksums, complete-trial markers and actual layer counts. Cache-precondition failures, unsuccessful requests and incomplete observations are retained and prevent acceptance of that cohort. Do not silently retry, exclude outliers, change sample counts, select a favorable subset or pool different driver/program/cache definitions.

Use the same frozen analysis for both variants. Resample complete independent paired blocks 10000 times; never treat correlated generations as independent replicas. Ratios divide the two sample medians, not per-pair ratios. Compute combined durations per observation before calculating their median. Publish raw numeric observations alongside the source and program identities.

Original A gates remain: every latency endpoint's one-sided 95% upper ratio <=1.05 for p50 and <=1.10 for p95, and the primary space upper mean-ratio bound below one, in every complete cell. B can guide a small code change, but cannot waive these gates. A runtime change is kept only with relevant correctness coverage and a reproducible end-to-end benefit; a microbenchmark win alone is insufficient.
