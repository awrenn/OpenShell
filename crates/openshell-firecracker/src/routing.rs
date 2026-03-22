// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! iptables rules that redirect VM egress through the host-side proxy.
//!
//! All TCP traffic from the VM's /30 subnet is redirected to the proxy port
//! via REDIRECT (which rewrites the destination to 127.0.0.1:<port>).
//! This is enforced at the kernel level — the VM cannot bypass it.
//!
//! Phase 4 wires this to the actual proxy process. For now the rules are
//! installed and removed but the proxy port is a placeholder.

use crate::error::{FirecrackerError, Result};
use crate::subnet::VmSubnet;
use std::process::Command;
use tracing::{info, warn};

/// Proxy port on the host where the per-VM L7 proxy listens.
/// TODO(phase4): make this per-VM and dynamic.
pub const PROXY_PORT: u16 = 3128;

/// iptables rules for one VM's tap subnet.
///
/// Removed on drop.
#[derive(Debug)]
pub struct VmRouting {
    network_cidr: String,
}

impl VmRouting {
    /// Install iptables rules for a VM's subnet.
    pub fn install(subnet: &VmSubnet) -> Result<Self> {
        let network_cidr = subnet.network_cidr();

        info!(network = %network_cidr, proxy_port = PROXY_PORT, "Installing VM routing rules");

        // Enable IP forwarding for the tap interface
        run_sysctl("net.ipv4.ip_forward", "1")?;

        // Redirect all TCP from VM subnet to proxy port
        run_iptables(&[
            "-t", "nat",
            "-A", "PREROUTING",
            "-s", &network_cidr,
            "-p", "tcp",
            "-j", "REDIRECT",
            "--to-port", &PROXY_PORT.to_string(),
        ])?;

        // Drop non-TCP egress (UDP, ICMP, etc.) — agents should only use TCP
        run_iptables(&[
            "-A", "FORWARD",
            "-s", &network_cidr,
            "!", "-p", "tcp",
            "-j", "DROP",
        ])?;

        Ok(Self { network_cidr })
    }

    fn remove(&self) {
        let _ = run_iptables(&[
            "-t", "nat",
            "-D", "PREROUTING",
            "-s", &self.network_cidr,
            "-p", "tcp",
            "-j", "REDIRECT",
            "--to-port", &PROXY_PORT.to_string(),
        ]).map_err(|e| warn!(error = %e, "Failed to remove PREROUTING rule"));

        let _ = run_iptables(&[
            "-D", "FORWARD",
            "-s", &self.network_cidr,
            "!", "-p", "tcp",
            "-j", "DROP",
        ]).map_err(|e| warn!(error = %e, "Failed to remove FORWARD rule"));

        info!(network = %self.network_cidr, "VM routing rules removed");
    }
}

impl Drop for VmRouting {
    fn drop(&mut self) {
        self.remove();
    }
}

fn run_iptables(args: &[&str]) -> Result<()> {
    let status = Command::new("iptables")
        .args(args)
        .status()
        .map_err(|e| FirecrackerError::Routing(e.to_string()))?;

    if !status.success() {
        return Err(FirecrackerError::Routing(format!(
            "iptables {} exited with {}",
            args.join(" "),
            status
        )));
    }
    Ok(())
}

fn run_sysctl(key: &str, value: &str) -> Result<()> {
    let status = Command::new("sysctl")
        .args(["-w", &format!("{key}={value}")])
        .status()
        .map_err(|e| FirecrackerError::Routing(e.to_string()))?;

    if !status.success() {
        return Err(FirecrackerError::Routing(format!("sysctl {key}={value} failed")));
    }
    Ok(())
}
