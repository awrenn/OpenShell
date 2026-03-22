// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Firecracker VM lifecycle: create, snapshot, restore, destroy.

use crate::api::{
    BootSource, Drive, FcApi, InstanceAction, MachineConfig, MemBackend, NetworkInterface,
    SnapshotCreate, SnapshotLoad,
};
use crate::error::{FirecrackerError, Result};
use crate::overlay::WorkspaceOverlay;
use crate::routing::VmRouting;
use crate::tap::TapDevice;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::{Child, Command};
use tracing::{debug, info};

const BOOT_ARGS: &str =
    "console=ttyS0 reboot=k panic=1 pci=off ro root=/dev/vda init=/sbin/tini -- /opt/openshell/bin/openshell-sandbox";

const API_READY_TIMEOUT: Duration = Duration::from_secs(5);
const API_READY_POLL_MS: u64 = 50;

/// A running Firecracker microVM sandbox.
///
/// All resources (tap, overlay, iptables rules, process) are cleaned up on drop.
pub struct FirecrackerVm {
    pub sandbox_id: String,
    tap: TapDevice,
    overlay: WorkspaceOverlay,
    routing: VmRouting,
    process: Child,
    api: FcApi,
    socket_path: PathBuf,
}

impl FirecrackerVm {
    /// Launch a new VM from a base snapshot.
    ///
    /// `snapshot_path` and `mem_path` point to the pre-built base snapshot.
    /// The workspace overlay is attached after restore.
    pub async fn from_snapshot(
        sandbox_id: &str,
        snapshot_path: &Path,
        mem_path: &Path,
        workspace_overlay: WorkspaceOverlay,
        fc_binary: &Path,
        overlay_dir: &Path,
    ) -> Result<Self> {
        check_kvm()?;

        let tap = TapDevice::create(sandbox_id)?;
        let routing = VmRouting::install(&tap.subnet)?;

        let socket_path = std::env::temp_dir().join(format!("fc-{}.sock", &sandbox_id[..8]));

        let process = spawn_firecracker(fc_binary, &socket_path).await?;
        let api = FcApi::new(&socket_path);

        wait_for_api(&api, &socket_path).await?;

        // Restore from base snapshot
        api.put("/snapshot/load", &SnapshotLoad {
            snapshot_path: snapshot_path.to_string_lossy().into(),
            mem_backend: MemBackend {
                backend_type: "File".into(),
                backend_path: mem_path.to_string_lossy().into(),
            },
            enable_diff_snapshots: false,
            resume_vm: false,
        }).await?;

        // Attach workspace drive (hot-plug after restore)
        api.patch("/drives/workspace", &Drive {
            drive_id: "workspace".into(),
            path_on_host: workspace_overlay.device_path.to_string_lossy().into(),
            is_root_device: false,
            is_read_only: false,
        }).await?;

        // Attach tap interface
        api.put(&format!("/network-interfaces/{}", tap.name), &NetworkInterface {
            iface_id: tap.name.clone(),
            host_dev_name: tap.name.clone(),
            guest_mac: derive_mac(sandbox_id),
        }).await?;

        // Resume
        api.put("/actions", &InstanceAction { action_type: "ResumeVm".into() }).await?;

        info!(sandbox_id, tap = %tap.name, "Firecracker VM started from snapshot");

        Ok(Self {
            sandbox_id: sandbox_id.to_string(),
            tap,
            overlay: workspace_overlay,
            routing,
            process,
            api,
            socket_path,
        })
    }

    /// Boot a fresh VM (no snapshot). Used to build the base snapshot.
    pub async fn boot_fresh(config: FreshBootConfig, fc_binary: &Path) -> Result<Self> {
        check_kvm()?;

        let tap = TapDevice::create(&config.sandbox_id)?;
        let routing = VmRouting::install(&tap.subnet)?;

        let socket_path = std::env::temp_dir()
            .join(format!("fc-{}.sock", &config.sandbox_id[..8]));

        let process = spawn_firecracker(fc_binary, &socket_path).await?;
        let api = FcApi::new(&socket_path);

        wait_for_api(&api, &socket_path).await?;

        api.put("/machine-config", &MachineConfig {
            vcpu_count: config.vcpu_count,
            mem_size_mib: config.mem_mib,
        }).await?;

        api.put("/boot-source", &BootSource {
            kernel_image_path: config.kernel_path.to_string_lossy().into(),
            boot_args: BOOT_ARGS.to_string(),
        }).await?;

        api.put("/drives/rootfs", &Drive {
            drive_id: "rootfs".into(),
            path_on_host: config.rootfs_path.to_string_lossy().into(),
            is_root_device: true,
            is_read_only: true,
        }).await?;

        api.put("/drives/workspace", &Drive {
            drive_id: "workspace".into(),
            path_on_host: config.workspace_overlay.device_path.to_string_lossy().into(),
            is_root_device: false,
            is_read_only: false,
        }).await?;

        api.put(&format!("/network-interfaces/{}", tap.name), &NetworkInterface {
            iface_id: tap.name.clone(),
            host_dev_name: tap.name.clone(),
            guest_mac: derive_mac(&config.sandbox_id),
        }).await?;

        api.put("/actions", &InstanceAction { action_type: "InstanceStart".into() }).await?;

        info!(sandbox_id = %config.sandbox_id, "Firecracker VM booted fresh");

        Ok(Self {
            sandbox_id: config.sandbox_id,
            tap,
            overlay: config.workspace_overlay,
            routing,
            process,
            api,
            socket_path,
        })
    }

    /// Snapshot this VM's state to disk. The VM is paused during snapshot.
    pub async fn create_snapshot(&self, snapshot_path: &Path, mem_path: &Path) -> Result<()> {
        info!(sandbox_id = %self.sandbox_id, "Creating base snapshot");

        self.api.put("/actions", &InstanceAction { action_type: "PauseVm".into() }).await?;

        self.api.put("/snapshot/create", &SnapshotCreate {
            snapshot_type: "Full".into(),
            snapshot_path: snapshot_path.to_string_lossy().into(),
            mem_file_path: mem_path.to_string_lossy().into(),
        }).await?;

        info!(
            snapshot = %snapshot_path.display(),
            mem = %mem_path.display(),
            "Base snapshot written"
        );
        Ok(())
    }

    /// Sync workspace changes back to the host and destroy the VM.
    pub async fn destroy(mut self, sync_dest: Option<&Path>) -> Result<()> {
        info!(sandbox_id = %self.sandbox_id, "Destroying Firecracker VM");

        // Kill VM process
        let _ = self.process.kill().await;

        // Sync workspace back if requested
        if let Some(dest) = sync_dest {
            self.overlay.sync_back(dest)?;
        }

        // Resources cleaned up by Drop on tap, overlay, routing
        std::fs::remove_file(&self.socket_path).ok();
        Ok(())
    }

    pub fn guest_ip(&self) -> std::net::Ipv4Addr {
        self.tap.subnet.guest_ip
    }
}

impl Drop for FirecrackerVm {
    fn drop(&mut self) {
        // Best-effort kill if destroy() wasn't called
        let _ = self.process.start_kill();
        std::fs::remove_file(&self.socket_path).ok();
        // tap, overlay, routing drop in field order and clean up
    }
}

pub struct FreshBootConfig {
    pub sandbox_id: String,
    pub vcpu_count: u32,
    pub mem_mib: u32,
    pub kernel_path: PathBuf,
    pub rootfs_path: PathBuf,
    pub workspace_overlay: WorkspaceOverlay,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn check_kvm() -> Result<()> {
    if !Path::new("/dev/kvm").exists() {
        return Err(FirecrackerError::KvmUnavailable);
    }
    Ok(())
}

async fn spawn_firecracker(binary: &Path, socket_path: &Path) -> Result<Child> {
    if !binary.exists() {
        return Err(FirecrackerError::BinaryNotFound {
            path: binary.to_string_lossy().into(),
        });
    }

    // Remove stale socket if present
    let _ = std::fs::remove_file(socket_path);

    let child = Command::new(binary)
        .args(["--api-sock", &socket_path.to_string_lossy()])
        .spawn()
        .map_err(|e| FirecrackerError::Process(e.to_string()))?;

    debug!(socket = %socket_path.display(), "Firecracker process spawned");
    Ok(child)
}

async fn wait_for_api(api: &FcApi, socket_path: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + API_READY_TIMEOUT;
    loop {
        if socket_path.exists() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(FirecrackerError::ApiConnect(
                "timed out waiting for Firecracker API socket".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(API_READY_POLL_MS)).await;
    }
}

/// Derive a deterministic MAC address from the sandbox ID.
fn derive_mac(sandbox_id: &str) -> String {
    let b = sandbox_id.as_bytes();
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        b.get(0).unwrap_or(&0),
        b.get(1).unwrap_or(&0),
        b.get(2).unwrap_or(&0),
        b.get(3).unwrap_or(&0),
        b.get(4).unwrap_or(&0),
    )
}
