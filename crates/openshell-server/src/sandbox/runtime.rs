// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `SandboxRuntime` trait — abstraction over sandbox execution backends.
//!
//! Current implementations:
//! - [`super::SandboxClient`] — Kubernetes-backed (via k8s agents.x-k8s.io CRDs)
//! - [`FirecrackerSandboxRuntime`] — Firecracker microVM (stub; Phase 4 wires the proxy)

use openshell_core::proto::Sandbox;
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;

/// Boxed future alias used for object-safe async trait methods.
type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Error returned by [`SandboxRuntime::create`].
#[derive(Debug)]
pub enum RuntimeCreateError {
    /// A sandbox with this name already exists.
    AlreadyExists,
    /// Any other failure.
    Other(anyhow::Error),
}

impl fmt::Display for RuntimeCreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => write!(f, "sandbox already exists"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RuntimeCreateError {}

/// Abstraction over sandbox execution backends.
///
/// Implementations must be `Send + Sync` so they can be stored in `Arc` and
/// shared across async task boundaries.
pub trait SandboxRuntime: Send + Sync + fmt::Debug {
    /// Default container/disk image for new sandboxes (used when the spec
    /// does not specify one).
    fn default_image(&self) -> &str;

    /// Verify the runtime can satisfy a GPU sandbox request.
    ///
    /// Returns a ready-to-return `tonic::Status` on failure so callers in the
    /// gRPC layer can propagate it directly.
    fn validate_gpu_support(&self) -> BoxFut<'_, Result<(), tonic::Status>>;

    /// Create a new sandbox from `sandbox.spec`.
    ///
    /// Returns [`RuntimeCreateError::AlreadyExists`] when a sandbox with the
    /// same name is already running, so callers can return a typed gRPC error.
    fn create<'a>(&'a self, sandbox: &'a Sandbox) -> BoxFut<'a, Result<(), RuntimeCreateError>>;

    /// Delete a sandbox by name.
    ///
    /// Returns `Ok(true)` when the sandbox was found and deleted, `Ok(false)`
    /// when it was already gone.
    fn delete<'a>(&'a self, sandbox_name: &'a str)
        -> BoxFut<'a, Result<bool, anyhow::Error>>;

    /// Resolve the IP address for the running agent process inside a sandbox.
    ///
    /// `identifier` is backend-specific: for Kubernetes it is the pod name; for
    /// Firecracker it is the sandbox ID (used to look up the tap subnet).
    ///
    /// Returns `Ok(None)` when the sandbox is not yet ready.
    fn agent_ip<'a>(
        &'a self,
        identifier: &'a str,
    ) -> BoxFut<'a, Result<Option<IpAddr>, anyhow::Error>>;
}

// ── Firecracker stub ──────────────────────────────────────────────────────────

/// Firecracker microVM runtime.
///
/// This is a Phase-3 stub that wires up the trait surface so the server
/// compiles with `--sandbox-runtime firecracker`.  The actual VM lifecycle
/// (boot, snapshot, overlay) lives in `openshell-firecracker` and will be
/// delegated to in Phase 4.
#[derive(Debug)]
pub struct FirecrackerSandboxRuntime {
    default_image: String,
}

impl FirecrackerSandboxRuntime {
    pub fn new(default_image: String) -> Self {
        Self { default_image }
    }
}

impl SandboxRuntime for FirecrackerSandboxRuntime {
    fn default_image(&self) -> &str {
        &self.default_image
    }

    fn validate_gpu_support(&self) -> BoxFut<'_, Result<(), tonic::Status>> {
        Box::pin(async {
            Err(tonic::Status::unimplemented(
                "GPU sandboxes are not supported by the Firecracker runtime",
            ))
        })
    }

    fn create<'a>(&'a self, _sandbox: &'a Sandbox) -> BoxFut<'a, Result<(), RuntimeCreateError>> {
        Box::pin(async {
            Err(RuntimeCreateError::Other(anyhow::anyhow!(
                "Firecracker runtime: create not yet implemented (Phase 4)"
            )))
        })
    }

    fn delete<'a>(
        &'a self,
        _sandbox_name: &'a str,
    ) -> BoxFut<'a, Result<bool, anyhow::Error>> {
        Box::pin(async {
            Err(anyhow::anyhow!(
                "Firecracker runtime: delete not yet implemented (Phase 4)"
            ))
        })
    }

    fn agent_ip<'a>(
        &'a self,
        _identifier: &'a str,
    ) -> BoxFut<'a, Result<Option<IpAddr>, anyhow::Error>> {
        Box::pin(async {
            Err(anyhow::anyhow!(
                "Firecracker runtime: agent_ip not yet implemented (Phase 4)"
            ))
        })
    }
}
