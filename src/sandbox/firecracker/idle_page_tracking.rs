//! Linux idle-page tracking for template working-set profiling.
//!
//! It translates Firecracker guest-RAM HVA mappings into GPA pages while keeping
//! host virtual addresses and PFNs out of manifest metadata and trace detail.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::{Arc, LazyLock};

use anyhow::{bail, ensure, Context, Result};
use tokio::sync::{Mutex, OwnedMutexGuard};

const PAGE_SIZE: u64 = 4096;
const PAGEMAP_PRESENT: u64 = 1 << 63;
const PAGEMAP_PFN_MASK: u64 = (1 << 55) - 1;

static PAGE_IDLE_PROFILING_LOCK: LazyLock<Arc<Mutex<()>>> =
    LazyLock::new(|| Arc::new(Mutex::new(())));
const PAGE_IDLE_BITMAP: &str = "/sys/kernel/mm/page_idle/bitmap";
const ENTRIES_PER_READ: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GuestRamRegion {
    pub(crate) base_host_virt_addr: u64,
    pub(crate) guest_phys_addr: u64,
    pub(crate) size: u64,
    pub(crate) page_size: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct IdlePageTrackingTarget {
    pub(crate) firecracker_pid: u32,
    pub(crate) regions: Vec<GuestRamRegion>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdlePageTrackingBaseline {
    pub present_pages: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdlePageTrackingObservation {
    /// Baseline pages whose PFN was stable and idle bit was cleared.
    pub accessed_baseline_gpas: BTreeSet<u64>,
    /// Baseline pages whose idle bit stayed set.
    pub still_idle_baseline_gpas: BTreeSet<u64>,
    /// Pages absent in the baseline scan and present after the resume window.
    pub newly_present_gpas: BTreeSet<u64>,
    /// Baseline GPA whose host PFN changed during the observation window.
    pub replaced_baseline_gpas: BTreeSet<u64>,
    /// Baseline GPA no longer resident after the observation window.
    pub no_longer_present_gpas: BTreeSet<u64>,
}

impl IdlePageTrackingObservation {
    pub fn tracked_pages(&self) -> BTreeSet<u64> {
        self.accessed_baseline_gpas
            .iter()
            .chain(&self.newly_present_gpas)
            .copied()
            .collect()
    }

    pub fn tracked_ranges(&self) -> Result<Vec<super::manifest::GuestMemoryRange>> {
        coalesce_gpa_pages(&self.tracked_pages())
    }

    pub fn tracked_bytes(&self) -> u64 {
        self.tracked_page_count() as u64 * PAGE_SIZE
    }

    pub fn tracked_page_count(&self) -> usize {
        self.tracked_pages().len()
    }

    /// Build metadata only after applying the same RAM-layout and budget checks
    /// required again at restore time. Callers must log the returned error and
    /// publish the snapshot without metadata rather than truncating the hint.
    pub fn to_working_set(
        &self,
        regions: &[super::manifest::GuestMemoryRegion],
        limits: super::manifest::GuestMemoryWorkingSetLimits,
    ) -> Result<super::manifest::GuestMemoryWorkingSet> {
        let working_set = super::manifest::GuestMemoryWorkingSet::new_with_tracker(
            super::manifest::LINUX_IDLE_PAGE_TRACKER,
            self.tracked_ranges()?,
        );
        working_set.validate_for_regions(regions, limits)?;
        Ok(working_set)
    }
}

#[derive(Clone, Debug)]
struct BaselinePage {
    pfn: u64,
}

/// The Linux file interface is deliberately isolated so regular unit tests can
/// exercise tracker lifecycle semantics without `/proc` or `/sys` privileges.
pub(crate) trait IdlePageTrackingIo: Send + Sync {
    fn scan_present_pages(&self, target: &IdlePageTrackingTarget) -> Result<BTreeMap<u64, u64>>;
    fn mark_pfns_idle(&self, pfns: &[u64]) -> Result<()>;
    fn read_idle_bits(&self, pfns: &[u64]) -> Result<BTreeMap<u64, bool>>;
}

#[derive(Default)]
pub(crate) struct LinuxIdlePageTrackingIo;

impl IdlePageTrackingIo for LinuxIdlePageTrackingIo {
    fn scan_present_pages(&self, target: &IdlePageTrackingTarget) -> Result<BTreeMap<u64, u64>> {
        scan_present_pages(target)
    }

    fn mark_pfns_idle(&self, pfns: &[u64]) -> Result<()> {
        mark_pfns_idle(pfns.iter().copied())
    }

    fn read_idle_bits(&self, pfns: &[u64]) -> Result<BTreeMap<u64, bool>> {
        read_idle_bits(pfns.iter().copied())
    }
}

/// Tracks Firecracker guest RAM through Linux page-idle state.
///
/// The caller must call mark_baseline_idle while the restored VM is paused,
/// then resume it and call observe_after_pause after a raw Firecracker pause.
/// Snapshot capture is intentionally not allowed inside this window because it
/// can touch guest-memory pages through process_vm_readv.
pub(crate) struct IdlePageTracker<I = LinuxIdlePageTrackingIo> {
    target: IdlePageTrackingTarget,
    baseline: BTreeMap<u64, BaselinePage>,
    io: I,
    /// Held through the entire observation window because page-idle is global.
    _session_lock: OwnedMutexGuard<()>,
}

impl IdlePageTracker<LinuxIdlePageTrackingIo> {
    pub(crate) async fn new(target: IdlePageTrackingTarget) -> Result<Self> {
        Self::new_with_io(target, LinuxIdlePageTrackingIo).await
    }
}

impl<I: IdlePageTrackingIo> IdlePageTracker<I> {
    async fn new_with_io(target: IdlePageTrackingTarget, io: I) -> Result<Self> {
        ensure!(
            target.firecracker_pid != 0,
            "Firecracker PID must be non-zero for idle-page tracking"
        );
        ensure!(
            cfg!(target_arch = "x86_64"),
            "idle-page template profiling currently supports x86_64 hosts only"
        );
        validate_regions(&target.regions)?;
        let session_lock = PAGE_IDLE_PROFILING_LOCK.clone().lock_owned().await;
        Ok(Self {
            target,
            baseline: BTreeMap::new(),
            io,
            _session_lock: session_lock,
        })
    }

    /// Records every present guest RAM page and marks its host PFN idle.
    ///
    /// The bitmap is normally root-only (0600), so the test runner needs
    /// CAP_DAC_OVERRIDE as well as CAP_SYS_ADMIN for unmasked pagemap PFNs.
    pub fn mark_baseline_idle(&mut self) -> Result<IdlePageTrackingBaseline> {
        crate::privileges::require_idle_page_tracking_capabilities()?;
        let present = crate::privileges::run_with_scoped_capabilities(
            &[
                crate::privileges::CAP_SYS_ADMIN,
                crate::privileges::CAP_DAC_OVERRIDE,
            ],
            || self.capture_baseline_with_io(),
        )?;
        self.baseline = present
            .into_iter()
            .map(|(gpa, pfn)| (gpa, BaselinePage { pfn }))
            .collect();
        Ok(IdlePageTrackingBaseline {
            present_pages: self.baseline.len(),
        })
    }

    /// Whether a GPA was resident in the paused baseline scan.
    pub fn was_baseline_present(&self, gpa: u64) -> bool {
        self.baseline.contains_key(&gpa)
    }

    pub fn observe_after_pause(&self) -> Result<IdlePageTrackingObservation> {
        ensure!(
            !self.baseline.is_empty(),
            "mark_baseline_idle must run before idle-page observation"
        );
        crate::privileges::require_idle_page_tracking_capabilities()?;
        let observation = crate::privileges::run_with_scoped_capabilities(
            &[
                crate::privileges::CAP_SYS_ADMIN,
                crate::privileges::CAP_DAC_OVERRIDE,
            ],
            || self.observe_with_io(),
        )?;
        Ok(observation)
    }

    fn capture_baseline_with_io(&self) -> Result<BTreeMap<u64, u64>> {
        let present = self.io.scan_present_pages(&self.target)?;
        ensure!(
            !present.is_empty(),
            "no present guest-RAM pages found before tracking"
        );
        let pfns = present.values().copied().collect::<Vec<_>>();
        self.io.mark_pfns_idle(&pfns)?;
        Ok(present)
    }

    fn observe_with_io(&self) -> Result<IdlePageTrackingObservation> {
        let current = self.io.scan_present_pages(&self.target)?;
        let pfns = self
            .baseline
            .values()
            .map(|page| page.pfn)
            .collect::<Vec<_>>();
        let idle_bits = self.io.read_idle_bits(&pfns)?;
        classify_observation(&self.baseline, current, &idle_bits)
    }
}

fn scan_present_pages(target: &IdlePageTrackingTarget) -> Result<BTreeMap<u64, u64>> {
    let path = format!("/proc/{}/pagemap", target.firecracker_pid);
    let pagemap = File::open(&path)
        .with_context(|| format!("open {path} to inspect Firecracker guest RAM"))?;
    let mut pages = BTreeMap::new();

    for region in &target.regions {
        let total = region.size / PAGE_SIZE;
        let mut index = 0_u64;
        let mut buffer = vec![0_u8; ENTRIES_PER_READ * 8];
        while index < total {
            let count = usize::try_from((total - index).min(ENTRIES_PER_READ as u64))
                .expect("bounded pagemap count fits usize");
            let hva = region
                .base_host_virt_addr
                .checked_add(index * PAGE_SIZE)
                .context("HVA overflow while reading pagemap")?;
            let offset = (hva / PAGE_SIZE)
                .checked_mul(8)
                .context("pagemap offset overflow")?;
            read_exact_at(&pagemap, &mut buffer[..count * 8], offset)
                .with_context(|| format!("read {path} at offset {offset}"))?;

            for entry_index in 0..count {
                let start = entry_index * 8;
                let entry = u64::from_ne_bytes(
                    buffer[start..start + 8]
                        .try_into()
                        .expect("pagemap entry is eight bytes"),
                );
                let Some(pfn) = parse_present_pfn(entry)
                    .with_context(|| format!("read pagemap entry for Firecracker HVA {hva:#x}"))?
                else {
                    continue;
                };
                let guest_offset = (index + entry_index as u64)
                    .checked_mul(PAGE_SIZE)
                    .context("GPA offset overflow")?;
                let gpa = region
                    .guest_phys_addr
                    .checked_add(guest_offset)
                    .context("GPA overflow while translating HVA")?;
                if pages.insert(gpa, pfn).is_some() {
                    bail!("duplicate guest physical page {gpa:#x}");
                }
            }
            index += count as u64;
        }
    }
    Ok(pages)
}

fn parse_present_pfn(entry: u64) -> Result<Option<u64>> {
    if entry & PAGEMAP_PRESENT == 0 {
        return Ok(None);
    }
    let pfn = entry & PAGEMAP_PFN_MASK;
    ensure!(
        pfn != 0,
        "pagemap PFN is masked for a present page; run with CAP_SYS_ADMIN"
    );
    Ok(Some(pfn))
}
fn classify_observation(
    baseline: &BTreeMap<u64, BaselinePage>,
    mut current: BTreeMap<u64, u64>,
    idle_bits: &BTreeMap<u64, bool>,
) -> Result<IdlePageTrackingObservation> {
    let mut result = IdlePageTrackingObservation::default();
    for (&gpa, baseline_page) in baseline {
        match current.remove(&gpa) {
            Some(pfn) if pfn == baseline_page.pfn => {
                let idle = *idle_bits
                    .get(&pfn)
                    .with_context(|| format!("missing idle bitmap result for PFN {pfn}"))?;
                if idle {
                    result.still_idle_baseline_gpas.insert(gpa);
                } else {
                    result.accessed_baseline_gpas.insert(gpa);
                }
            }
            Some(_) => {
                result.replaced_baseline_gpas.insert(gpa);
                result.newly_present_gpas.insert(gpa);
            }
            None => {
                result.no_longer_present_gpas.insert(gpa);
            }
        }
    }
    result.newly_present_gpas.extend(current.into_keys());
    Ok(result)
}
pub fn coalesce_gpa_pages(pages: &BTreeSet<u64>) -> Result<Vec<super::manifest::GuestMemoryRange>> {
    let Some(&first) = pages.iter().next() else {
        return Ok(Vec::new());
    };
    ensure!(
        first % PAGE_SIZE == 0,
        "GPA {first:#x} is not 4 KiB aligned"
    );
    let mut start = first;
    let mut previous = first;
    let mut result = Vec::new();

    for &page in pages.iter().skip(1) {
        ensure!(page % PAGE_SIZE == 0, "GPA {page:#x} is not 4 KiB aligned");
        if previous.checked_add(PAGE_SIZE) == Some(page) {
            previous = page;
            continue;
        }
        let size = previous
            .checked_sub(start)
            .and_then(|span| span.checked_add(PAGE_SIZE))
            .context("coalesced GPA range overflows u64")?;
        result.push(super::manifest::GuestMemoryRange { gpa: start, size });
        start = page;
        previous = page;
    }
    let size = previous
        .checked_sub(start)
        .and_then(|span| span.checked_add(PAGE_SIZE))
        .context("coalesced GPA range overflows u64")?;
    result.push(super::manifest::GuestMemoryRange { gpa: start, size });
    Ok(result)
}

fn validate_regions(regions: &[GuestRamRegion]) -> Result<()> {
    ensure!(
        !regions.is_empty(),
        "Firecracker returned no guest-memory regions"
    );
    for region in regions {
        ensure!(
            region.page_size == PAGE_SIZE,
            "the experiment supports only normal 4 KiB guest pages; Firecracker reported {}",
            region.page_size
        );
        ensure!(
            region.size > 0 && region.size % PAGE_SIZE == 0,
            "guest memory region size must be a non-zero multiple of 4 KiB"
        );
        ensure!(
            region.base_host_virt_addr % PAGE_SIZE == 0 && region.guest_phys_addr % PAGE_SIZE == 0,
            "guest memory HVA and GPA starts must be 4 KiB aligned"
        );
        region
            .base_host_virt_addr
            .checked_add(region.size)
            .context("guest memory HVA range overflows u64")?;
        region
            .guest_phys_addr
            .checked_add(region.size)
            .context("guest memory GPA range overflows u64")?;
    }
    ensure_non_overlapping(regions, |region| region.base_host_virt_addr, "HVA")?;
    ensure_non_overlapping(regions, |region| region.guest_phys_addr, "GPA")
}

fn ensure_non_overlapping(
    regions: &[GuestRamRegion],
    start: impl Fn(&GuestRamRegion) -> u64,
    label: &str,
) -> Result<()> {
    let mut sorted = regions.to_vec();
    sorted.sort_by_key(&start);
    for pair in sorted.windows(2) {
        let left_end = start(&pair[0])
            .checked_add(pair[0].size)
            .expect("region overflow was checked before overlap validation");
        ensure!(
            left_end <= start(&pair[1]),
            "Firecracker returned overlapping guest-memory {label} regions"
        );
    }
    Ok(())
}

fn mark_pfns_idle(pfns: impl Iterator<Item = u64>) -> Result<()> {
    let words = bitmap_words(pfns);
    let bitmap = OpenOptions::new()
        .read(true)
        .write(true)
        .open(PAGE_IDLE_BITMAP)
        .with_context(|| {
            format!(
                "open {PAGE_IDLE_BITMAP}; test runner needs CAP_DAC_OVERRIDE in addition to CAP_SYS_ADMIN"
            )
        })?;
    for (word, mask) in words {
        let offset = word
            .checked_mul(8)
            .context("page-idle bitmap offset overflow")?;
        let mut bytes = [0_u8; 8];
        read_exact_at(&bitmap, &mut bytes, offset)?;
        let value = u64::from_ne_bytes(bytes) | mask;
        write_all_at(&bitmap, &value.to_ne_bytes(), offset)?;
    }
    Ok(())
}

fn read_idle_bits(pfns: impl Iterator<Item = u64>) -> Result<BTreeMap<u64, bool>> {
    let words = bitmap_words(pfns);
    let bitmap = File::open(PAGE_IDLE_BITMAP).with_context(|| {
        format!(
            "open {PAGE_IDLE_BITMAP}; test runner needs CAP_DAC_OVERRIDE in addition to CAP_SYS_ADMIN"
        )
    })?;
    let mut result = BTreeMap::new();
    for (word, requested) in words {
        let offset = word
            .checked_mul(8)
            .context("page-idle bitmap offset overflow")?;
        let mut bytes = [0_u8; 8];
        read_exact_at(&bitmap, &mut bytes, offset)?;
        let value = u64::from_ne_bytes(bytes);
        let mut bits = requested;
        while bits != 0 {
            let bit = bits.trailing_zeros();
            let pfn = word
                .checked_mul(64)
                .and_then(|base| base.checked_add(bit as u64))
                .context("page-idle PFN overflow")?;
            result.insert(pfn, value & (1_u64 << bit) != 0);
            bits &= bits - 1;
        }
    }
    Ok(result)
}

fn bitmap_words(pfns: impl Iterator<Item = u64>) -> BTreeMap<u64, u64> {
    let mut words = BTreeMap::new();
    for pfn in pfns {
        *words.entry(pfn / 64).or_insert(0) |= 1_u64 << (pfn % 64);
    }
    words
}

fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        let count = file.read_at(buffer, offset)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short page-tracking read",
            ));
        }
        offset += count as u64;
        buffer = &mut buffer[count..];
    }
    Ok(())
}

fn write_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        let count = file.write_at(buffer, offset)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short page-idle write",
            ));
        }
        offset += count as u64;
        buffer = &buffer[count..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeIdlePageTrackingIo {
        scans: std::sync::Mutex<std::collections::VecDeque<BTreeMap<u64, u64>>>,
        marked_pfns: std::sync::Mutex<Vec<Vec<u64>>>,
        idle_bits: BTreeMap<u64, bool>,
    }

    impl IdlePageTrackingIo for FakeIdlePageTrackingIo {
        fn scan_present_pages(
            &self,
            _target: &IdlePageTrackingTarget,
        ) -> Result<BTreeMap<u64, u64>> {
            self.scans
                .lock()
                .expect("fake scan queue mutex is not poisoned")
                .pop_front()
                .context("fake backend ran out of pagemap scans")
        }

        fn mark_pfns_idle(&self, pfns: &[u64]) -> Result<()> {
            self.marked_pfns
                .lock()
                .expect("fake marked-PFN mutex is not poisoned")
                .push(pfns.to_vec());
            Ok(())
        }

        fn read_idle_bits(&self, _pfns: &[u64]) -> Result<BTreeMap<u64, bool>> {
            Ok(self.idle_bits.clone())
        }
    }

    fn target() -> IdlePageTrackingTarget {
        IdlePageTrackingTarget {
            firecracker_pid: 1,
            regions: vec![GuestRamRegion {
                base_host_virt_addr: 0x1000,
                guest_phys_addr: 0,
                size: PAGE_SIZE * 8,
                page_size: PAGE_SIZE,
            }],
        }
    }

    #[tokio::test]
    async fn profiling_sessions_are_serialized_for_the_whole_observation_window() {
        let first = IdlePageTracker::new(target()).await.unwrap();
        let waiting = tokio::spawn(async { IdlePageTracker::new(target()).await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !waiting.is_finished(),
            "a second profiling session acquired the global page-idle lock"
        );

        drop(first);
        let second = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("second profiling session did not acquire the released lock")
            .expect("second profiling task panicked");
        assert!(second.is_ok());
    }

    #[tokio::test]
    async fn fake_backend_exercises_baseline_and_observation_without_linux_privileges() {
        let io = FakeIdlePageTrackingIo {
            scans: std::sync::Mutex::new(std::collections::VecDeque::from([
                BTreeMap::from([(0, 11), (PAGE_SIZE, 12)]),
                BTreeMap::from([(0, 11), (PAGE_SIZE, 13), (PAGE_SIZE * 2, 14)]),
            ])),
            marked_pfns: std::sync::Mutex::new(Vec::new()),
            idle_bits: BTreeMap::from([(11, false)]),
        };
        let mut tracker = IdlePageTracker::new_with_io(target(), io).await.unwrap();

        let present = tracker.capture_baseline_with_io().unwrap();
        assert_eq!(present, BTreeMap::from([(0, 11), (PAGE_SIZE, 12)]));
        tracker.baseline = present
            .into_iter()
            .map(|(gpa, pfn)| (gpa, BaselinePage { pfn }))
            .collect();

        let observation = tracker.observe_with_io().unwrap();
        assert_eq!(observation.accessed_baseline_gpas, BTreeSet::from([0]));
        assert_eq!(
            observation.replaced_baseline_gpas,
            BTreeSet::from([PAGE_SIZE])
        );
        assert_eq!(
            observation.newly_present_gpas,
            BTreeSet::from([PAGE_SIZE, PAGE_SIZE * 2])
        );
        assert_eq!(
            tracker
                .io
                .marked_pfns
                .lock()
                .expect("fake marked-PFN mutex is not poisoned")
                .as_slice(),
            &[vec![11, 12]]
        );
    }

    #[test]
    fn pagemap_entry_parser_handles_absent_present_and_masked_pfns() {
        assert_eq!(parse_present_pfn(0).unwrap(), None);
        assert_eq!(parse_present_pfn(PAGEMAP_PRESENT | 42).unwrap(), Some(42));
        assert!(parse_present_pfn(PAGEMAP_PRESENT).is_err());
    }

    #[test]
    fn bitmap_words_groups_pfns_without_losing_bits() {
        assert_eq!(
            bitmap_words([1, 63, 64, 65, 64].into_iter()),
            BTreeMap::from([(0, (1_u64 << 1) | (1_u64 << 63)), (1, 0b11)])
        );
    }

    #[test]
    fn observation_classifies_accessed_new_replaced_and_disappeared_pages() {
        let baseline = BTreeMap::from([
            (0, BaselinePage { pfn: 10 }),
            (PAGE_SIZE, BaselinePage { pfn: 20 }),
            (PAGE_SIZE * 2, BaselinePage { pfn: 30 }),
        ]);
        let current = BTreeMap::from([(0, 10), (PAGE_SIZE, 21), (PAGE_SIZE * 3, 40)]);
        let idle_bits = BTreeMap::from([(10, false)]);

        let observation = classify_observation(&baseline, current, &idle_bits).unwrap();
        assert_eq!(observation.accessed_baseline_gpas, BTreeSet::from([0]));
        assert_eq!(
            observation.replaced_baseline_gpas,
            BTreeSet::from([PAGE_SIZE])
        );
        assert_eq!(
            observation.newly_present_gpas,
            BTreeSet::from([PAGE_SIZE, PAGE_SIZE * 3])
        );
        assert_eq!(
            observation.no_longer_present_gpas,
            BTreeSet::from([PAGE_SIZE * 2])
        );
        assert_eq!(
            observation.tracked_pages(),
            BTreeSet::from([0, PAGE_SIZE, PAGE_SIZE * 3])
        );
    }

    #[test]
    fn observation_requires_idle_bitmap_for_stable_baseline_page() {
        let baseline = BTreeMap::from([(0, BaselinePage { pfn: 10 })]);
        let err = classify_observation(&baseline, BTreeMap::from([(0, 10)]), &BTreeMap::new())
            .expect_err("stable baseline page requires its idle bit");
        assert!(err.to_string().contains("missing idle bitmap"));
    }

    #[test]
    fn coalesce_emits_sorted_manifest_ranges_and_rejects_unaligned_gpas() {
        let ranges = coalesce_gpa_pages(&BTreeSet::from([0, PAGE_SIZE, PAGE_SIZE * 3])).unwrap();
        assert_eq!(
            ranges,
            vec![
                super::super::manifest::GuestMemoryRange {
                    gpa: 0,
                    size: PAGE_SIZE * 2,
                },
                super::super::manifest::GuestMemoryRange {
                    gpa: PAGE_SIZE * 3,
                    size: PAGE_SIZE,
                },
            ]
        );
        assert!(coalesce_gpa_pages(&BTreeSet::from([1])).is_err());
    }

    #[test]
    fn working_set_generation_applies_budget_without_silent_truncation() {
        let observation = IdlePageTrackingObservation {
            accessed_baseline_gpas: BTreeSet::from([0, PAGE_SIZE]),
            ..Default::default()
        };
        let regions = [super::super::manifest::GuestMemoryRegion {
            base_host_virt_addr: 0x1000,
            guest_phys_addr: 0,
            size: PAGE_SIZE * 4,
            page_size: PAGE_SIZE,
        }];
        let accepted = observation
            .to_working_set(
                &regions,
                super::super::manifest::GuestMemoryWorkingSetLimits {
                    max_bytes: PAGE_SIZE * 2,
                    max_ranges: 1,
                    max_guest_memory_ratio_percent: 100,
                },
            )
            .unwrap();
        assert_eq!(accepted.ranges.len(), 1);
        assert_eq!(accepted.ranges[0].size, PAGE_SIZE * 2);

        let err = observation
            .to_working_set(
                &regions,
                super::super::manifest::GuestMemoryWorkingSetLimits {
                    max_bytes: PAGE_SIZE,
                    max_ranges: 1,
                    max_guest_memory_ratio_percent: 100,
                },
            )
            .expect_err("over-budget working set must be rejected as a whole");
        assert!(err.to_string().contains("exceeds configured maximum"));
    }

    #[test]
    fn region_validation_rejects_overlapping_hva_and_gpa_ranges() {
        let first = GuestRamRegion {
            base_host_virt_addr: 0x1000,
            guest_phys_addr: 0,
            size: PAGE_SIZE * 2,
            page_size: PAGE_SIZE,
        };
        let overlapping_hva = GuestRamRegion {
            base_host_virt_addr: 0x2000,
            guest_phys_addr: PAGE_SIZE * 4,
            size: PAGE_SIZE,
            page_size: PAGE_SIZE,
        };
        assert!(validate_regions(&[first.clone(), overlapping_hva]).is_err());

        let overlapping_gpa = GuestRamRegion {
            base_host_virt_addr: 0x4000,
            guest_phys_addr: PAGE_SIZE,
            size: PAGE_SIZE,
            page_size: PAGE_SIZE,
        };
        assert!(validate_regions(&[first, overlapping_gpa]).is_err());
    }
}
