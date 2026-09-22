use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agentenv::sandbox::{FirecrackerSandbox, SandboxBackend, SandboxLaunchConfig};
use agentenv::snapshot::repository::backends::{PosixFsBackend, PosixFsBackendConfig};
use agentenv::snapshot::{
    ResolvedStartupPackSource, SnapshotAlias, SnapshotId, SnapshotManager, SnapshotPublishMetadata,
    SnapshotPublishSource, SnapshotRuntimeVersions,
};
use agentenv::types::{SandboxId, SandboxResources};
use anyhow::{bail, Context, Result};

use crate::common;

#[tokio::test]
#[ignore = "requires enabled startup-pack config plus KVM/ublk"]
async fn posix_startup_pack_publish_resolve_and_resume() -> Result<()> {
    common::setup().await;
    let config = agentenv::cfg::ConfigManager::global_config();
    if !config.snapshot.memory_startup_pack.enabled
        || !config.snapshot.memory_startup_pack.consume_enabled
    {
        bail!("enable snapshot.memory_startup_pack record and consume for this test");
    }

    let root = tempfile::tempdir()?;
    let (_, manager, _) = common::snapshot_test_parts(root.path());
    let mut original = FirecrackerSandbox::new(common::default_sandbox_config()?)?;
    original.start().await?;
    let captured = SandboxBackend::snapshot(&mut original).await?;
    let record = manager
        .publish_captured(
            SnapshotPublishMetadata {
                id: SnapshotId::generate(),
                alias: Some(SnapshotAlias::parse(&format!(
                    "posix-startup-pack-{}",
                    std::process::id()
                ))?),
                source: SnapshotPublishSource::Sandbox {
                    source_sandbox_id: SandboxId::new().to_string(),
                },
                context: agentenv::snapshot::CommandContext::default(),
                startup: None,
                resources: SandboxResources {
                    cpu_count: 1,
                    memory_mib: 128,
                    disk_size_mib: 0,
                },
                runtime_versions: SnapshotRuntimeVersions {
                    kernel_version: "kernel".into(),
                    firecracker_version: "firecracker".into(),
                    envd_version: "envd".into(),
                    tools_drive_version: config.resolved_tools_version().to_string(),
                },
                virtualization_mode: config.virtualization_mode,
                image_configs: agentenv::types::ImageConfigs::new(),
                volume_snapshots: Vec::new(),
                custom_extension_params: None,
            },
            captured,
        )
        .await?;

    let attached = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let current = manager
                .get(&record.id.to_string())
                .await?
                .context("published POSIX snapshot disappeared before descriptor attach")?;
            if current
                .committed
                .as_ref()
                .and_then(|committed| committed.memory_startup.as_ref())
                .is_some()
            {
                return Ok::<_, anyhow::Error>(current);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("POSIX startup descriptor did not attach")??;

    let runnable = manager.resolve_runnable(attached).await?;
    let pack = runnable
        .manifest()
        .memory_startup_pack
        .as_ref()
        .context("POSIX resolver did not enable startup-pack consumption")?;
    let ResolvedStartupPackSource::LocalPath(path) = &pack.source else {
        bail!("POSIX resolver selected a non-local startup-pack source");
    };
    assert!(
        path.is_file(),
        "LocalPath manifest must exist: {}",
        path.display()
    );

    let launch = SandboxLaunchConfig {
        sandbox_id: SandboxId::new(),
        snapshot_id: runnable.record().id.to_string(),
        env_vars: None,
        network: None,
        extra_mmds: serde_json::Map::new(),
        extra_drives: Vec::new(),
        extra_drives_in_snapshot: false,
        custom_extension_params: None,
        envd_access_token: None,
    };
    let mut restored = FirecrackerSandbox::from_snapshot(&runnable, &launch)?;
    restored.start().await?;
    restored.stop().await?;
    original.stop().await?;
    Ok(())
}

/// Fixed-input hardware measurement.  The caller supplies one already-published
/// snapshot so each process uses the normal POSIX resolver without recapturing.
#[tokio::test]
#[ignore = "requires KVM/ublk plus fixed POSIX startup-pack input"]
async fn posix_startup_pack_fixed_cold_resume() -> Result<()> {
    common::setup_runtime_only().await;
    let repository_root = PathBuf::from(std::env::var("AENV_POSIX_STARTUP_PACK_REPOSITORY")?);
    let runtime_root = PathBuf::from(std::env::var("AENV_POSIX_STARTUP_PACK_RUNTIME")?);
    let snapshot_id = std::env::var("AENV_POSIX_STARTUP_PACK_SNAPSHOT")?;
    let expect_local = std::env::var("AENV_POSIX_STARTUP_PACK_EXPECT_LOCAL")? == "1";
    let backend = PosixFsBackend::new(PosixFsBackendConfig {
        root: repository_root,
        cache_root: Some(runtime_root.join("cache")),
        runtime_cache_root: Some(runtime_root.join("runtime")),
    })?;
    let manager =
        SnapshotManager::from_parts(backend.repository(), backend.runtime_resolver(), None);
    let record = manager
        .get(&snapshot_id)
        .await?
        .context("fixed POSIX snapshot is missing")?;
    let runnable = manager.resolve_runnable(record).await?;
    match runnable.manifest().memory_startup_pack.as_ref() {
        Some(pack) if expect_local => match &pack.source {
            ResolvedStartupPackSource::LocalPath(path) if path.is_file() => {}
            _ => bail!("consume-enabled resolver did not provide a local manifest"),
        },
        None if !expect_local => {}
        _ => bail!("unexpected startup-pack resolver result for configured mode"),
    }

    let image =
        overlaybd::config::load_image_config(&runnable.manifest().memory.image_config_path)?;
    let cold_control_start = Instant::now();
    let mut checked_layers = 0usize;
    for layer in image.lowers.iter().filter(|layer| !layer.file.is_empty()) {
        verify_file_cold(Path::new(&layer.file))?;
        checked_layers += 1;
    }
    anyhow::ensure!(
        checked_layers != 0,
        "resolved memory image has no local layers"
    );
    let cold_control_ms = cold_control_start.elapsed().as_millis();

    let launch = SandboxLaunchConfig {
        sandbox_id: SandboxId::new(),
        snapshot_id: runnable.record().id.to_string(),
        env_vars: None,
        network: None,
        extra_mmds: serde_json::Map::new(),
        extra_drives: Vec::new(),
        extra_drives_in_snapshot: false,
        custom_extension_params: None,
        envd_access_token: None,
    };
    let resume_start = Instant::now();
    let mut restored = FirecrackerSandbox::from_snapshot(&runnable, &launch)?;
    restored.start().await?;
    let ready_ms = resume_start.elapsed().as_millis();
    restored.stop().await?;
    eprintln!(
        "POSIX_STARTUP_PACK_FIXED_RESULT expect_local={expect_local} cold_control_ms={cold_control_ms} ready_ms={ready_ms} checked_layers={checked_layers}"
    );
    Ok(())
}

fn verify_file_cold(path: &Path) -> Result<()> {
    let file = File::open(path).with_context(|| format!("open memory layer {}", path.display()))?;
    let len = file.metadata()?.len();
    anyhow::ensure!(len != 0, "memory layer {} is empty", path.display());
    let advise = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    anyhow::ensure!(
        advise == 0,
        "fadvise failed for {}: {advise}",
        path.display()
    );
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    anyhow::ensure!(page_size != 0, "invalid system page size");
    let mapped_len = len as usize;
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            mapped_len,
            libc::PROT_NONE,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    anyhow::ensure!(
        mapping != libc::MAP_FAILED,
        "mmap failed for {}",
        path.display()
    );
    let mut residency = vec![0_u8; mapped_len.div_ceil(page_size)];
    let mincore = unsafe { libc::mincore(mapping, mapped_len, residency.as_mut_ptr()) };
    let unmap = unsafe { libc::munmap(mapping, mapped_len) };
    anyhow::ensure!(unmap == 0, "munmap failed for {}", path.display());
    anyhow::ensure!(mincore == 0, "mincore failed for {}", path.display());
    let resident = residency.iter().filter(|page| **page & 1 != 0).count();
    anyhow::ensure!(
        resident == 0,
        "memory layer {} remains resident: {resident} pages",
        path.display()
    );
    Ok(())
}
