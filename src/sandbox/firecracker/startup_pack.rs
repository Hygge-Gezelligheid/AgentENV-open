//! Startup pack recording orchestration.
//!
//! After a snapshot is captured (and the source sandbox is running again),
//! boot one throwaway VM from the captured snapshot with a dedicated memory
//! device so the daemon-side recorder sees every first-touch read while all
//! memory layers are still node-local. The recorder emits a first-touch trace
//! file; the publisher expands it into the v3 startup manifest (exact-order
//! prefix plus merged ranges) and uploads just that list. The whole flow is
//! best-effort: any failure returns `None` and the publish continues without
//! a manifest.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tracing::{debug, info, warn, Instrument};
use uvm_ublk_daemon::protocol::PackRecordingState;

use super::{FirecrackerSandbox, FirecrackerSnapshotConfig};
use crate::cfg::ConfigManager;
use crate::sandbox::ublk::{SharedReadOnlyDevice, UblkDeviceManager};
use crate::snapshot::MEMORY_STARTUP_TRACE_ARTIFACT;

/// Recording is opt-in for every repository backend. The publisher selects
/// the durable manifest destination appropriate to its backend.
fn recording_enabled_for(config: &crate::cfg::SnapshotConfig) -> bool {
    config.memory_startup_pack.enabled
}

fn recording_enabled() -> bool {
    recording_enabled_for(&ConfigManager::global_config().snapshot)
}

/// Record the first-touch trace for a just-captured snapshot.
///
/// Returns the trace path (`{snapshot_dir}/memory-startup.trace`) on success,
/// `None` on any failure or timeout. Cleanup (abort, VM stop, partial files)
/// always completes and is never bounded by the recording budget.
pub(crate) async fn record_startup_pack(
    mut config: FirecrackerSnapshotConfig,
    snapshot_dir: PathBuf,
) -> Option<PathBuf> {
    if !recording_enabled() {
        return None;
    }
    info!(dir = %snapshot_dir.display(), "startup pack recording started");
    let budget_secs = ConfigManager::global_config()
        .snapshot
        .memory_startup_pack
        .record_budget_secs;
    let trace_path = snapshot_dir.join(MEMORY_STARTUP_TRACE_ARTIFACT);

    let recording_config = match derive_recording_mem_config(
        &config.mem_overlaybd_config.image_config_path,
        &snapshot_dir,
    )
    .await
    {
        Ok(path) => path,
        Err(error) => {
            warn!(%error, "startup pack: derive recording mem config failed");
            return None;
        }
    };
    config.mem_overlaybd_config.image_config_path = recording_config.clone();
    config.pack_recording = true;

    let outcome = boot_and_wait(config, &trace_path, budget_secs).await;

    if outcome.is_none() {
        cleanup_pack_files(&trace_path, &recording_config).await;
        return None;
    }
    // Success: the derived recording config is no longer needed (the trace
    // stays next to the snapshot artifacts for the publisher).
    if let Err(error) = tokio::fs::remove_file(&recording_config).await {
        debug!(%error, "startup pack: remove derived recording config failed");
    }
    info!(path = %trace_path.display(), "startup trace recorded");
    Some(trace_path)
}

/// Boot the recording VM, wait (bounded) for the daemon-side window, then
/// clean up (unbounded). The VM handle and device id live outside the
/// timeout scope so the cleanup path always runs to completion.
async fn boot_and_wait(
    config: FirecrackerSnapshotConfig,
    trace_path: &Path,
    budget_secs: u64,
) -> Option<PathBuf> {
    let phase_t0 = std::time::Instant::now();
    let mut recording_vm = match FirecrackerSandbox::from_snapshot_config(&config) {
        Ok(vm) => vm,
        Err(error) => {
            warn!(%error, "startup pack: build recording VM failed");
            return None;
        }
    };
    if let Err(error) = recording_vm.start_nowait().await {
        warn!(%error, "startup pack: recording VM start failed");
        // Best-effort teardown of whatever start_nowait managed to create.
        if let Err(stop_error) = recording_vm.stop().await {
            warn!(%stop_error, "startup pack: recording VM stop after start failure failed");
        }
        return None;
    }
    let Some(dev_id) = recording_vm.dedicated_mem_device_id() else {
        warn!("startup pack: recording VM has no dedicated memory device");
        if let Err(stop_error) = recording_vm.stop().await {
            warn!(%stop_error, "startup pack: recording VM stop failed");
        }
        return None;
    };

    info!(
        elapsed_ms = phase_t0.elapsed().as_millis() as u64,
        "startup pack: recording VM started; status wait begins"
    );
    // Only this wait is bounded by the budget.
    let wait = async {
        loop {
            match UblkDeviceManager::global()
                .pack_recording_status(dev_id)
                .await
            {
                Ok(PackRecordingState::Done { pages, bytes, .. }) => {
                    break Some((pages, bytes));
                }
                Ok(PackRecordingState::Failed { reason }) => {
                    warn!(%reason, "startup pack: daemon recording failed");
                    break None;
                }
                Ok(PackRecordingState::Recording) => {}
                Err(error) => {
                    warn!(%error, "startup pack: recording status poll failed");
                    break None;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    let outcome = tokio::select! {
        outcome = tokio::time::timeout(Duration::from_secs(budget_secs), wait) => {
            match outcome {
                Ok(done) => WaitOutcome::Done(done),
                Err(_) => WaitOutcome::Timeout,
            }
        }
        _ = crate::snapshot::startup_pack::startup_manifest_abort_notify() => {
            WaitOutcome::Shutdown
        }
    };
    info!(
        elapsed_ms = phase_t0.elapsed().as_millis() as u64,
        "startup pack: status wait ended"
    );

    // Cleanup is never truncated by the budget on the normal paths — except
    // the daemon abort RPC, which is always time-boxed: it is best-effort
    // (stopping the VM deletes the device and makes the daemon abort the
    // recording anyway), and a dying daemon must never hang this task.
    let succeeded = matches!(outcome, WaitOutcome::Done(Some(_)));
    if !succeeded {
        let abort = async {
            if let Err(error) = UblkDeviceManager::global()
                .abort_pack_recording(dev_id)
                .await
            {
                warn!(%error, "startup pack: abort recording failed");
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(3), abort).await;
    }
    let stop = async {
        if let Err(error) = recording_vm.stop().await {
            warn!(%error, "startup pack: recording VM stop failed");
        }
    };
    if matches!(outcome, WaitOutcome::Shutdown) {
        let _ = tokio::time::timeout(Duration::from_secs(5), stop).await;
    } else {
        stop.await;
    }
    info!(
        elapsed_ms = phase_t0.elapsed().as_millis() as u64,
        "startup pack: recording VM stopped"
    );

    match outcome {
        WaitOutcome::Done(Some((pages, bytes))) => {
            debug!(pages, bytes, "startup pack recording finished");

            Some(trace_path.to_path_buf())
        }
        WaitOutcome::Done(None) => None,
        WaitOutcome::Timeout => {
            warn!(budget_secs, "startup pack: recording budget exceeded");
            None
        }
        WaitOutcome::Shutdown => None,
    }
}

enum WaitOutcome {
    Done(Option<(u32, u64)>),
    Timeout,
    Shutdown,
}

async fn cleanup_pack_files(trace_path: &Path, recording_config: &Path) {
    let mut tmp = trace_path.as_os_str().to_owned();
    tmp.push(".tmp");
    for path in [trace_path, Path::new(&tmp), recording_config] {
        if let Err(error) = tokio::fs::remove_file(path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                debug!(%error, path = %path.display(), "startup pack: cleanup remove failed");
            }
        }
    }
}

/// Derive a recording variant of the captured memory image config: same
/// lowers, background download force-disabled. Recording-time foreground
/// reads are still allowed (chain snapshots have remote parents fetched
/// on demand), but nothing should trigger a background bulk download.
async fn derive_recording_mem_config(src: &Path, snapshot_dir: &Path) -> Result<PathBuf> {
    let raw = tokio::fs::read(src)
        .await
        .with_context(|| format!("read memory image config {}", src.display()))?;
    let mut image_config: overlaybd::config::ImageConfig =
        serde_json::from_slice(&raw).context("parse memory image config")?;
    image_config.download_override = Some(overlaybd::config::DownloadConfig {
        enable: false,
        ..Default::default()
    });
    let derived = snapshot_dir.join("mem_image.pack-rec.json");
    tokio::fs::write(&derived, serde_json::to_vec_pretty(&image_config)?)
        .await
        .with_context(|| format!("write recording mem config {}", derived.display()))?;
    Ok(derived)
}

/// A subscriber to a shared, bounded local page-cache warmup.
///
/// Each restoring sandbox owns a subscriber. A subscriber leaving cancels only
/// its own interest; the actual reads stop only after the final subscriber has
/// gone away. This keeps one teardown from aborting another restore.
pub(crate) struct LocalStartupPrefetch {
    cancel: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl LocalStartupPrefetch {
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub(crate) async fn stop(mut self) {
        self.cancel();
        // A blocking device read cannot be aborted. In particular, do not let
        // stop release the last device reference before the reader has drained.
        let _ = (&mut self.task).await;
    }
}

/// A stalled device read must not prevent Firecracker from receiving its stop
/// request. The caller drains prefetch only after this future has completed.
pub(crate) async fn stop_vm_after_prefetch_cancel<F>(
    prefetch: Option<&LocalStartupPrefetch>,
    stop_vm: F,
) -> Result<()>
where
    F: std::future::Future<Output = Result<()>>,
{
    if let Some(prefetch) = prefetch {
        prefetch.cancel();
    }
    stop_vm.await
}

impl Drop for LocalStartupPrefetch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        // Dropping a JoinHandle detaches, rather than aborts, the cleanup task.
        // It retains the device until any in-kernel read has returned.
    }
}

const LOCAL_PREFETCH_MAX_READS: usize = 4;

struct SharedLocalPrefetch {
    subscribers: std::sync::atomic::AtomicUsize,
    active_parts: std::sync::atomic::AtomicUsize,
    cancel: AtomicBool,
    finished: AtomicBool,
}

/// Completion is published only after both the async supervisor and any
/// in-kernel blocking reader have exited, including panic/cancellation paths.
struct LocalPrefetchCompletion(Arc<SharedLocalPrefetch>);

impl Drop for LocalPrefetchCompletion {
    fn drop(&mut self) {
        if self.0.active_parts.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.finished.store(true, Ordering::Release);
        }
    }
}

struct LocalPrefetchLease {
    identity: String,
    shared: Arc<SharedLocalPrefetch>,
    released: bool,
}

impl LocalPrefetchLease {
    /// Release this subscriber while holding the registry lock that protects
    /// acquisition. The final release removes this run before exposing its
    /// cancellation, so a new restore starts a replacement instead of joining
    /// a draining, permanently-cancelled worker.
    fn release(&mut self) -> bool {
        if self.released {
            return false;
        }
        self.released = true;
        let mut runs = local_prefetches()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if self.shared.subscribers.fetch_sub(1, Ordering::AcqRel) != 1 {
            return false;
        }
        self.shared.cancel.store(true, Ordering::Release);
        let maps_to_this_run = runs
            .get(&self.identity)
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|run| Arc::ptr_eq(&run, &self.shared));
        if maps_to_this_run {
            runs.remove(&self.identity);
        }
        true
    }
}

impl Drop for LocalPrefetchLease {
    fn drop(&mut self) {
        self.release();
    }
}

struct PreparedLocalPrefetch {
    identity: String,
    manifest: overlaybd::startup_manifest::StartupManifest,
    max_pack_bytes: u64,
    deadline: tokio::time::Instant,
}

fn local_prefetches() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, std::sync::Weak<SharedLocalPrefetch>>,
> {
    static RUNS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<SharedLocalPrefetch>>>,
    > = std::sync::OnceLock::new();
    RUNS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn local_prefetch_read_permits() -> &'static Arc<tokio::sync::Semaphore> {
    static PERMITS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    PERMITS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(LOCAL_PREFETCH_MAX_READS)))
}

fn acquire_local_prefetch(identity: String) -> (Arc<SharedLocalPrefetch>, bool) {
    let mut runs = local_prefetches()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    runs.retain(|_, run| run.strong_count() != 0);
    if let Some(shared) = runs.get(&identity).and_then(std::sync::Weak::upgrade) {
        if !shared.cancel.load(Ordering::Acquire) && !shared.finished.load(Ordering::Acquire) {
            shared.subscribers.fetch_add(1, Ordering::AcqRel);
            return (shared, false);
        }
        runs.remove(&identity);
    }
    let shared = Arc::new(SharedLocalPrefetch {
        subscribers: std::sync::atomic::AtomicUsize::new(1),
        active_parts: std::sync::atomic::AtomicUsize::new(1),
        cancel: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });
    runs.insert(identity, Arc::downgrade(&shared));
    (shared, true)
}

async fn wait_for_local_prefetch_finished(shared: &SharedLocalPrefetch) {
    while !shared.finished.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Submit buffered memory-device reads without delaying resume. Sharing includes
/// descriptor SHA-256, final local layer identities and live device generation.
pub(crate) fn submit_local_startup_prefetch(
    device: SharedReadOnlyDevice,
    image_config: PathBuf,
    global_config: PathBuf,
    pack: crate::snapshot::ResolvedStartupPack,
) -> LocalStartupPrefetch {
    let cancel = Arc::new(AtomicBool::new(false));
    let subscriber_cancel = Arc::clone(&cancel);
    let task = tokio::spawn(
        async move {
            if let Err(error) = local_device_startup_prefetch(
                device,
                image_config,
                global_config,
                pack,
                subscriber_cancel,
            )
            .await
            {
                warn!(%error, "local startup manifest prefetch failed; resuming on demand");
            }
        }
        .in_current_span(),
    );
    LocalStartupPrefetch { cancel, task }
}
async fn local_device_startup_prefetch(
    device: SharedReadOnlyDevice,
    image_config: PathBuf,
    global_config: PathBuf,
    pack: crate::snapshot::ResolvedStartupPack,
    subscriber_cancel: Arc<AtomicBool>,
) -> Result<()> {
    let timeout = Duration::from_secs(
        ConfigManager::global_config()
            .snapshot
            .memory_startup_pack
            .consume_timeout_secs,
    );
    let deadline = tokio::time::Instant::now() + timeout;
    let outcome = async {
        let prepared = match tokio::time::timeout(
            timeout,
            prepare_local_prefetch(image_config, global_config, pack, &subscriber_cancel),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                warn!("local startup manifest prefetch preparation timed out");
                return Ok(());
            }
        };
        let Some(mut prepared) = prepared else {
            return Ok(());
        };
        prepared.deadline = deadline;
        local_startup_prefetch_inner(prepared, device.clone(), subscriber_cancel).await
    }
    .await;
    device.release().await?;
    outcome
}

async fn local_startup_prefetch_inner(
    mut prepared: PreparedLocalPrefetch,
    device: SharedReadOnlyDevice,
    subscriber_cancel: Arc<AtomicBool>,
) -> Result<()> {
    // A recycled /dev/ublkbN is not the same cache. The live Arc identity
    // cannot be reused while the worker retains its device lease.
    prepared
        .identity
        .push_str(&format!("|device:{}", device.prefetch_identity()));
    let (shared, leader) = acquire_local_prefetch(prepared.identity.clone());
    let mut lease = LocalPrefetchLease {
        identity: prepared.identity.clone(),
        shared: Arc::clone(&shared),
        released: false,
    };
    if leader {
        let worker = Arc::clone(&shared);
        let completion = LocalPrefetchCompletion(Arc::clone(&worker));
        tokio::spawn(async move {
            let _completion = completion;
            finish_local_prefetch_worker(Arc::clone(&worker), async {
                let result = execute_device_prefetch(
                    prepared,
                    device.device_path().to_path_buf(),
                    Arc::clone(&worker),
                    Some(device.clone()),
                )
                .await;
                let released = device.release().await;
                result.and(released)
            })
            .await;
        });
    } else {
        device.release().await?;
    }
    while !shared.finished.load(Ordering::Acquire) {
        if subscriber_cancel.load(Ordering::Acquire) {
            if lease.release() {
                wait_for_local_prefetch_finished(&shared).await;
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

async fn prepare_local_prefetch(
    image_config: PathBuf,
    global_config: PathBuf,
    pack: crate::snapshot::ResolvedStartupPack,
    subscriber_cancel: &AtomicBool,
) -> Result<Option<PreparedLocalPrefetch>> {
    if subscriber_cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let crate::snapshot::startup_pack::ResolvedStartupPackSource::LocalPath(manifest_path) =
        pack.source
    else {
        return Ok(None);
    };
    let startup_config = &ConfigManager::global_config().snapshot.memory_startup_pack;
    if overlaybd::config::load_global_config(&global_config)?.io_engine == 2 {
        warn!(path = %manifest_path.display(), "skipping memory device startup prefetch: lower O_DIRECT configuration is not yet validated");
        return Ok(None);
    }
    let max_manifest_bytes = overlaybd::startup_manifest::MANIFEST_HEADER_BYTES
        + overlaybd::startup_manifest::MAX_PREFIX_PAGES as usize * 8
        + overlaybd::startup_manifest::MAX_RANGES as usize * 16;
    anyhow::ensure!(
        pack.pack_size <= max_manifest_bytes as u64,
        "startup manifest descriptor exceeds format limit"
    );
    // Limit bytes actually read, not just pre-read metadata: a local file may
    // grow after a metadata check and must not cause unbounded allocation.
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(&manifest_path).await?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(max_manifest_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    anyhow::ensure!(
        bytes.len() <= max_manifest_bytes,
        "startup manifest exceeds format limit"
    );
    if subscriber_cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    anyhow::ensure!(
        bytes.len() as u64 == pack.pack_size,
        "startup manifest size changed"
    );
    anyhow::ensure!(
        crate::snapshot::startup_pack::hex_sha256(&bytes) == pack.index_sha256,
        "startup manifest sha256 mismatch"
    );
    let manifest = overlaybd::startup_manifest::decode_manifest(&bytes)?;
    anyhow::ensure!(
        manifest.mem_virtual_size == pack.mem_virtual_size,
        "startup manifest memory size mismatch"
    );
    let image = overlaybd::config::load_image_config(&image_config)?;
    if subscriber_cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let layers = image
        .lowers
        .iter()
        .filter(|layer| !layer.file.is_empty())
        .map(|layer| overlaybd::pack_planner::FinalLayerSource {
            digest: layer.digest.clone(),
            size: layer.size,
            source: overlaybd::pack_planner::FinalLayerBytes::LocalPath(PathBuf::from(&layer.file)),
        })
        .collect::<Vec<_>>();
    Ok(Some(PreparedLocalPrefetch {
        identity: local_prefetch_identity(&pack.index_sha256, &layers),
        manifest,
        max_pack_bytes: startup_config.max_pack_bytes,
        deadline: tokio::time::Instant::now()
            + Duration::from_secs(startup_config.consume_timeout_secs),
    }))
}

fn local_prefetch_identity(
    pack_sha256: &str,
    layers: &[overlaybd::pack_planner::FinalLayerSource],
) -> String {
    format!(
        "{}|{}",
        pack_sha256,
        layers
            .iter()
            .map(|layer| match &layer.source {
                overlaybd::pack_planner::FinalLayerBytes::LocalPath(path) =>
                    format!("{}:{}:{}", layer.digest, layer.size, path.display()),
                overlaybd::pack_planner::FinalLayerBytes::VFile(_) => unreachable!(),
            })
            .collect::<Vec<_>>()
            .join("|")
    )
}
/// Read through the same buffered block-device mapping used by Firecracker.
/// Lower-file reads warm a different page cache and cannot satisfy its faults.
async fn execute_device_prefetch(
    prepared: PreparedLocalPrefetch,
    path: PathBuf,
    shared: Arc<SharedLocalPrefetch>,
    device_lease: Option<SharedReadOnlyDevice>,
) -> Result<()> {
    let deadline = prepared.deadline;
    let spans = device_prefetch_spans(&prepared.manifest, prepared.max_pack_bytes);
    let requested_bytes: u64 = spans.iter().map(|(_, len)| len).sum();
    let permit = loop {
        if spans.is_empty()
            || shared.cancel.load(Ordering::Acquire)
            || tokio::time::Instant::now() >= deadline
        {
            return Ok(());
        }
        tokio::select! {
            permit = Arc::clone(local_prefetch_read_permits()).acquire_owned() => break permit?,
            _ = tokio::time::sleep(Duration::from_millis(10)) => {},
        }
    };
    let worker_shared = Arc::clone(&shared);
    shared.active_parts.fetch_add(1, Ordering::AcqRel);
    let completion = LocalPrefetchCompletion(Arc::clone(&shared));
    let worker = tokio::task::spawn_blocking(move || {
        let _completion = completion;
        let _device_lease = device_lease;
        let _permit = permit;
        read_device_prefetch_spans(&path, &spans, deadline, &worker_shared)
    });
    // Never drop this handle on timeout: the caller owns a device lease until
    // this read has actually returned, including after subscriber cancellation.
    let read_bytes = worker
        .await
        .context("device startup prefetch reader panicked")??;
    debug!(
        requested_bytes,
        read_bytes, "memory device startup prefetch drained"
    );
    Ok(())
}

fn device_prefetch_spans(
    manifest: &overlaybd::startup_manifest::StartupManifest,
    budget: u64,
) -> Vec<(u64, u64)> {
    const PAGE: u64 = 4096;
    let mut remaining = budget / PAGE * PAGE;
    let mut spans = Vec::new();
    let mut prefix = std::collections::BTreeSet::new();
    for &page in &manifest.prefix_pages {
        if remaining < PAGE {
            break;
        }
        if prefix.insert(page) {
            spans.push((page, PAGE));
            remaining -= PAGE;
        }
    }
    for &(offset, len) in &manifest.ranges {
        let end = offset + len;
        let mut cursor = offset;
        for &page in prefix.range(offset..end) {
            let count = (page - cursor).min(remaining);
            if count != 0 {
                spans.push((cursor, count));
                remaining -= count;
            }
            cursor = page + PAGE;
        }
        let count = (end - cursor).min(remaining);
        if count != 0 {
            spans.push((cursor, count));
            remaining -= count;
        }
        if remaining == 0 {
            break;
        }
    }
    spans
}

fn read_device_prefetch_spans(
    path: &Path,
    spans: &[(u64, u64)],
    deadline: tokio::time::Instant,
    shared: &SharedLocalPrefetch,
) -> Result<u64> {
    use std::os::unix::fs::FileExt;
    let cancelled =
        || shared.cancel.load(Ordering::Acquire) || tokio::time::Instant::now() >= deadline;
    if cancelled() {
        return Ok(0);
    }
    let file = std::fs::File::open(path)?;
    read_device_prefetch_spans_with(spans, deadline, shared, |buffer, offset| {
        file.read_exact_at(buffer, offset)
    })
}

fn read_device_prefetch_spans_with(
    spans: &[(u64, u64)],
    deadline: tokio::time::Instant,
    shared: &SharedLocalPrefetch,
    mut read: impl FnMut(&mut [u8], u64) -> std::io::Result<()>,
) -> Result<u64> {
    let cancelled =
        || shared.cancel.load(Ordering::Acquire) || tokio::time::Instant::now() >= deadline;
    let mut buffer = vec![0; 1024 * 1024];
    let mut read_bytes = 0;
    for &(offset, len) in spans {
        let mut position = offset;
        while position < offset + len {
            if cancelled() {
                return Ok(read_bytes);
            }
            let count = (offset + len - position).min(buffer.len() as u64) as usize;
            read(&mut buffer[..count], position)?;
            read_bytes += count as u64;
            position += count as u64;
        }
    }
    Ok(read_bytes)
}

async fn finish_local_prefetch_worker<F>(_shared: Arc<SharedLocalPrefetch>, execution: F)
where
    F: std::future::Future<Output = Result<()>>,
{
    match execution.await {
        Ok(()) => {}
        Err(error) => warn!(%error, "local startup manifest prefetch failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::SnapshotRepositoryBackendKind;

    fn test_shared() -> Arc<SharedLocalPrefetch> {
        Arc::new(SharedLocalPrefetch {
            subscribers: std::sync::atomic::AtomicUsize::new(1),
            active_parts: std::sync::atomic::AtomicUsize::new(1),
            cancel: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        })
    }

    #[tokio::test]
    async fn panic_completes_supervisor_without_marking_live_reader_drained() {
        let shared = test_shared();
        shared.active_parts.fetch_add(1, Ordering::AcqRel);
        let reader = LocalPrefetchCompletion(Arc::clone(&shared));
        let supervisor = LocalPrefetchCompletion(Arc::clone(&shared));
        let task = tokio::spawn(async move {
            let _supervisor = supervisor;
            panic!("controlled prefetch supervisor failure");
        });
        assert!(task.await.is_err());
        assert!(!shared.finished.load(Ordering::Acquire));
        drop(reader);
        assert!(shared.finished.load(Ordering::Acquire));
    }

    #[test]
    fn device_spans_preserve_first_touch_order_and_deduplicate_prefix() {
        let manifest = overlaybd::startup_manifest::StartupManifest {
            mem_virtual_size: 32768,
            prefix_pages: vec![8192, 0, 8192],
            ranges: vec![(16384, 8192), (0, 16384)],
        };
        assert_eq!(
            device_prefetch_spans(&manifest, 32768),
            vec![
                (8192, 4096),
                (0, 4096),
                (16384, 8192),
                (4096, 4096),
                (12288, 4096)
            ]
        );
        assert_eq!(
            device_prefetch_spans(&manifest, 13000),
            vec![(8192, 4096), (0, 4096), (16384, 4096)]
        );
        assert!(device_prefetch_spans(&manifest, 0).is_empty());
    }

    #[test]
    fn device_reader_checks_cancellation_between_bounded_reads() -> Result<()> {
        let shared = test_shared();
        let mut calls = Vec::new();
        let bytes = read_device_prefetch_spans_with(
            &[(0, 3 * 1024 * 1024)],
            tokio::time::Instant::now() + Duration::from_secs(10),
            &shared,
            |buffer, offset| {
                calls.push((offset, buffer.len()));
                shared.cancel.store(true, Ordering::Release);
                Ok(())
            },
        )?;
        assert_eq!(calls, vec![(0, 1024 * 1024)]);
        assert_eq!(bytes, 1024 * 1024);
        Ok(())
    }

    #[test]
    fn expired_device_reader_does_not_open_path() -> Result<()> {
        assert_eq!(
            read_device_prefetch_spans(
                Path::new("/missing-expired-device"),
                &[(0, 4096)],
                tokio::time::Instant::now() - Duration::from_secs(1),
                &test_shared()
            )?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn actual_device_executor_reads_ranges_and_reports_failure() -> Result<()> {
        let tmp = tempfile::TempDir::new()?;
        let file = tmp.path().join("device");
        tokio::fs::write(&file, vec![7_u8; 8192]).await?;
        let prepared = || PreparedLocalPrefetch {
            identity: "unit-device".into(),
            manifest: overlaybd::startup_manifest::StartupManifest {
                mem_virtual_size: 8192,
                prefix_pages: vec![4096],
                ranges: vec![(0, 8192)],
            },
            max_pack_bytes: 8192,
            deadline: tokio::time::Instant::now() + Duration::from_secs(5),
        };
        execute_device_prefetch(prepared(), file, test_shared(), None).await?;
        assert!(execute_device_prefetch(
            prepared(),
            tmp.path().join("missing"),
            test_shared(),
            None
        )
        .await
        .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn recording_mem_config_disables_background_download() -> Result<()> {
        let tmp = tempfile::TempDir::new()?;
        let src = tmp.path().join("mem_image.json");
        let image_config = overlaybd::config::ImageConfig {
            repo_blob_url: "https://example/v2/repo/blobs".into(),
            lowers: vec![overlaybd::config::LayerConfig {
                file: "/layers/a.commit".into(),
                digest: "sha256:a".into(),
                size: 4096,
                ..Default::default()
            }],
            ..Default::default()
        };
        tokio::fs::write(&src, serde_json::to_vec_pretty(&image_config)?).await?;

        let derived = derive_recording_mem_config(&src, tmp.path()).await?;

        assert_eq!(derived, tmp.path().join("mem_image.pack-rec.json"));
        let written: overlaybd::config::ImageConfig =
            serde_json::from_slice(&tokio::fs::read(&derived).await?)?;
        let download = written.download_override.expect("download override");
        assert!(!download.enable);
        assert_eq!(written.lowers.len(), 1);
        assert_eq!(written.lowers[0].digest, "sha256:a");
        Ok(())
    }

    #[test]
    fn recording_gate_uses_feature_enablement_for_all_backends() {
        fn startup_pack_config(enabled: bool) -> crate::cfg::SnapshotStartupPackConfig {
            crate::cfg::SnapshotStartupPackConfig {
                enabled,
                record_min_window_ms: 200,
                record_quiet_ms: 300,
                record_max_window_ms: 2000,
                record_budget_secs: 10,
                max_pack_bytes: 1 << 30,
                consume_enabled: false,
                consume_timeout_secs: 30,
            }
        }

        let default_config = crate::cfg::SnapshotConfig::default();
        assert!(
            !recording_enabled_for(&default_config),
            "feature must be off by default"
        );

        let posix_enabled = crate::cfg::SnapshotConfig {
            repository_backend: SnapshotRepositoryBackendKind::PosixFs,
            memory_startup_pack: startup_pack_config(true),
            ..Default::default()
        };
        assert!(
            recording_enabled_for(&posix_enabled),
            "enabled=true with posix_fs backend must record"
        );

        let oss_enabled = crate::cfg::SnapshotConfig {
            repository_backend: SnapshotRepositoryBackendKind::Oss,
            memory_startup_pack: startup_pack_config(true),
            ..Default::default()
        };
        assert!(
            recording_enabled_for(&oss_enabled),
            "enabled=true with oss backend must record"
        );

        let oss_disabled = crate::cfg::SnapshotConfig {
            repository_backend: SnapshotRepositoryBackendKind::Oss,
            memory_startup_pack: startup_pack_config(false),
            ..Default::default()
        };
        assert!(
            !recording_enabled_for(&oss_disabled),
            "enabled=false with oss backend must NOT record"
        );
    }

    #[tokio::test]
    async fn cancelled_prefetch_never_opens_invalid_paths() -> Result<()> {
        let cancelled = Arc::new(AtomicBool::new(true));
        let missing = PathBuf::from("/definitely-missing-startup-layer");
        let pack = crate::snapshot::ResolvedStartupPack {
            source: crate::snapshot::startup_pack::ResolvedStartupPackSource::LocalPath(
                missing.clone(),
            ),
            pack_size: 0,
            mem_virtual_size: 0,
            index_sha256: String::new(),
        };
        assert!(
            prepare_local_prefetch(missing.clone(), missing, pack, &cancelled)
                .await?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn stop_signals_and_joins_local_prefetch_task() {
        let cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = Arc::clone(&cancel);
        let finished = Arc::new(AtomicBool::new(false));
        let task_finished = Arc::clone(&finished);
        let task = tokio::spawn(async move {
            while !task_cancel.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
            task_finished.store(true, Ordering::Release);
        });
        LocalStartupPrefetch { cancel, task }.stop().await;
        assert!(finished.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn vm_stop_precedes_blocked_reader_drain_and_device_release() -> Result<()> {
        let cancel = Arc::new(AtomicBool::new(false));
        let read_started = Arc::new(AtomicBool::new(false));
        let allow_read_finish = Arc::new(AtomicBool::new(false));
        let device = Arc::new(());
        let held_device = Arc::clone(&device);
        let started = Arc::clone(&read_started);
        let allow = Arc::clone(&allow_read_finish);
        let read = tokio::task::spawn_blocking(move || {
            started.store(true, Ordering::Release);
            while !allow.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(held_device);
        });
        let task = tokio::spawn(async move {
            read.await.expect("blocking reader panicked");
        });
        let prefetch = LocalStartupPrefetch {
            cancel: Arc::clone(&cancel),
            task,
        };
        while !read_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }

        let vm_stopped = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&vm_stopped);
        stop_vm_after_prefetch_cancel(Some(&prefetch), async move {
            stopped.store(true, Ordering::Release);
            Ok(())
        })
        .await?;
        assert!(cancel.load(Ordering::Acquire));
        assert!(vm_stopped.load(Ordering::Acquire));
        assert_eq!(Arc::strong_count(&device), 2);

        let draining = tokio::spawn(prefetch.stop());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!draining.is_finished(), "stop must retain the blocked read");
        assert_eq!(Arc::strong_count(&device), 2);
        allow_read_finish.store(true, Ordering::Release);
        draining.await.context("prefetch drain panicked")?;
        assert_eq!(Arc::strong_count(&device), 1);
        Ok(())
    }

    #[tokio::test]
    async fn failed_vm_stop_still_cancels_and_can_drain_prefetch() {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(async {});
        let prefetch = LocalStartupPrefetch {
            cancel: Arc::clone(&cancel),
            task,
        };
        let result = stop_vm_after_prefetch_cancel(Some(&prefetch), async {
            anyhow::bail!("controlled VM stop error")
        })
        .await;
        assert!(result.is_err());
        assert!(cancel.load(Ordering::Acquire));
        prefetch.stop().await;
    }

    #[tokio::test]
    async fn direct_io_skips_local_prefetch_before_manifest_access() -> Result<()> {
        let tmp = tempfile::TempDir::new()?;
        let global = tmp.path().join("overlaybd.json");
        let config = overlaybd::config::GlobalConfig {
            io_engine: 2,
            ..Default::default()
        };
        tokio::fs::write(&global, serde_json::to_vec(&config)?).await?;
        let missing = tmp.path().join("missing-manifest");
        let pack = crate::snapshot::ResolvedStartupPack {
            source: crate::snapshot::startup_pack::ResolvedStartupPackSource::LocalPath(
                missing.clone(),
            ),
            pack_size: 1,
            mem_virtual_size: 4096,
            index_sha256: "x".to_owned(),
        };
        prepare_local_prefetch(
            tmp.path().join("missing-image-config"),
            global,
            pack,
            &AtomicBool::new(false),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn corrupt_local_manifest_is_best_effort_and_does_not_fail_the_task() -> Result<()> {
        let tmp = tempfile::TempDir::new()?;
        let global = tmp.path().join("overlaybd.json");
        tokio::fs::write(
            &global,
            serde_json::to_vec(&overlaybd::config::GlobalConfig::default())?,
        )
        .await?;
        let manifest = tmp.path().join("memory-startup.pack");
        let bytes = b"not a startup manifest";
        tokio::fs::write(&manifest, bytes).await?;
        let pack = crate::snapshot::ResolvedStartupPack {
            source: crate::snapshot::startup_pack::ResolvedStartupPackSource::LocalPath(manifest),
            pack_size: bytes.len() as u64,
            mem_virtual_size: 4096,
            index_sha256: crate::snapshot::startup_pack::hex_sha256(bytes),
        };
        assert!(prepare_local_prefetch(
            tmp.path().join("missing-image-config"),
            global,
            pack,
            &AtomicBool::new(false)
        )
        .await
        .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn oversized_descriptor_is_rejected_before_manifest_open() -> Result<()> {
        let tmp = tempfile::TempDir::new()?;
        let global = tmp.path().join("overlaybd.json");
        tokio::fs::write(
            &global,
            serde_json::to_vec(&overlaybd::config::GlobalConfig::default())?,
        )
        .await?;
        let max = overlaybd::startup_manifest::MANIFEST_HEADER_BYTES
            + overlaybd::startup_manifest::MAX_PREFIX_PAGES as usize * 8
            + overlaybd::startup_manifest::MAX_RANGES as usize * 16;
        let pack = crate::snapshot::ResolvedStartupPack {
            source: crate::snapshot::ResolvedStartupPackSource::LocalPath(
                tmp.path().join("missing-manifest"),
            ),
            pack_size: max as u64 + 1,
            mem_virtual_size: 4096,
            index_sha256: "irrelevant".into(),
        };
        let outcome = prepare_local_prefetch(
            tmp.path().join("missing-image-config"),
            global,
            pack,
            &AtomicBool::new(false),
        )
        .await;
        let Err(error) = outcome else {
            panic!("oversized descriptor must fail before opening the missing file");
        };
        assert!(error.to_string().contains("format limit"));
        Ok(())
    }

    #[test]
    fn local_prefetch_dedup_keeps_remaining_subscriber_running() {
        let key = format!("dedup-test-{}", uuid::Uuid::new_v4());
        let (first, first_leader) = acquire_local_prefetch(key.clone());
        let (second, second_leader) = acquire_local_prefetch(key.clone());
        assert!(first_leader);
        assert!(!second_leader);
        let first_lease = LocalPrefetchLease {
            identity: key.clone(),
            shared: Arc::clone(&first),
            released: false,
        };
        let second_lease = LocalPrefetchLease {
            identity: key,
            shared: second,
            released: false,
        };
        drop(first_lease);
        assert_eq!(first.subscribers.load(Ordering::Acquire), 1);
        assert!(
            !first.cancel.load(Ordering::Acquire),
            "one sandbox leaving must not cancel another subscriber"
        );
        drop(second_lease);
        assert_eq!(first.subscribers.load(Ordering::Acquire), 0);
        assert!(first.cancel.load(Ordering::Acquire));
    }

    #[test]
    fn local_prefetch_identity_includes_actual_layer_source() {
        let layers = |path: &str| {
            vec![overlaybd::pack_planner::FinalLayerSource {
                digest: "sha256:layer".to_owned(),
                size: 4096,
                source: overlaybd::pack_planner::FinalLayerBytes::LocalPath(PathBuf::from(path)),
            }]
        };
        assert_ne!(
            local_prefetch_identity("pack", &layers("/repository-a/layer")),
            local_prefetch_identity("pack", &layers("/repository-b/layer")),
            "different local layers must not share a physical prefetch task"
        );
    }

    #[test]
    fn acquire_after_final_release_never_reuses_cancelled_worker() {
        let key = format!("replacement-test-{}", uuid::Uuid::new_v4());
        let (draining, leader) = acquire_local_prefetch(key.clone());
        assert!(leader);
        let mut lease = LocalPrefetchLease {
            identity: key.clone(),
            shared: Arc::clone(&draining),
            released: false,
        };
        assert!(lease.release());
        assert!(draining.cancel.load(Ordering::Acquire));

        let (replacement, replacement_leader) = acquire_local_prefetch(key.clone());
        assert!(replacement_leader);
        assert!(!Arc::ptr_eq(&draining, &replacement));
        assert!(!replacement.cancel.load(Ordering::Acquire));
        drop(LocalPrefetchLease {
            identity: key,
            shared: replacement,
            released: false,
        });
    }

    #[test]
    fn concurrent_release_and_acquire_never_returns_cancelled_worker() {
        let key = format!("concurrent-test-{}", uuid::Uuid::new_v4());
        let (first, leader) = acquire_local_prefetch(key.clone());
        assert!(leader);
        let lease = LocalPrefetchLease {
            identity: key.clone(),
            shared: Arc::clone(&first),
            released: false,
        };
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let release_barrier = Arc::clone(&barrier);
        let release = std::thread::spawn(move || {
            release_barrier.wait();
            let mut lease = lease;
            lease.release()
        });
        barrier.wait();
        let (next, _) = acquire_local_prefetch(key.clone());
        release.join().expect("release thread must not panic");
        assert!(
            !next.cancel.load(Ordering::Acquire),
            "a concurrent acquire must receive an active worker"
        );
        drop(LocalPrefetchLease {
            identity: key,
            shared: next,
            released: false,
        });
    }

    #[tokio::test]
    async fn timeout_cleanup_drains_slow_read_before_marking_finished() {
        let shared = Arc::new(SharedLocalPrefetch {
            subscribers: std::sync::atomic::AtomicUsize::new(1),
            active_parts: std::sync::atomic::AtomicUsize::new(1),
            cancel: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        });
        let read_started = Arc::new(AtomicBool::new(false));
        let allow_read_finish = Arc::new(AtomicBool::new(false));
        let read_finished = Arc::new(AtomicBool::new(false));
        let started = Arc::clone(&read_started);
        let allow = Arc::clone(&allow_read_finish);
        let completed = Arc::clone(&read_finished);
        let read = tokio::task::spawn_blocking(move || {
            started.store(true, Ordering::Release);
            while !allow.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            completed.store(true, Ordering::Release);
            Ok::<(), anyhow::Error>(())
        });
        while !read_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        let worker_shared = Arc::clone(&shared);
        let cleanup = async move {
            worker_shared.cancel.store(true, Ordering::Release);
            read.await.context("blocking reader panicked")?
        };
        let completion = LocalPrefetchCompletion(Arc::clone(&shared));
        let runner_shared = Arc::clone(&shared);
        let runner = tokio::spawn(async move {
            let _completion = completion;
            finish_local_prefetch_worker(runner_shared, cleanup).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!read_finished.load(Ordering::Acquire));
        assert!(
            !shared.finished.load(Ordering::Acquire),
            "finished must wait for a dispatched blocking read"
        );
        allow_read_finish.store(true, Ordering::Release);
        runner.await.expect("worker runner must not panic");
        assert!(read_finished.load(Ordering::Acquire));
        assert!(shared.finished.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn last_subscriber_waits_for_worker_to_finish_after_cancellation() {
        let shared = Arc::new(SharedLocalPrefetch {
            subscribers: std::sync::atomic::AtomicUsize::new(1),
            active_parts: std::sync::atomic::AtomicUsize::new(1),
            cancel: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = tokio::spawn(async move {
            while !worker_shared.cancel.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            worker_shared.finished.store(true, Ordering::Release);
        });
        let mut lease = LocalPrefetchLease {
            identity: "standalone-test-worker".to_owned(),
            shared: Arc::clone(&shared),
            released: false,
        };
        assert!(lease.release(), "sole subscriber must cancel the worker");
        wait_for_local_prefetch_finished(&shared).await;
        worker.await.expect("worker must not panic");
        assert!(shared.finished.load(Ordering::Acquire));
    }
}
