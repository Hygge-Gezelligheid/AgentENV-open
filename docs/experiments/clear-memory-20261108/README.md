# CLEAR memory baseline experiment

Status on 2026-11-08: all seven fixed confirmation cells completed, with payload/deletion checks and primary retained-history space reduction passing. Six cells fail one or more unchanged latency gates; only the 4 GiB dense cell passes every latency gate. Performance acceptance is **not established**. This PR preserves experimental evidence; the implementation is reviewed separately in the code PR.

## Implementation

After a complete local capture, AgentENV owns the memory parents independently of capture/publication cleanup, calls `PUT /vm/dirty-memory-ranges/clear` while the VM is paused, adopts that baseline, then resumes before repository publication. Subsequent ordinary memory layers contain the runtime-reported dirty ranges since the preceding successful CLEAR. Runtime conservative dirty pages are retained. Existing periodic compaction can produce a merged layer; complete snapshots still reference their parents.

GET/CLEAR errors, cancellation and ambiguous responses poison dirty tracking and prevent an unsafe resume. Publication failure after a successful CLEAR leaves the independently owned baseline usable. Dirty tracking disabled retains residency-based capture behavior. The snapshot record and layer formats are unchanged.

The read changes are confined to OverlayBD:

- Independent mapped fragments read into disjoint destination slices, with at most 32 in flight. All borrowed-buffer operations drain before propagating an error. The production read implementation has fewer lines than the original serial loop.
- Buffered contextual reads first try `preadv2(RWF_NOWAIT)`. A complete cache hit returns immediately. Misses, partial reads, EOF and errors use the original full-range asynchronous read. Unsupported operation is remembered per open file. This adds roughly thirty production lines, one atomic flag and a focused real io-uring regression; no data cache, extra copy buffer or configuration.

The code audit also removed repeated disk-layer adoption and repeated parent hashing. Rootfs/drive sealed files have one live owner; captures obtain independent links or sparse copies. An index-rewrite prototype, coalescing prototype and 4 KiB-only NOWAIT alternative were measured and removed because they did not improve the relevant end-to-end behavior.

The five handwritten runtime files have a net increase of 180 lines before their independent `cfg(test)` modules: client +27, snapshot helpers +32, sandbox lifecycle +100, LocalFile +31 and mapped reads -10. Counts include comments and blank lines. The baseline ownership and failure state belong to the capture lifecycle; the reader changes stay in OverlayBD.

## Fixed programs and environment

Original upstream baseline: `05323abea960ab9348208d774c320d3782651544`. Current runtime code: `367b29fcf989fa7a0ffcc65564451f7be6b280a1`; all 21 changed files matched the source manifest before measurement. Later report/export changes do not alter those runtime binaries. Earlier results explicitly identify the preceding `9b99228` candidate.

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

Subsequent input/build preparation verified all 536870912 logical memory bytes between the retained 33-layer image and a flattened single layer, preserving the saved VM-state files. Two reader builds share 872 source files with exactly two differences (local.rs, readonly.rs), matching compiler settings, release profiles and io-uring features. The initial shared-target build attempt copied identical binaries and was rejected before any trial. Source timestamp refresh plus Cargo JSON checks required the library and executable to be rebuilt and supplied the actual executable paths. The accepted binary hashes reproduce the original frozen formal4 baseline/current readers exactly. All attempts, build manifests and preparation sources are retained in the existing audit bundle. This prepares controlled inputs only: no VM or performance trial was launched, and actual spawn/cache preflight remains mandatory.

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

## Controlled comparison plan

Status: proposed controls for subsequent work; no new performance trial is started by this document. Existing measurements and failures remain intact. The original confidence gates and seven workload cells are unchanged.

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
