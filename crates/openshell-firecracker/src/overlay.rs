// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! dm-snapshot copy-on-write workspace overlays.
//!
//! Each sandbox gets a sparse writable snapshot on top of a read-only base
//! ext4 image. The base image is packed once from a host directory; individual
//! sandbox writes never touch it.
//!
//! Layout:
//!   /var/lib/openshell/firecracker/workspace-base.img   (read-only base)
//!   /var/lib/openshell/firecracker/overlays/<id>.cow    (per-sandbox COW)
//!   /dev/mapper/fc-ws-<id>                              (writable snapshot device)

use crate::error::{FirecrackerError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, warn};

const COW_SIZE_MB: u64 = 2048; // 2 GB sparse COW backing file

/// A dm-snapshot overlay for one sandbox's workspace drive.
///
/// Dropped on destroy — detaches the dm device and removes the COW file.
#[derive(Debug)]
pub struct WorkspaceOverlay {
    /// dm-snapshot device name, e.g. "fc-ws-a1b2c3d4"
    pub dm_name: String,
    /// Block device path: /dev/mapper/fc-ws-<id>
    pub device_path: PathBuf,
    /// COW backing file path
    cow_path: PathBuf,
    /// Loop device for the base image (e.g. /dev/loop5)
    base_loop: String,
    /// Loop device for the COW file (e.g. /dev/loop6)
    cow_loop: String,
}

impl WorkspaceOverlay {
    /// Create a new snapshot overlay for a sandbox.
    ///
    /// `base_img` must be a valid ext4 image. It is mounted read-only.
    /// `overlay_dir` is where the per-sandbox COW file is stored.
    pub fn create(sandbox_id: &str, base_img: &Path, overlay_dir: &Path) -> Result<Self> {
        let short = &sandbox_id[..8];
        let dm_name = format!("fc-ws-{short}");
        let cow_path = overlay_dir.join(format!("{short}.cow"));
        let device_path = PathBuf::from(format!("/dev/mapper/{dm_name}"));

        info!(sandbox_id = %short, dm = %dm_name, "Creating workspace overlay");

        // Create sparse COW backing file
        truncate_sparse(&cow_path, COW_SIZE_MB)?;

        // Attach base image to a free loop device (read-only)
        let base_loop = losetup_attach(base_img, true)?;

        // Attach COW file to a free loop device
        let cow_loop = losetup_attach(&cow_path, false)?;

        // Get sector count for the base image
        let sectors = blockdev_getsz(&base_loop)?;

        // Create dm-snapshot
        // Table: "0 <sectors> snapshot <origin> <cow> P <chunk_sectors>"
        // P = persistent (survives reboots)
        // 8 = 4KB chunk size
        let table = format!("0 {sectors} snapshot {base_loop} {cow_loop} P 8");
        dmsetup_create(&dm_name, &table)?;

        debug!(device = %device_path.display(), "Workspace overlay ready");

        Ok(Self {
            dm_name,
            device_path,
            cow_path,
            base_loop,
            cow_loop,
        })
    }

    /// Pack a host directory into an ext4 image (the shared base image).
    ///
    /// This is called once when setting up the base workspace, not per-sandbox.
    /// `size_mb` should be directory size + generous slack.
    pub fn pack_directory(src: &Path, dest: &Path, size_mb: u64) -> Result<()> {
        info!(src = %src.display(), dest = %dest.display(), size_mb, "Packing directory into ext4");

        // Create blank ext4 image
        truncate_sparse(dest, size_mb)?;
        run_cmd("mkfs.ext4", &["-q", &dest.to_string_lossy()]).map_err(|e| {
            FirecrackerError::Overlay(format!("mkfs.ext4 failed: {e}"))
        })?;

        // Mount and populate
        let mount_dir = tempfile_dir()?;
        run_cmd("mount", &["-o", "loop", &dest.to_string_lossy(), &mount_dir.to_string_lossy()])
            .map_err(|e| FirecrackerError::Overlay(format!("mount failed: {e}")))?;

        let copy_result = run_cmd(
            "cp",
            &["-a", &format!("{}/.", src.to_string_lossy()), &mount_dir.to_string_lossy()],
        );

        run_cmd("umount", &[&mount_dir.to_string_lossy()]).ok();
        std::fs::remove_dir_all(&mount_dir).ok();

        copy_result.map_err(|e| FirecrackerError::Overlay(format!("copy failed: {e}")))?;

        info!(dest = %dest.display(), "Directory packed into ext4");
        Ok(())
    }

    /// Sync changes from the overlay back to the host directory.
    ///
    /// Mounts the overlay device, copies modified files back to `dest`.
    pub fn sync_back(&self, dest: &Path) -> Result<()> {
        info!(dm = %self.dm_name, dest = %dest.display(), "Syncing workspace back to host");

        let mount_dir = tempfile_dir()?;
        run_cmd("mount", &[&self.device_path.to_string_lossy(), &mount_dir.to_string_lossy()])
            .map_err(|e| FirecrackerError::Overlay(format!("mount failed: {e}")))?;

        let result = run_cmd(
            "cp",
            &["-a", &format!("{}/.", mount_dir.to_string_lossy()), &dest.to_string_lossy()],
        );

        run_cmd("umount", &[&mount_dir.to_string_lossy()]).ok();
        std::fs::remove_dir_all(&mount_dir).ok();

        result.map_err(|e| FirecrackerError::Overlay(format!("sync back failed: {e}")))?;
        Ok(())
    }

    fn destroy(&self) {
        if let Err(e) = dmsetup_remove(&self.dm_name) {
            warn!(dm = %self.dm_name, error = %e, "Failed to remove dm device");
        }
        if let Err(e) = losetup_detach(&self.base_loop) {
            warn!(dev = %self.base_loop, error = %e, "Failed to detach base loop");
        }
        if let Err(e) = losetup_detach(&self.cow_loop) {
            warn!(dev = %self.cow_loop, error = %e, "Failed to detach COW loop");
        }
        if let Err(e) = std::fs::remove_file(&self.cow_path) {
            warn!(path = %self.cow_path.display(), error = %e, "Failed to remove COW file");
        }
        info!(dm = %self.dm_name, "Workspace overlay destroyed");
    }
}

impl Drop for WorkspaceOverlay {
    fn drop(&mut self) {
        self.destroy();
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn truncate_sparse(path: &Path, size_mb: u64) -> Result<()> {
    let size = format!("{}M", size_mb);
    run_cmd("truncate", &["-s", &size, &path.to_string_lossy()])
        .map_err(|e| FirecrackerError::Overlay(format!("truncate failed: {e}")))
}

fn losetup_attach(path: &Path, read_only: bool) -> Result<String> {
    let path_str = path.to_string_lossy().into_owned();
    let mut args = vec!["-f", "--show"];
    if read_only { args.push("-r"); }
    args.push(&path_str);

    let out = Command::new("losetup")
        .args(&args)
        .output()
        .map_err(|e| FirecrackerError::Overlay(format!("losetup failed: {e}")))?;

    if !out.status.success() {
        return Err(FirecrackerError::Overlay(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn losetup_detach(dev: &str) -> Result<()> {
    run_cmd("losetup", &["-d", dev])
        .map_err(|e| FirecrackerError::Overlay(format!("losetup -d failed: {e}")))
}

fn blockdev_getsz(dev: &str) -> Result<u64> {
    let out = Command::new("blockdev")
        .args(["--getsz", dev])
        .output()
        .map_err(|e| FirecrackerError::Overlay(format!("blockdev failed: {e}")))?;

    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|e| FirecrackerError::Overlay(format!("blockdev parse failed: {e}")))
}

fn dmsetup_create(name: &str, table: &str) -> Result<()> {
    run_cmd("dmsetup", &["create", name, "--table", table])
        .map_err(|e| FirecrackerError::Overlay(format!("dmsetup create failed: {e}")))
}

fn dmsetup_remove(name: &str) -> Result<()> {
    run_cmd("dmsetup", &["remove", name])
        .map_err(|e| FirecrackerError::Overlay(format!("dmsetup remove failed: {e}")))
}

fn tempfile_dir() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("fc-mount-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn run_cmd(cmd: &str, args: &[&str]) -> std::result::Result<(), String> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| e.to_string())?;

    if !status.success() {
        return Err(format!("{cmd} exited with {status}"));
    }
    Ok(())
}
