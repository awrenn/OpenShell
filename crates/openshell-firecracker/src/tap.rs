// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! TAP device lifecycle for Firecracker VM networking.
//!
//! Each VM gets a dedicated tap device. All VM egress goes through this tap.
//! iptables rules (added in routing.rs) redirect that egress to the host proxy.

use crate::error::{FirecrackerError, Result};
use crate::subnet::VmSubnet;
use std::process::Command;
use tracing::{debug, info, warn};

/// A TAP device allocated for one Firecracker VM.
///
/// Cleaned up automatically on drop.
#[derive(Debug)]
pub struct TapDevice {
    /// Interface name, e.g. "fc-tap-a1b2c3d4"
    pub name: String,
    /// Subnet assigned to this tap.
    pub subnet: VmSubnet,
}

impl TapDevice {
    /// Create a new tap device and bring it up with the host-side IP.
    pub fn create(sandbox_id: &str) -> Result<Self> {
        let short = &sandbox_id[..8];
        let name = format!("fc-tap-{short}");
        let subnet = VmSubnet::allocate();

        info!(tap = %name, host_ip = %subnet.host_ip, guest_ip = %subnet.guest_ip, "Creating tap device");

        // Create tap
        run_ip(&["tuntap", "add", "dev", &name, "mode", "tap"]).map_err(|e| {
            FirecrackerError::Tap(format!("tuntap add failed: {e}"))
        })?;

        // Assign host IP
        run_ip(&["addr", "add", &subnet.host_cidr(), "dev", &name]).map_err(|e| {
            FirecrackerError::Tap(format!("addr add failed: {e}"))
        })?;

        // Bring up
        run_ip(&["link", "set", &name, "up"]).map_err(|e| {
            FirecrackerError::Tap(format!("link set up failed: {e}"))
        })?;

        debug!(tap = %name, "Tap device ready");
        Ok(Self { name, subnet })
    }

    /// Delete the tap device.
    pub fn destroy(&self) {
        if let Err(e) = run_ip(&["tuntap", "del", "dev", &self.name, "mode", "tap"]) {
            warn!(tap = %self.name, error = %e, "Failed to delete tap device");
        } else {
            info!(tap = %self.name, "Tap device deleted");
        }
    }
}

impl Drop for TapDevice {
    fn drop(&mut self) {
        self.destroy();
    }
}

fn run_ip(args: &[&str]) -> Result<()> {
    let status = Command::new("ip")
        .args(args)
        .status()
        .map_err(|e| FirecrackerError::Tap(format!("ip command failed: {e}")))?;

    if !status.success() {
        return Err(FirecrackerError::Tap(format!(
            "ip {} exited with {}",
            args.join(" "),
            status
        )));
    }
    Ok(())
}
