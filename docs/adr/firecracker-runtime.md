# ADR: Firecracker Sandbox Runtime

## Goal

Add a Firecracker microVM runtime as an alternative to the Kubernetes pod
sandbox. The agent runs inside a real isolated kernel via KVM. OpenShell's
L7 network proxy and OPA policy enforcement are preserved — the proxy moves
to the host side of the VM's tap device, which is strictly stronger than the
current in-pod placement.

---

## Scope

This is broken into five sequential phases. Each phase can be reviewed
independently.

---

## Phase 1 — Proto + config types

**Files:** `proto/sandbox.proto`, `crates/openshell-core/`

Add a `SandboxRuntime` enum to the sandbox proto:

```proto
enum SandboxRuntime {
  SANDBOX_RUNTIME_CONTAINER = 0;   // default, current behaviour
  SANDBOX_RUNTIME_FIRECRACKER = 1;
}
```

Add a `FirecrackerConfig` message for per-sandbox VM settings:

```proto
message FirecrackerConfig {
  uint32 vcpu_count  = 1;  // default 2
  uint32 mem_mib     = 2;  // default 2048
  string kernel_path = 3;  // host path to vmlinux
  string rootfs_path = 4;  // host path to base ext4 image
}
```

Wire `SandboxRuntime` and `FirecrackerConfig` into `SandboxSpec`.

**Why first:** everything downstream depends on these types.

---

## Phase 2 — New crate: `openshell-firecracker`

**Files:** `crates/openshell-firecracker/` (new)

Owns all Firecracker-specific lifecycle logic. Keeps this out of
`openshell-server` and testable in isolation.

Responsibilities:

- **Tap management** — create/delete a `tap-<sandbox-id>` device, assign
  the host-side IP (`192.168.100.1/30` per VM in a dedicated subnet)
- **Rootfs overlay** — create a per-VM writable overlay on top of the
  shared base ext4 image using dm-snapshot or a loop-mounted overlayfs
- **VM config** — write the Firecracker JSON config (kernel, drives, tap
  interface, vcpu/mem). No network interfaces in the VM config means
  the tap is the only egress path.
- **Process management** — spawn `firecracker --no-api --config-file`,
  hold the child handle, report exit status
- **iptables routing** — on tap creation, add rules that redirect all VM
  egress (from the tap subnet) to the proxy port on the host. On
  destruction, remove them.

Key types:

```rust
pub struct FirecrackerVm { /* tap, process handle, overlay path */ }

impl FirecrackerVm {
    pub async fn create(config: &FirecrackerConfig, sandbox_id: &str) -> Result<Self>;
    pub async fn destroy(self) -> Result<()>;
    pub fn host_ip(&self) -> IpAddr;    // proxy bind address
    pub fn guest_ip(&self) -> IpAddr;   // VM-side address
}
```

---

## Phase 3 — Runtime abstraction in `openshell-server`

**Files:** `crates/openshell-server/src/sandbox/mod.rs`,
`crates/openshell-server/src/lib.rs`

Currently `ServerState` holds a concrete `SandboxClient` (Kubernetes).
Replace with a trait so the server doesn't know which runtime it's talking to:

```rust
#[async_trait]
pub trait SandboxRuntime: Send + Sync {
    async fn create(&self, sandbox: &Sandbox) -> Result<(), RuntimeError>;
    async fn delete(&self, sandbox_id: &str) -> Result<(), RuntimeError>;
    async fn agent_ip(&self, sandbox_id: &str) -> Result<Option<IpAddr>, RuntimeError>;
    fn default_image(&self) -> &str;
}
```

Rename the existing `SandboxClient` to `KubernetesSandboxRuntime`, implement
the trait on it. Add `FirecrackerSandboxRuntime` that wraps `FirecrackerVm`.

`ServerState.sandbox_client` becomes `sandbox_runtime: Arc<dyn SandboxRuntime>`.

Selected at startup based on config flag `--sandbox-runtime container|firecracker`.

---

## Phase 4 — Proxy on host-side tap

**Files:** `crates/openshell-firecracker/` (tap routing),
`crates/openshell-server/` (proxy spawn for FC sandboxes)

For container sandboxes the proxy runs inside the pod (current behaviour,
unchanged). For Firecracker sandboxes the proxy runs on the **host**, bound
to the tap's host-side IP.

The proxy and OPA engine code in `crates/openshell-sandbox/src/proxy.rs` are
reused as a library — no logic changes. The difference is the process that
hosts them:

- **Container:** sandbox supervisor binary inside the pod
- **Firecracker:** a lightweight host-side proxy process spawned by the
  gateway per VM, bound to `192.168.100.1:3128` for that VM's tap subnet

iptables rules (added in Phase 2) redirect all egress from the VM's tap
subnet to this port. The VM guest's default route goes through the tap —
the proxy is unavoidable.

Policy hot-reload still works: the gateway pushes a new policy revision, the
host proxy process reloads it, same as the in-pod poll loop today.

---

## Phase 5 — CLI + bootstrap

**Files:** `crates/openshell-cli/src/`, `crates/openshell-bootstrap/`

CLI:

```bash
openshell sandbox create --runtime firecracker -- claude
openshell sandbox create --runtime firecracker --vcpus 4 --mem 4096 -- claude
```

`--runtime` defaults to `container`. Add `--vcpus` and `--mem` flags that
map to `FirecrackerConfig`.

Bootstrap:

- Add `/dev/kvm` availability check to `openshell gateway start`
- Add Firecracker binary download to the gateway container image
  (or detect it on the host path)
- Add kernel image management (download or accept `--kernel-path`)

---

## What does not change

| Component | Reason |
|---|---|
| OPA policy engine | Runs inside VM guest, identical Linux environment |
| Landlock + seccomp | Guest kernel supports them, sandbox binary unchanged |
| Policy YAML format | No changes |
| gRPC gateway API | No changes |
| Credential injection | VM connects to gateway gRPC via tap, same poll loop |
| SSH server (russh) | Exposed on guest IP via tap, CLI connects same way |
| Policy hot-reload | Host proxy reloads on version change, same mechanism |
| TUI | Reads sandbox state from gateway, no runtime awareness needed |

---

## Security posture vs current design

| Property | Container (current) | Firecracker |
|---|---|---|
| Kernel isolation | Landlock + seccomp on shared kernel | Separate kernel (KVM boundary) |
| Proxy location | Inside pod, same namespace as agent | Host-side tap, outside VM entirely |
| Proxy bypassable via kernel exploit | Yes (shared kernel) | No (VM boundary) |
| L7 network policy | ✅ | ✅ |
| Hot-reload | ✅ | ✅ |

---

## Open questions

1. **Rootfs delivery** — base ext4 built from the community sandbox image.
   How do we handle image updates? Pull from registry and rebuild, or ship
   pre-built ext4 artifacts alongside the cluster image?

2. **Guest init** — `openshell-sandbox` as PID 1, or thin init (tini) +
   sandbox binary? PID 1 needs signal handling and zombie reaping.

3. **Nested virt fallback** — if `/dev/kvm` is unavailable, error clearly
   or fall back to container runtime?

4. **Snapshot boot** — Firecracker can restore from a snapshot in ~150ms.
   Worth doing for fast sandbox creation, or scope creep for v1?
