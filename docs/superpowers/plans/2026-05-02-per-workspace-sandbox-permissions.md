# Per-Workspace Sandbox + Permissions — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every workspace runs apps inside a Bubblewrap sandbox with
its own user namespace, mount namespace, and a per-workspace Wayland
bridge socket. Capabilities (filesystem, hardware, network) require
explicit grants surfaced as inline prompts; grants persist in the
workspace's CRDT.

**Architecture:** Compositor allocates a per-workspace bridge socket
that proxies foreign-client traffic to its main socket while filtering
protocol messages against the workspace's grants. App-launch goes
through Bubblewrap with a per-workspace profile. Capabilities use the
existing freedesktop.org portal protocol; we ship a portal backend
that forwards prompts to the compositor's UI.

**Tech Stack:** `bwrap` CLI (Bubblewrap), `cap-std` for sandboxed FS,
`zbus` for portal D-Bus interface, smithay's `wayland-server` for the
bridge socket.

---

## Spec coverage

This plan implements §3 + §29 + §30 of
`specs/2026-05-02-nirios-foundation-design.md`. It depends on
`plans/2026-05-02-real-wayland-compositor.md` phases 0-4 (compositor
must be hosting clients before sandboxing them).

## Out of scope

- App SDK (how a `quick-ui` app declares capabilities) — separate spec.
- The portal D-Bus protocol numbers — use freedesktop.org's spec
  verbatim.
- Granting flow UI nuances (multi-step, batch grant) — separate spec.
- Network egress filtering implementation (uses kernel netfilter; not
  Rust-native) — phase 5 stubs the policy and applies it via `iptables`
  or `nftables` rules invoked from a small helper.
- FUSE per-workspace bind-mounts — covered by foundation spec §8 and
  the existing `quick-fs-fuse` work; this plan assumes
  `/var/workspace/<ws_id>/` already exists.

## Definition of done

- Launching an app into a workspace runs it in a `bwrap` sandbox that:
  - sees only `/work` (workspace's `/var/workspace/<ws_id>`) + `/cache`
    + read-only `/usr` + an empty `/home`.
  - has its own user namespace (UID 1000 → host subuid).
  - has a private DBus session bus.
  - reaches the compositor only via a per-workspace bridge socket.
- A test demonstrates: app reads `/work/notes.txt` ✓, app reads
  `/etc/passwd` is denied / shows a permission prompt.
- A capability grant (e.g. "camera") shows a prompt at the top of the
  active strip; user can allow / deny / always-allow / always-deny;
  durable choices persist in the workspace CRDT and replicate to peers
  (peers see the grant — but only the owner can grant).
- All existing tests pass + the new sandbox / permissions tests
  (target: ≥4 new integration tests).

---

## Phase 0 — Branch + dependencies

### Task 0.1: Add deps

**Files:** modify `quick-shell/Cargo.toml`

- [ ] **Step 1:**

   ```toml
   [dependencies]
   zbus = "5"
   cap-std = "3"
   uuid = { version = "1", features = ["v4"] }
   ```

- [ ] **Step 2:** verify host has `bwrap`:
   ```bash
   which bwrap && bwrap --version
   ```
   Expected: `/usr/bin/bwrap` and a version ≥0.10. If missing, document
   in the OS spec; assume present for now.

- [ ] **Step 3:** commit, branch off `nirios-shell-compositor` (depends
   on phase 1.x of the compositor plan having shipped).

---

## Phase 1 — Bubblewrap profile builder

### Task 1.1: `SandboxProfile` struct + render to `bwrap` argv

**Files:** new `quick-shell/src/sandbox/mod.rs`,
`quick-shell/src/sandbox/profile.rs`

- [ ] **Step 1: Define the type**

```rust
pub struct SandboxProfile {
    pub workspace_id: WorkspaceId,
    pub binds_ro: Vec<(PathBuf, PathBuf)>,   // host → sandbox
    pub binds_rw: Vec<(PathBuf, PathBuf)>,
    pub env: HashMap<String, String>,
    pub uid: u32,
    pub gid: u32,
    pub seccomp: Option<PathBuf>,             // path to compiled BPF blob
    pub wayland_socket: PathBuf,              // bridge socket; bind into /run/user/1000/wayland-0
    pub dbus_socket: Option<PathBuf>,
    pub net_isolated: bool,                   // unshare net
}
```

- [ ] **Step 2: Default profile constructor**

```rust
impl SandboxProfile {
    pub fn for_workspace(ws: &Workspace, ws_root: &Path, cache_root: &Path) -> Self;
}
```

Defaults:
- `binds_ro`: `/usr` → `/usr`, `/etc/resolv.conf` → `/etc/resolv.conf`
  if the workspace has network.
- `binds_rw`: `ws_root` → `/work`, `cache_root` → `/cache`,
  `/run/user/1000/<bridge>` → `/run/user/1000/wayland-0`.
- `env`: `WAYLAND_DISPLAY=wayland-0`, `XDG_RUNTIME_DIR=/run/user/1000`,
  `HOME=/work`.
- `uid`/`gid`: 1000 / 1000.
- `seccomp`: workspace-blocking BPF file (empty for v1; phase 2 fills).
- `net_isolated`: true for invited-peer sessions, false for owner's
  session.

- [ ] **Step 3: Render to `bwrap` argv**

```rust
impl SandboxProfile {
    pub fn into_argv(self, command: &[String]) -> Vec<String>;
}
```

- [ ] **Step 4: Test**

```rust
#[test]
fn profile_renders_expected_argv_for_minimal_workspace() {
    let p = SandboxProfile::for_workspace(/* … */);
    let argv = p.into_argv(&["sh".into(), "-c".into(), "echo hi".into()]);
    assert!(argv.iter().any(|a| a == "--ro-bind"));
    assert!(argv.iter().any(|a| a == "/usr"));
    assert!(argv.iter().any(|a| a == "--bind"));
    assert!(argv.windows(3).any(|w| w[0] == "--bind" && w[2] == "/work"));
}
```

- [ ] **Step 5: Commit**

### Task 1.2: Spawn helper

**Files:** new `quick-shell/src/sandbox/spawn.rs`

```rust
pub fn spawn_in_workspace(
    profile: &SandboxProfile,
    command: &[String],
) -> std::io::Result<std::process::Child>;
```

Wraps `std::process::Command::new("bwrap").args(profile.into_argv(command))`.

- [ ] **Step 1: Implement**
- [ ] **Step 2: Test (integration)**

```rust
#[test]
fn spawned_process_can_read_work_path() {
    let (ws_dir, cache_dir) = make_temp_workspace();
    std::fs::write(ws_dir.join("notes.txt"), b"hello").unwrap();
    let profile = SandboxProfile::for_workspace_test(&ws_dir, &cache_dir);
    let mut child = spawn_in_workspace(&profile, &[
        "cat".into(), "/work/notes.txt".into()
    ]).unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
}

#[test]
fn spawned_process_cannot_read_etc_passwd() {
    // /etc isn't bound; cat exits non-zero
    // (or, if /etc is partly bound for resolv.conf, the file inside
    //  the namespace doesn't exist).
}
```

- [ ] **Step 3: Commit**

---

## Phase 2 — seccomp-bpf blocklist

### Task 2.1: Compile a per-workspace BPF blob

**Files:** new `quick-shell/src/sandbox/seccomp.rs`

- [ ] **Step 1:** Use the `seccompiler` crate (host filter compiler)
   to emit a blob blocking: `ptrace`, `mount`, `umount2`, `pivot_root`,
   `chroot`, `clone3` (with namespace flags), `unshare`, `setns`,
   raw socket family `AF_PACKET`, `bpf`, `keyctl`.

- [ ] **Step 2:** Cache the compiled blob at
   `/var/cache/quick-fs/<ws_id>/seccomp.bpf` so we compile once per
   workspace.

- [ ] **Step 3: Test**

```rust
#[test]
fn seccomp_blob_compiles_and_is_nonempty() {
    let bytes = compile_workspace_seccomp().unwrap();
    assert!(!bytes.is_empty());
}
```

- [ ] **Step 4: Commit**

---

## Phase 3 — Wayland bridge socket per workspace

### Task 3.1: Per-workspace listening socket

**Files:** new `quick-shell/src/compositor/bridge.rs`

The compositor's main socket lives at `$XDG_RUNTIME_DIR/wayland-1`
(say). For each workspace, bind a second listening socket
`$XDG_RUNTIME_DIR/nirios-ws-<ws_id>` and proxy its connections to
the main socket.

The bridge **filters incoming protocol messages**:

- Pass-through: `wl_compositor`, `wl_subcompositor`, `wl_shm`,
  `xdg_shell` — basic surface lifecycle is unfiltered.
- Tagged: every connection is associated with a workspace ID at
  bind-time. The compositor's adoption logic (compositor plan §6)
  reads that tag to route surfaces.
- Blocked / prompted: `wl_data_device` for clipboard between
  workspaces (initially block, capability-gate later);
  `zwp_screencopy` (always prompt).

- [ ] **Step 1: Implement listen + proxy**
- [ ] **Step 2: Tag connections at the bridge boundary**

   Use a per-connection `WorkspaceTag` `UserData` on the smithay
   `Client` — populated when the bridge accepts the upstream connection,
   read by handlers in compositor::state.

- [ ] **Step 3: Test**

```rust
#[test]
fn bridge_tags_clients_with_workspace_id() {
    // Spin up a fake client through the bridge, assert the
    // compositor's per-client UserData has the expected workspace tag.
}
```

- [ ] **Step 4: Commit**

### Task 3.2: Wire bridge into spawn

**Files:** modify `quick-shell/src/sandbox/spawn.rs`

- [ ] **Step 1:** before `bwrap`, ensure the workspace's bridge socket
   exists (lazy-init).
- [ ] **Step 2:** profile's `wayland_socket` field points to the bridge.
- [ ] **Step 3:** end-to-end test:
   ```rust
   #[test]
   fn weston_info_can_connect_through_workspace_bridge() {
       let profile = /* … */;
       let out = spawn_in_workspace(&profile, &["weston-info".into()])
           .unwrap().wait_with_output().unwrap();
       assert!(out.status.success());
       assert!(String::from_utf8_lossy(&out.stdout).contains("wl_compositor"));
   }
   ```
- [ ] **Step 4:** commit.

---

## Phase 4 — Capability grant model + UI prompts

### Task 4.1: CRDT representation of grants

**Files:** modify `quick-shell/src/shell/state.rs`

- [ ] **Step 1:** Add to `Workspace`:

   ```rust
   #[serde(default)]
   pub grants: HashMap<AppId, Vec<Capability>>,

   pub enum Capability {
       Filesystem { path: PathBuf, mode: FsMode, scope: GrantScope },
       Camera   { scope: GrantScope },
       Microphone { scope: GrantScope },
       Screencast { scope: GrantScope },
       Network  { hosts: Option<Vec<String>>, scope: GrantScope },
   }

   pub enum GrantScope { Once, Session, Always }
   pub enum FsMode { Read, ReadWrite }
   ```

- [ ] **Step 2:** Round-trip tests.

### Task 4.2: Inline prompt widget

**Files:** new `quick-ui/src/widget/permission_prompt.rs` and chrome
integration in `quick-shell/src/shell/overlay_host.rs`.

- [ ] **Step 1:** Build a `PermissionPrompt` widget — looks like
   concept's notification bar at top-of-strip. Buttons: Allow /
   Allow this session / Always allow / Deny. Disabled "Always allow"
   for non-owners.

- [ ] **Step 2:** Pump prompts via `Chrome::pending_grants:
   VecDeque<(AppId, Capability)>`. Top prompt renders.

- [ ] **Step 3:** Tests covering each branch (allow / deny / persist).

### Task 4.3: Portal backend (D-Bus)

**Files:** new `quick-shell/src/portal/mod.rs`,
`quick-shell/src/portal/file_chooser.rs`,
`quick-shell/src/portal/screencast.rs`,
`quick-shell/src/portal/camera.rs`.

- [ ] **Step 1:** Implement minimal `org.freedesktop.portal.FileChooser`,
   `…ScreenCast`, `…Camera` interfaces via `zbus`.
- [ ] **Step 2:** Each request:
   1. Identify caller's workspace (via the bridge tag).
   2. Look up caller's app-id in `Workspace::grants`. If `Always`,
      grant immediately.
   3. Else push to `pending_grants`; await user response; cache per
      `GrantScope` rules.
- [ ] **Step 3:** `xdg-desktop-portal` config registers `nirios` as
   the backend for these interfaces.
- [ ] **Step 4:** End-to-end test using `xdg-desktop-portal-test`.

---

## Phase 5 — Default-deny enforcement

### Task 5.1: Filesystem

The bind-mount story already enforces FS isolation — apps simply
can't see paths outside `/work`/`/cache`. Capability grants outside
those roots are implemented by:

- [ ] **Step 1:** the portal `FileChooser` returns a one-shot file
   handle (open file descriptor), not a path. `cap-std` wraps it.
- [ ] **Step 2:** `Always` filesystem grants → re-mount on next workspace
   start: extend `SandboxProfile::binds_ro/rw` with the granted paths.
- [ ] **Step 3:** Tests pin the round-trip (grant → restart → app sees
   the path → revoke → restart → app doesn't).

### Task 5.2: Network

- [ ] **Step 1:** `unshare(CLONE_NEWNET)` already isolates per-workspace.
- [ ] **Step 2:** A `Network::Always { hosts: Some(["github.com"]) }`
   grant → wires a per-workspace `nftables` rule allowing only those
   hosts.
- [ ] **Step 3:** Owner workspace defaults to "no isolation"; invited
   peers default to "isolated".

### Task 5.3: Hardware

- [ ] **Step 1:** Camera/microphone capability simply doesn't bind
   `/dev/video*` / Pipewire socket without a grant.
- [ ] **Step 2:** Screencast goes through the portal; output is bound
   to the requesting app's tab only — never the full output.

---

## Phase 6 — UI + integration

### Task 6.1: WS-Settings → Permissions tab

**Files:** modify `quick-shell/src/shell/workspace_settings.rs`

- [ ] **Step 1:** Add a "Permissions" tab listing every persisted grant
   per app.
- [ ] **Step 2:** Each row has a Revoke button; click writes the
   removal to the CRDT (replicates).
- [ ] **Step 3:** Tests covering revoke flow.

### Task 6.2: Owner-only enforcement on grant rows

- [ ] **Step 1:** Non-owners see the rows but can't revoke.
- [ ] **Step 2:** Tests.

### Task 6.3: Smoke + ledger

- [ ] **Step 1:** Run the full test suite. Pre-work counts hold or grow.
- [ ] **Step 2:** Update `parity-status.md`. Mark §3, §29, §30 ✅.
- [ ] **Step 3:** Commit; bump submodule pointer.

---

## Risk register

| Risk | Mitigation |
|---|---|
| Bubblewrap version differences across distros | Pin minimum version; document. Bootc image controls. |
| seccomp-bpf blocking too much / breaking apps | Conservative blocklist v1; expand as we learn. |
| nftables availability on minimal images | Make network isolation optional via a `nirios-network-policy` helper that's a no-op if `nftables` is missing. Document in OS spec. |
| Portal protocol churn | Use the latest stable `xdg-desktop-portal` interface version; pin in Cargo + document. |
| Phase 1 test setup (creating tmp workspaces) is non-trivial | Build a `make_temp_workspace()` test helper early; reuse across phases. |

## Success metrics

- Owner spawns `firefox` into a workspace. Firefox writes to
  `/work/Downloads`. Firefox cannot read `/home/quinten/.ssh`.
- Owner clicks "Share screen" in a video call. Compositor prompt
  appears. Allow → Firefox sees its own surface only.
- Invited peer Alice spawns `vim` into a workspace via the palette.
  Vim cannot reach the network. Owner sees a "Network access?" prompt
  in their notification strip.

