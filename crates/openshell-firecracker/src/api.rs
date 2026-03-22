// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Firecracker REST API client over a Unix domain socket.
//!
//! Firecracker exposes a simple HTTP/1.1 API. We use raw tokio UnixStream
//! rather than pulling in a full HTTP client — the API surface is small and
//! well-defined.

use crate::error::{FirecrackerError, Result};
use serde::Serialize;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::debug;

/// Thin client for the Firecracker REST API.
pub struct FcApi {
    socket_path: std::path::PathBuf,
}

impl FcApi {
    pub fn new(socket_path: &Path) -> Self {
        Self { socket_path: socket_path.to_owned() }
    }

    /// PUT a JSON body to a Firecracker API endpoint.
    pub async fn put<T: Serialize>(&self, path: &str, body: &T) -> Result<()> {
        let body = serde_json::to_string(body)
            .map_err(|e| FirecrackerError::Api { status: 0, body: e.to_string() })?;
        let response = self.request("PUT", path, Some(&body)).await?;
        if response.status < 200 || response.status >= 300 {
            return Err(FirecrackerError::Api { status: response.status, body: response.body });
        }
        debug!(method = "PUT", path, status = response.status, "FC API OK");
        Ok(())
    }

    /// PATCH a JSON body to a Firecracker API endpoint.
    pub async fn patch<T: Serialize>(&self, path: &str, body: &T) -> Result<()> {
        let body = serde_json::to_string(body)
            .map_err(|e| FirecrackerError::Api { status: 0, body: e.to_string() })?;
        let response = self.request("PATCH", path, Some(&body)).await?;
        if response.status < 200 || response.status >= 300 {
            return Err(FirecrackerError::Api { status: response.status, body: response.body });
        }
        debug!(method = "PATCH", path, status = response.status, "FC API OK");
        Ok(())
    }

    async fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<Response> {
        let mut stream = UnixStream::connect(&self.socket_path).await.map_err(|e| {
            FirecrackerError::ApiConnect(e.to_string())
        })?;

        let content_length = body.map(|b| b.len()).unwrap_or(0);
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\nAccept: application/json\r\n\r\n{}",
            body.unwrap_or("")
        );

        stream.write_all(req.as_bytes()).await?;

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        let raw = String::from_utf8_lossy(&buf);

        parse_response(&raw)
    }
}

struct Response {
    status: u16,
    body: String,
}

fn parse_response(raw: &str) -> Result<Response> {
    // HTTP/1.1 <status> <reason>\r\n...
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| FirecrackerError::ApiConnect("malformed HTTP response".into()))?;

    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();

    Ok(Response { status, body })
}

// ── Request body types ────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct MachineConfig {
    pub vcpu_count: u32,
    pub mem_size_mib: u32,
}

#[derive(Serialize)]
pub struct BootSource {
    pub kernel_image_path: String,
    pub boot_args: String,
}

#[derive(Serialize)]
pub struct Drive {
    pub drive_id: String,
    pub path_on_host: String,
    pub is_root_device: bool,
    pub is_read_only: bool,
}

#[derive(Serialize)]
pub struct NetworkInterface {
    pub iface_id: String,
    pub host_dev_name: String,
    pub guest_mac: String,
}

#[derive(Serialize)]
pub struct InstanceAction {
    pub action_type: String,
}

#[derive(Serialize)]
pub struct SnapshotCreate {
    pub snapshot_type: String,
    pub snapshot_path: String,
    pub mem_file_path: String,
}

#[derive(Serialize)]
pub struct SnapshotLoad {
    pub snapshot_path: String,
    pub mem_backend: MemBackend,
    pub enable_diff_snapshots: bool,
    pub resume_vm: bool,
}

#[derive(Serialize)]
pub struct MemBackend {
    pub backend_type: String,
    pub backend_path: String,
}
