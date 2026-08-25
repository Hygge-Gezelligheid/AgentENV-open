//! Restore-time pre-fault decision logic.
//!
//! This module intentionally builds an API-independent request plan. The public
//! Firecracker client currently lacks the GPA-region and pre-fault endpoint
//! needed to submit it, so callers safely fall back to normal resume.

use super::manifest::{GuestMemoryRegion, GuestMemoryWorkingSet, GuestMemoryWorkingSetLimits};
use anyhow::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrefaultSkipReason {
    Disabled,
    UnsupportedArchitecture,
    NoWorkingSet,
    EmptyWorkingSet,
    InvalidWorkingSet,
    FirecrackerApiUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrefaultPlan {
    Skip(PrefaultSkipReason),
    Request {
        ranges: Vec<super::manifest::GuestMemoryRange>,
        bytes: u64,
    },
}

/// Build a validated all-or-nothing pre-fault plan. A malformed range, budget
/// overrun, or RAM-layout mismatch skips the complete hint instead of truncating
/// arbitrary GPA ranges.
pub(crate) fn build_prefault_plan(
    enabled: bool,
    is_x86_64: bool,
    working_set: Option<&GuestMemoryWorkingSet>,
    regions: &[GuestMemoryRegion],
    limits: GuestMemoryWorkingSetLimits,
    firecracker_api_available: bool,
) -> PrefaultPlan {
    if !enabled {
        return PrefaultPlan::Skip(PrefaultSkipReason::Disabled);
    }
    if !is_x86_64 {
        return PrefaultPlan::Skip(PrefaultSkipReason::UnsupportedArchitecture);
    }
    let Some(working_set) = working_set else {
        return PrefaultPlan::Skip(PrefaultSkipReason::NoWorkingSet);
    };
    if working_set.ranges.is_empty() {
        return PrefaultPlan::Skip(PrefaultSkipReason::EmptyWorkingSet);
    }
    if working_set.validate_for_regions(regions, limits).is_err() {
        return PrefaultPlan::Skip(PrefaultSkipReason::InvalidWorkingSet);
    }
    if !firecracker_api_available {
        return PrefaultPlan::Skip(PrefaultSkipReason::FirecrackerApiUnavailable);
    }
    let bytes = working_set
        .total_bytes()
        .expect("working-set validation already checked total bytes");
    PrefaultPlan::Request {
        ranges: working_set.ranges.clone(),
        bytes,
    }
}

/// Result of attempting a real pre-fault request. A request error never blocks
/// resume; the caller records the outcome and continues with the normal path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrefaultOutcome {
    Skipped(PrefaultSkipReason),
    Applied { range_count: usize, bytes: u64 },
    FailedFallback,
}

/// Execute a plan through the future generated Firecracker client. This is an
/// adapter seam, not a hand-written HTTP client: until the published schema
/// exposes the endpoint, production always uses `FirecrackerApiUnavailable`.
pub(crate) fn execute_prefault_plan(
    plan: PrefaultPlan,
    send_request: impl FnOnce(&[super::manifest::GuestMemoryRange]) -> Result<()>,
) -> PrefaultOutcome {
    match plan {
        PrefaultPlan::Skip(reason) => PrefaultOutcome::Skipped(reason),
        PrefaultPlan::Request { ranges, bytes } => match send_request(&ranges) {
            Ok(()) => PrefaultOutcome::Applied {
                range_count: ranges.len(),
                bytes,
            },
            Err(_) => PrefaultOutcome::FailedFallback,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::firecracker::manifest::{GuestMemoryRange, GUEST_MEMORY_PAGE_SIZE};

    fn region() -> GuestMemoryRegion {
        GuestMemoryRegion {
            base_host_virt_addr: 0x1000_0000,
            guest_phys_addr: 0,
            size: 0x4000,
            page_size: GUEST_MEMORY_PAGE_SIZE,
        }
    }

    fn limits() -> GuestMemoryWorkingSetLimits {
        GuestMemoryWorkingSetLimits {
            max_bytes: 0x4000,
            max_ranges: 4,
            max_guest_memory_ratio_percent: 100,
        }
    }

    #[test]
    fn no_metadata_or_empty_ranges_never_issue_request() {
        assert_eq!(
            build_prefault_plan(true, true, None, &[region()], limits(), true),
            PrefaultPlan::Skip(PrefaultSkipReason::NoWorkingSet)
        );
        assert_eq!(
            build_prefault_plan(
                true,
                true,
                Some(&GuestMemoryWorkingSet::new(vec![])),
                &[region()],
                limits(),
                true,
            ),
            PrefaultPlan::Skip(PrefaultSkipReason::EmptyWorkingSet)
        );
    }

    #[test]
    fn invalid_metadata_and_missing_api_fall_back() {
        let invalid = GuestMemoryWorkingSet::new(vec![GuestMemoryRange {
            gpa: 0x4000,
            size: GUEST_MEMORY_PAGE_SIZE,
        }]);
        assert_eq!(
            build_prefault_plan(true, true, Some(&invalid), &[region()], limits(), true),
            PrefaultPlan::Skip(PrefaultSkipReason::InvalidWorkingSet)
        );

        let valid = GuestMemoryWorkingSet::new(vec![GuestMemoryRange {
            gpa: 0,
            size: GUEST_MEMORY_PAGE_SIZE,
        }]);
        assert_eq!(
            build_prefault_plan(true, true, Some(&valid), &[region()], limits(), false),
            PrefaultPlan::Skip(PrefaultSkipReason::FirecrackerApiUnavailable)
        );
    }

    #[test]
    fn valid_request_requires_enabled_x86_and_api() {
        let working_set = GuestMemoryWorkingSet::new(vec![GuestMemoryRange {
            gpa: 0,
            size: GUEST_MEMORY_PAGE_SIZE,
        }]);
        assert_eq!(
            build_prefault_plan(false, true, Some(&working_set), &[region()], limits(), true),
            PrefaultPlan::Skip(PrefaultSkipReason::Disabled)
        );
        assert_eq!(
            build_prefault_plan(true, false, Some(&working_set), &[region()], limits(), true),
            PrefaultPlan::Skip(PrefaultSkipReason::UnsupportedArchitecture)
        );
        assert!(matches!(
            build_prefault_plan(true, true, Some(&working_set), &[region()], limits(), true),
            PrefaultPlan::Request { bytes: 4096, .. }
        ));
    }

    #[test]
    fn request_errors_fall_back_without_blocking_resume() {
        let ranges = vec![GuestMemoryRange {
            gpa: 0,
            size: GUEST_MEMORY_PAGE_SIZE,
        }];
        let applied = execute_prefault_plan(
            PrefaultPlan::Request {
                ranges: ranges.clone(),
                bytes: GUEST_MEMORY_PAGE_SIZE,
            },
            |request| {
                assert_eq!(request, ranges.as_slice());
                Ok(())
            },
        );
        assert_eq!(
            applied,
            PrefaultOutcome::Applied {
                range_count: 1,
                bytes: GUEST_MEMORY_PAGE_SIZE,
            }
        );

        let fallback = execute_prefault_plan(
            PrefaultPlan::Request {
                ranges,
                bytes: GUEST_MEMORY_PAGE_SIZE,
            },
            |_| Err(anyhow::anyhow!("pre-fault endpoint rejected the request")),
        );
        assert_eq!(fallback, PrefaultOutcome::FailedFallback);

        let skipped =
            execute_prefault_plan(PrefaultPlan::Skip(PrefaultSkipReason::Disabled), |_| {
                panic!("skipped plan must not send a request")
            });
        assert_eq!(
            skipped,
            PrefaultOutcome::Skipped(PrefaultSkipReason::Disabled)
        );
    }
}
