// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Per-VM /30 subnet allocation from the 192.168.100.0/24 range.
//!
//! Each VM gets a unique /30:
//!   index 0 → 192.168.100.0/30  (host: .1, guest: .2)
//!   index 1 → 192.168.100.4/30  (host: .5, guest: .6)
//!   ...
//!   index 62 → 192.168.100.248/30 (host: .249, guest: .250)
//!
//! Max 63 concurrent Firecracker sandboxes per gateway.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU8, Ordering};

static NEXT_INDEX: AtomicU8 = AtomicU8::new(0);

const BASE_A: u8 = 192;
const BASE_B: u8 = 168;
const BASE_C: u8 = 100;

/// Addresses for one VM's /30 subnet.
#[derive(Debug, Clone)]
pub struct VmSubnet {
    /// Host-side IP (proxy binds here).
    pub host_ip: Ipv4Addr,
    /// Guest-side IP (VM's eth0).
    pub guest_ip: Ipv4Addr,
    /// Prefix length (always 30).
    pub prefix_len: u8,
}

impl VmSubnet {
    /// Allocate the next available subnet. Wraps at 63.
    pub fn allocate() -> Self {
        let idx = NEXT_INDEX.fetch_add(1, Ordering::Relaxed) % 63;
        let base = idx * 4;
        Self {
            host_ip: Ipv4Addr::new(BASE_A, BASE_B, BASE_C, base + 1),
            guest_ip: Ipv4Addr::new(BASE_A, BASE_B, BASE_C, base + 2),
            prefix_len: 30,
        }
    }

    pub fn host_cidr(&self) -> String {
        format!("{}/{}", self.host_ip, self.prefix_len)
    }

    pub fn network_cidr(&self) -> String {
        let o = self.host_ip.octets();
        format!("{}.{}.{}.{}/30", o[0], o[1], o[2], o[3] - 1)
    }
}
