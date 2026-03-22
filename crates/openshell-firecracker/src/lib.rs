// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Firecracker microVM runtime for OpenShell sandboxes.
//!
//! # Architecture
//!
//! Each sandbox gets:
//! - A dedicated TAP device (`fc-tap-<id>`) with a /30 subnet
//! - A dm-snapshot workspace overlay (COW on top of a shared base ext4)
//! - iptables rules redirecting all VM TCP egress to the host-side proxy
//! - A Firecracker process restored from a base snapshot (~150ms start)
//!
//! The host-side proxy (Phase 4) enforces OPA network policy before
//! traffic exits the host. The VM cannot reach the internet without going
//! through it.
//!
//! # Usage
//!
//! ```rust,ignore
//! // One-time setup: build the base snapshot
//! FirecrackerRuntime::build_base_snapshot(&config).await?;
//!
//! // Per sandbox: restore from snapshot
//! let vm = runtime.create_sandbox(&sandbox_id, workspace_dir).await?;
//!
//! // On destroy:
//! vm.destroy(Some(workspace_dir)).await?;
//! ```

pub mod api;
pub mod error;
pub mod overlay;
pub mod routing;
pub mod subnet;
pub mod tap;
pub mod vm;

use error::Result;
use overlay::WorkspaceOverlay;
use std::path::{Path, PathBuf};
use tracing::info;
use vm::{FirecrackerVm, FreshBootConfig};

/// Configuration for the Firecracker runtime on a gateway node.
#[derive(Debug, Clone)]
pub struct FirecrackerRuntimeConfig {
    /// Path to the `firecracker` binary.
    pub binary_path: PathBuf,
    /// Path to the vmlinux kernel image.
    pub kernel_path: PathBuf,
    /// Path to the base rootfs ext4 image.
    pub rootfs_path: PathBuf,
    /// Directory for per-sandbox COW overlay files.
    pub overlay_dir: PathBuf,
    /// Path to the base VM snapshot file.
    pub snapshot_path: PathBuf,
    /// Path to the base VM memory snapshot file.
    pub snapshot_mem_path: PathBuf,
    /// Default vCPU count for new VMs.
    pub default_vcpu_count: u32,
    /// Default memory in MiB for new VMs.
    pub default_mem_mib: u32,
}

impl Default for FirecrackerRuntimeConfig {
    fn default() -> Self {
        let base = PathBuf::from("/var/lib/openshell/firecracker");
        Self {
            binary_path: PathBuf::from("/usr/local/bin/firecracker"),
            kernel_path: base.join("vmlinux"),
            rootfs_path: base.join("rootfs.ext4"),
            overlay_dir: base.join("overlays"),
            snapshot_path: base.join("base.snapshot"),
            snapshot_mem_path: base.join("base.mem"),
            default_vcpu_count: 2,
            default_mem_mib: 2048,
        }
    }
}

/// Gateway-level Firecracker runtime manager.
pub struct FirecrackerRuntime {
    config: FirecrackerRuntimeConfig,
}

impl FirecrackerRuntime {
    pub fn new(config: FirecrackerRuntimeConfig) -> Self {
        Self { config }
    }

    /// Build the base snapshot from which all sandboxes are restored.
    ///
    /// This is called once during gateway setup (or when the rootfs is updated).
    /// It boots a fresh VM, waits for the sandbox supervisor to be ready,
    /// snapshots it, and tears it down.
    pub async fn build_base_snapshot(&self) -> Result<()> {
        info!("Building Firecracker base snapshot");

        std::fs::create_dir_all(&self.config.overlay_dir)?;

        // Temporary workspace overlay for the snapshot VM
        let overlay = WorkspaceOverlay::create(
            "snapshot-build",
            &self.config.rootfs_path,
            &self.config.overlay_dir,
        )?;

        let vm = FirecrackerVm::boot_fresh(
            FreshBootConfig {
                sandbox_id: "snapshot-build".to_string(),
                vcpu_count: self.config.default_vcpu_count,
                mem_mib: self.config.default_mem_mib,
                kernel_path: self.config.kernel_path.clone(),
                rootfs_path: self.config.rootfs_path.clone(),
                workspace_overlay: overlay,
            },
            &self.config.binary_path,
        ).await?;

        // TODO(phase3): wait for sandbox supervisor gRPC ready signal
        // For now, sleep to give the VM time to boot
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        vm.create_snapshot(&self.config.snapshot_path, &self.config.snapshot_mem_path).await?;
        vm.destroy(None).await?;

        info!("Base snapshot ready");
        Ok(())
    }

    /// Create a new sandbox VM restored from the base snapshot.
    pub async fn create_sandbox(
        &self,
        sandbox_id: &str,
        workspace_dir: Option<&Path>,
    ) -> Result<FirecrackerVm> {
        std::fs::create_dir_all(&self.config.overlay_dir)?;

        // Pack workspace directory into a base ext4 if provided
        let workspace_base = self.config.overlay_dir.join(format!("{}-ws-base.img", &sandbox_id[..8]));
        if let Some(dir) = workspace_dir {
            let size_mb = dir_size_mb(dir) + 512; // slack
            WorkspaceOverlay::pack_directory(dir, &workspace_base, size_mb)?;
        } else {
            // Empty workspace
            WorkspaceOverlay::pack_directory(
                Path::new("/dev/null"), // won't be used
                &workspace_base,
                512,
            )?;
        }

        let overlay = WorkspaceOverlay::create(sandbox_id, &workspace_base, &self.config.overlay_dir)?;

        FirecrackerVm::from_snapshot(
            sandbox_id,
            &self.config.snapshot_path,
            &self.config.snapshot_mem_path,
            overlay,
            &self.config.binary_path,
            &self.config.overlay_dir,
        ).await
    }
}

fn dir_size_mb(path: &Path) -> u64 {
    // Best-effort: walk directory and sum file sizes
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                total += meta.len();
                if meta.is_dir() {
                    total += dir_size_mb(&entry.path()) * 1024 * 1024;
                }
            }
        }
    }
    (total / (1024 * 1024)).max(64) // minimum 64 MB
}
