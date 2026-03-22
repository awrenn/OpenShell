# Firecracker Runtime — Remaining Work

Branch: `feat/firecracker-runtime`

---

## Phase 4 — Wire `FirecrackerSandboxRuntime` to real VM lifecycle

**File:** `crates/openshell-server/src/sandbox/runtime.rs`

Add `openshell-firecracker` as a dep of `openshell-server` in `Cargo.toml`, then replace the stub bodies:

- `create()` — call `FirecrackerRuntime::create_sandbox(sandbox_id, workspace_dir)`, store the returned `FirecrackerVm` somewhere (needs a `HashMap<String, FirecrackerVm>` behind a `Mutex` on the struct)
- `delete()` — look up the `FirecrackerVm` by name, call `vm.destroy(None)`
- `agent_ip()` — return `vm.guest_ip()` (already available on `FirecrackerVm`)

**Notes:**
- `FirecrackerSandboxRuntime` needs to hold `FirecrackerRuntime` + `Arc<Mutex<HashMap<String, FirecrackerVm>>>` for the live VM table
- `FirecrackerRuntime::build_base_snapshot()` must be called once at startup before `create_sandbox()` works — wire into server init or a separate CLI command
- The `workspace_dir` for `create_sandbox` maps to wherever the sandbox's working files live on the gateway host

---

## Phase 4 — Host-side proxy on the tap

**File:** `crates/openshell-firecracker/src/routing.rs`

Currently installs iptables REDIRECT to port 3128 but nothing listens there.

- Spawn a per-VM L7 proxy process (or reuse `openshell-router`) that listens on `PROXY_PORT`
- The proxy should load the sandbox's OPA policy and enforce it before forwarding
- `PROXY_PORT` is currently a static constant — make it per-VM and dynamic so multiple VMs don't collide
- Wire proxy lifecycle into `VmRouting::install` / `Drop`

**Notes:**
- `openshell-router` already exists in the workspace — check if it can be reused as a library or needs a listener mode
- The VM can't bypass iptables REDIRECT (kernel-enforced), so the proxy is the only exit point for TCP — same guarantee as the k8s HTTP CONNECT proxy

---

## Phase 5 — CLI / bootstrap

**File:** `crates/openshell-server/src/main.rs` + new `openshell-firecracker` CLI subcommand

- `--sandbox-runtime firecracker` currently starts but `create` returns "not yet implemented" — after Phase 4 this will work
- Add a `openshell gateway build-snapshot` (or similar) command that calls `FirecrackerRuntime::build_base_snapshot()` — must be run once before sandboxes can be created
- `/dev/kvm` check: `check_kvm()` exists in `vm.rs` but is only called at VM boot — surface the error earlier at server startup with a clear message
- Document the required host binaries: `firecracker`, `ip`, `losetup`, `dmsetup`, `mkfs.ext4`, `truncate`, `sysctl`, `iptables`
- `FirecrackerRuntimeConfig` paths default to `/var/lib/openshell/firecracker/` — add CLI flags or env vars to override them

---

## Known gaps / tech debt

- `build_base_snapshot()` sleeps 3 seconds waiting for the VM — replace with a real gRPC ready signal from the sandbox supervisor (`TODO(phase3)` comment in `lib.rs`)
- `derive_mac()` in `vm.rs` uses raw bytes of the sandbox ID — not guaranteed unique if IDs share a prefix; use a hash instead
- `sandbox_client` is still used directly in `spawn_kube_event_tailer` (reads k8s events) — this watcher runs even in firecracker mode right now and will fail silently; gate it on runtime type
- No test coverage for `FirecrackerSandboxRuntime` — add integration tests that mock the VM lifecycle
