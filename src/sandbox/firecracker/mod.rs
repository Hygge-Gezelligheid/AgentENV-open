//! Firecracker-based sandbox backend implementation.
//!
//! Provides [`FirecrackerSandbox`] (the concrete VM-backed sandbox) and
//! [`FirecrackerSandboxFactory`] which wires sandbox configuration from the
//! global [`ConfigManager`][crate::cfg::ConfigManager].

mod config;
mod connector;
mod factory;
// The tracker is production code but cannot be wired into a real Firecracker
// lifecycle until the public client exposes GPA regions and pre-fault.
#[allow(dead_code)]
mod idle_page_tracking;
mod instance;
mod manifest;
mod mincore_tracking;
mod mmds;
mod overlaybd_snapshot;
mod pool;
#[allow(dead_code)]
mod prefault;
mod process_vm_reader;
mod sandbox;
mod socket;

pub use config::{
    FirecrackerCommonConfig, FirecrackerRuntimePolicy, FirecrackerSandboxConfig,
    FirecrackerSnapshotConfig,
};
pub use factory::FirecrackerSandboxFactory;
pub(super) use instance::FirecrackerInstance;
pub use manifest::{FirecrackerSnapshotManifest, GuestMemoryWorkingSetLimits};
pub use pool::FirecrackerPool;
pub use sandbox::{FirecrackerCapturedSnapshot, FirecrackerPausedState, FirecrackerSandbox};
