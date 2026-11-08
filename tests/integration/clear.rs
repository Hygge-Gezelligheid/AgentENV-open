//! Real-guest regression for consecutive captures of one running instance.
use agentenv::sandbox::{
    FirecrackerSandbox, FirecrackerSandboxConfig, OverlaybdConfig, SandboxExecutor,
};
use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};

use crate::common;

async fn verify_memory(sandbox: &FirecrackerSandbox, expected: &str) -> Result<()> {
    let output = sandbox
        .run_command("sha256sum", &["/run/clear-memory"])
        .await?;
    ensure!(
        output.exit_code == 0,
        "read memory witness: {}",
        output.stderr
    );
    ensure!(
        output.stdout.split_whitespace().next() == Some(expected),
        "memory witness mismatch: expected {expected}, got {}",
        output.stdout
    );
    Ok(())
}

fn publication_metadata(
    source: &FirecrackerSandbox,
    versions: agentenv::snapshot::SnapshotRuntimeVersions,
) -> agentenv::snapshot::SnapshotPublishMetadata {
    use agentenv::snapshot::{SnapshotId, SnapshotPublishMetadata, SnapshotPublishSource};
    SnapshotPublishMetadata {
        id: SnapshotId::generate(),
        alias: None,
        source: SnapshotPublishSource::Sandbox {
            source_sandbox_id: source.sandbox_id().to_string(),
            source_sandbox_metadata: None,
        },
        context: Default::default(),
        startup: None,
        resources: agentenv::types::SandboxResources {
            cpu_count: 2,
            memory_mib: 512,
            disk_size_mib: 0,
        },
        runtime_versions: versions,
        virtualization_mode: agentenv::cfg::ConfigManager::global_config().virtualization_mode,
        image_configs: Default::default(),
        volume_snapshots: vec![],
        custom_extension_params: None,
    }
}

#[tokio::test]
#[ignore = "requires a CLEAR-capable Firecracker, KVM, ublk and prepared test dependencies"]
async fn consecutive_captures_restore_every_generation_after_source_deletion() -> Result<()> {
    common::setup().await;
    let mut config = common::default_sandbox_config()?;
    let store = tempfile::tempdir()?;
    config.vcpu_count = 2;
    config.mem_size_mib = 512;
    config.common.track_dirty_pages = true;
    let mut source = FirecrackerSandbox::new(config)?;
    source.start().await?;
    let result = async {
        let output = source
            .run_command(
                "sh",
                &[
                    "-c",
                    "dd if=/dev/zero of=/run/clear-memory bs=4096 count=16 status=none",
                ],
            )
            .await?;
        ensure!(
            output.exit_code == 0,
            "initialize witness: {}",
            output.stderr
        );
        let mut expected = vec![0u8; 16 * 4096];
        let mut captures = Vec::new();
        for generation in 0..96 {
            let page = generation % 16;
            let byte = if generation % 3 == 0 {
                0
            } else {
                generation as u8 + 1
            };
            let command = format!(
                "head -c 4096 /dev/zero | tr '\\000' '\\{byte:03o}' | \
                 dd of=/run/clear-memory bs=4096 seek={page} conv=notrunc status=none"
            );
            let output = source.run_command("sh", &["-c", &command]).await?;
            ensure!(
                output.exit_code == 0,
                "write generation {generation}: {}",
                output.stderr
            );
            expected[page * 4096..(page + 1) * 4096].fill(byte);
            let digest = format!("{:x}", Sha256::digest(&expected));
            verify_memory(&source, &digest).await?;
            let directory = store.path().join(format!("generation-{generation:03}"));
            let (snapshot, _) = source.pause_to_dir(&directory).await?;
            source.resume().await?;
            captures.push((snapshot, digest));
        }
        source.stop().await?;
        for (generation, (snapshot, expected)) in captures.iter().enumerate() {
            let mut restored = FirecrackerSandbox::resume_from_snapshot_config(snapshot).await?;
            let verified = verify_memory(&restored, expected).await;
            restored.stop().await?;
            verified.map_err(|error| error.context(format!("restore generation {generation}")))?;
        }
        // Each capture owns its complete chain; earlier capture directories are
        // no longer needed, even when later snapshots inherit their pages.
        for generation in 0..95 {
            std::fs::remove_dir_all(store.path().join(format!("generation-{generation:03}")))?;
        }
        let (snapshot, expected) = captures.last().unwrap();
        let mut restored = FirecrackerSandbox::resume_from_snapshot_config(snapshot).await?;
        let verified = verify_memory(&restored, expected).await;
        restored.stop().await?;
        verified?;
        Ok(())
    }
    .await;
    source.stop().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated KVM/ublk state and a prepared local image"]
async fn failed_publication_after_clear_preserves_next_snapshot() -> Result<()> {
    use agentenv::sandbox::{SandboxBackend, SandboxLaunchConfig};
    use std::os::unix::fs::PermissionsExt;
    common::setup_runtime_only().await;
    let path = std::env::var_os("AENV_CLEAR_TEST_IMAGE_CONFIG")
        .ok_or_else(|| anyhow::anyhow!("set AENV_CLEAR_TEST_IMAGE_CONFIG"))?;
    let mut config =
        FirecrackerSandboxConfig::from_global_config_with_user_image(OverlaybdConfig {
            image_config_path: path.into(),
            read_only: false,
            runtime_upper_mode: overlaybd::config::UpperMode::LogStructured,
        })?;
    config.vcpu_count = 2;
    config.mem_size_mib = 512;
    let versions_binary = config.common.firecracker_binary.clone();
    let tools_version = config.common.tools_drive_version.clone();
    let mut source = FirecrackerSandbox::new(config)?;
    source.start().await?;
    let store = tempfile::tempdir()?;
    let (_, manager, _) = common::snapshot_test_parts(store.path());
    let result = async {
        let versions =
            agentenv::sandbox::probe_runtime_versions(&source, versions_binary, tools_version)
                .await?;
        let first_metadata = publication_metadata(&source, versions.clone());
        let first_id = first_metadata.id.clone();
        let next_metadata = publication_metadata(&source, versions);
        let output = source
            .run_command("sh", &["-c", "echo FIRST > /run/clear-first"])
            .await?;
        ensure!(output.exit_code == 0);
        let first = source.snapshot().await?;
        let failed_directory = first
            .manifest()
            .vm_state
            .path
            .parent()
            .unwrap()
            .to_path_buf();
        // Read/search access allows the preflight missing-record check, while
        // denying the catalog lock/write after artifact uploads have completed.
        let records = store.path().join("repository/catalog/records");
        std::fs::create_dir_all(&records)?;
        let original = std::fs::metadata(&records)?.permissions();
        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o555))?;
        let failed = agentenv::sandbox::publish_captured(&manager, first_metadata, first).await;
        std::fs::set_permissions(&records, original)?;
        ensure!(
            failed.is_err(),
            "catalog failure injection did not fail publication"
        );
        ensure!(manager.get(first_id.to_string()).await?.is_none());
        std::fs::remove_dir_all(failed_directory)?;
        let output = source
            .run_command("sh", &["-c", "echo SECOND > /run/clear-second"])
            .await?;
        ensure!(output.exit_code == 0);
        let next = source.snapshot().await?;
        let record = agentenv::sandbox::publish_captured(&manager, next_metadata, next).await?;
        source.stop().await?;
        let runnable = manager.resolve_runnable(record).await?;
        let mut restored =
            FirecrackerSandbox::from_snapshot(&runnable, &SandboxLaunchConfig::default())?;
        restored.start().await?;
        let output = restored
            .run_command("sh", &["-c", "cat /run/clear-first /run/clear-second"])
            .await;
        restored.stop().await?;
        let output = output?;
        ensure!(
            output.exit_code == 0 && output.stdout == "FIRST\nSECOND\n",
            "publication failure lost memory updates"
        );
        Ok(())
    }
    .await;
    source.stop().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated KVM/ublk state and a prepared local image"]
async fn fork_and_encoded_pause_preserve_incremental_memory() -> Result<()> {
    use agentenv::sandbox::{FirecrackerPausedState, SandboxBackend, SandboxForkSpec};
    use agentenv::types::SandboxId;
    common::setup().await;
    for tracking in [true, false] {
        let mut config = common::default_sandbox_config()?;
        config.vcpu_count = 2;
        config.mem_size_mib = 512;
        config.common.track_dirty_pages = tracking;
        let mut source = FirecrackerSandbox::new(config)?;
        source.start().await?;
        let store = tempfile::tempdir()?;
        let result: Result<()> = async {
            let output = source
                .run_command("sh", &["-c", "echo FIRST > /run/clear-fork"])
                .await?;
            ensure!(output.exit_code == 0);
            drop(SandboxBackend::snapshot(&mut source).await?);
            let output = source
                .run_command("sh", &["-c", "echo SECOND >> /run/clear-fork"])
                .await?;
            ensure!(output.exit_code == 0);
            let spec = || SandboxForkSpec {
                sandbox_id: SandboxId::new(),
                envd_access_token: None,
                extra_drives: vec![],
                replace_drive_ids: vec![],
            };
            let children = source.fork(&[spec(), spec()]).await?;
            for (index, child) in children.into_iter().enumerate() {
                let mut child = child?;
                let directory = store.path().join(index.to_string());
                let state = child.pause(Some(&directory)).await;
                child.stop().await?;
                let state = state?;
                let encoded = state.encode()?;
                drop(state);
                let decoded = FirecrackerPausedState::decode(directory, encoded)?;
                let mut restored =
                    FirecrackerSandbox::resume_from_snapshot_config(decoded.snapshot_config())
                        .await?;
                let output = restored
                    .run_command(
                        "sh",
                        &["-c", "cat /run/clear-fork; echo CHILD >> /run/clear-fork"],
                    )
                    .await;
                restored.stop().await?;
                let output = output?;
                ensure!(
                    output.exit_code == 0 && output.stdout == "FIRST\nSECOND\n",
                    "fork/pause lost memory or shared child writes (tracking={tracking})"
                );
            }
            let output = source.run_command("cat", &["/run/clear-fork"]).await?;
            ensure!(output.exit_code == 0 && output.stdout == "FIRST\nSECOND\n");
            Ok(())
        }
        .await;
        source.stop().await?;
        result?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated KVM/ublk state; optional AENV_CLEAR_OSS_ENDPOINT uses a test S3 bucket"]
async fn committed_chain_survives_source_deletion_and_fresh_repository_cache() -> Result<()> {
    use agentenv::sandbox::{SandboxBackend, SandboxLaunchConfig};
    use agentenv::snapshot::{backends::OssBackend, SnapshotManager};
    common::setup().await;
    let root = tempfile::tempdir()?;
    let endpoint = std::env::var("AENV_CLEAR_OSS_ENDPOINT").ok();
    let make_manager = || -> Result<SnapshotManager> {
        if let Some(endpoint) = &endpoint {
            let oss = agentenv::cfg::OssBackendConfig {
                endpoint: endpoint.clone(),
                bucket: "clear-api-test".into(),
                prefix: Some(
                    root.path()
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                ),
                credential_process: None,
                access_key_id: Some("minioadmin".into()),
                access_key_secret: Some("minioadmin".into()),
                security_token: None,
                region: Some("us-east-1".into()),
                addressing_style: None,
                cache_max_size_gb: Some(1),
            };
            let (repository, resolver) = OssBackend::new_with_publish_compression(
                &oss,
                root.path().join("runtime-cache"),
                &agentenv::cfg::SnapshotPublishCompressionConfig {
                    enabled: false,
                    ..Default::default()
                },
            )?
            .into_parts();
            Ok(SnapshotManager::from_parts(repository, resolver, None))
        } else {
            Ok(common::snapshot_test_parts(root.path()).1)
        }
    };
    let mut manager = make_manager()?;
    let mut config = common::default_sandbox_config()?;
    config.vcpu_count = 2;
    config.mem_size_mib = 512;
    let binary = config.common.firecracker_binary.clone();
    let tools = config.common.tools_drive_version.clone();
    let mut source = FirecrackerSandbox::new(config)?;
    source.start().await?;
    let result: Result<_> = async {
        let versions = agentenv::sandbox::probe_runtime_versions(&source, binary, tools).await?;
        let mut records = Vec::new();
        for generation in 0..3 {
            let command = format!(
                "echo {generation} >> /run/clear-cache; echo {generation} >> /tmp/clear-cache"
            );
            let output = source.run_command("sh", &["-c", &command]).await?;
            ensure!(output.exit_code == 0);
            let metadata = publication_metadata(&source, versions.clone());
            let captured = SandboxBackend::snapshot(&mut source).await?;
            records.push(agentenv::sandbox::publish_captured(&manager, metadata, captured).await?);
        }
        Ok(records)
    }
    .await;
    source.stop().await?;
    drop(source);
    let records = result?;
    let latest_id = records.last().unwrap().id.to_string();
    drop(
        manager
            .resolve_runnable(records.last().unwrap().clone())
            .await?,
    );
    for record in &records[..2] {
        manager.delete(&record.id.to_string()).await?;
    }
    drop(records);
    drop(manager);
    std::fs::remove_dir_all(root.path().join("runtime-cache"))?;
    manager = make_manager()?;
    let record = manager
        .get(&latest_id)
        .await?
        .expect("committed latest record");
    let runnable = manager.resolve_runnable(record).await?;
    let mut restored =
        FirecrackerSandbox::from_snapshot(&runnable, &SandboxLaunchConfig::default())?;
    restored.start().await?;
    let output = restored
        .run_command("sh", &["-c", "cat /run/clear-cache /tmp/clear-cache"])
        .await;
    restored.stop().await?;
    let output = output?;
    ensure!(
        output.exit_code == 0 && output.stdout == "0\n1\n2\n0\n1\n2\n",
        "fresh repository cache lost incremental RAM or disk state"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated KVM/ublk state and a prepared local image"]
async fn captures_during_atomic_memory_and_disk_writes_restore_complete_files() -> Result<()> {
    use agentenv::sandbox::{FirecrackerCaptureArtifacts, SandboxBackend};
    common::setup().await;
    let mut config = common::default_sandbox_config()?;
    config.vcpu_count = 2;
    config.mem_size_mib = 512;
    let mut source = FirecrackerSandbox::new(config)?;
    source.start().await?;
    let result: Result<()> = async {
        let output = source
            .run_command(
                "sh",
                &[
                    "-c",
                    r#"
            head -c 262144 /dev/zero | tr '\000' '\125' > /run/clear-io-a
            head -c 262144 /dev/zero | tr '\000' '\252' > /run/clear-io-b
            cp /run/clear-io-a /run/clear-io-atomic
            cp /run/clear-io-a /tmp/clear-io-atomic
            sync -f /tmp/clear-io-atomic
            setsid sh -c '
                n=0
                while :; do
                    for p in a b; do
                        cp /run/clear-io-$p /tmp/clear-io-next
                        sync -f /tmp/clear-io-next
                        mv -f /tmp/clear-io-next /tmp/clear-io-atomic
                        cp /run/clear-io-$p /run/clear-io-next
                        mv -f /run/clear-io-next /run/clear-io-atomic
                        n=$((n+1)); echo "$n" > /run/clear-io-count
                    done
                done
            ' </dev/null >/dev/null 2>&1 &
        "#,
                ],
            )
            .await?;
        ensure!(output.exit_code == 0, "start writer: {}", output.stderr);
        let mut captures = Vec::new();
        for _ in 0..8 {
            captures.push(SandboxBackend::snapshot(&mut source).await?);
        }
        let count = source.run_command("cat", &["/run/clear-io-count"]).await?;
        ensure!(
            count.exit_code == 0 && count.stdout.trim().parse::<u64>()? >= 4,
            "background writer did not make progress during captures"
        );
        source.stop().await?;
        let expected =
            [0x55, 0xAA].map(|value| format!("{:x}", Sha256::digest(vec![value; 262144])));
        for captured in &captures {
            let config = captured
                .downcast_artifacts_ref::<FirecrackerCaptureArtifacts>()
                .expect("Firecracker capture artifacts")
                .snapshot_config();
            let mut restored = FirecrackerSandbox::resume_from_snapshot_config(config).await?;
            let output = restored
                .run_command(
                    "sha256sum",
                    &["/run/clear-io-atomic", "/tmp/clear-io-atomic"],
                )
                .await;
            restored.stop().await?;
            let output = output?;
            ensure!(output.exit_code == 0 && output.stdout.lines().count() == 2);
            for line in output.stdout.lines() {
                ensure!(
                    expected
                        .iter()
                        .any(|digest| Some(digest.as_str()) == line.split_whitespace().next()),
                    "restored atomic file contains partial data: {line}"
                );
            }
        }
        Ok(())
    }
    .await;
    source.stop().await?;
    result
}
