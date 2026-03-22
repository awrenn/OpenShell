// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FirecrackerError {
    #[error("tap device error: {0}")]
    Tap(String),

    #[error("overlay error: {0}")]
    Overlay(String),

    #[error("Firecracker API error ({status}): {body}")]
    Api { status: u16, body: String },

    #[error("Firecracker API connection error: {0}")]
    ApiConnect(String),

    #[error("VM process error: {0}")]
    Process(String),

    #[error("snapshot error: {0}")]
    Snapshot(String),

    #[error("routing error: {0}")]
    Routing(String),

    #[error("/dev/kvm is not available — Firecracker requires KVM")]
    KvmUnavailable,

    #[error("firecracker binary not found at {path}")]
    BinaryNotFound { path: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, FirecrackerError>;
