# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Scope

qOS is a collaborative, spatial Linux OS built on **Fedora Bootc**. It is a stack, not a single app:

- An immutable container-based OS image
- A custom Wayland compositor with a Niri-inspired scrollable-strip tiling model
- A P2P FUSE filesystem that projects remote files and hydrates on demand
- A custom CRDT-native UI toolkit used by system apps
- Native system apps (terminal, file explorer, browser)

Treat these as deeply interrelated — a change in `ui` or `fs` can break the OS image build; changes to the compositor affect sandboxing and collaboration semantics.

## Repository Layout

This is a **Cargo workspace (monorepo)**. The workspace members in `Cargo.toml` are:

- `shell/` (binary `shell`) — Wayland compositor. Built on `smithay` + `vello`/`wgpu` + `libp2p` (gossipsub, mdns). Contains the workspace/strip/sandbox/collaboration logic (Niri-inspired scrollable strips; wheel-disc workspace switcher).
- `fs/` — FUSE3 driver, Iroh-based P2P, mounted at `/var/workspace` with cache at `/var/cache/qos-fs`. 10s metadata TTL, 1MB readahead.
- `ui/` (in-tree) — The UI toolkit. Retained-mode, CRDT-native. Stack: `wgpu` + `vello` (render), `taffy` (layout), `cosmic-text` (text), `automerge` (state), `winit` (windowing), `fluent` (i18n).
- `apps/terminal/` (binary `terminal`) — GPU-accelerated terminal. Uses `ui`. `--shots PATH` for headless screenshot.
- `apps/file-explorer/` — Spatial file manager. Uses `ui`. `--shots PATH` for headless screenshot.
- `apps/browser/` (binary `browser`) — Servo-based, uses `iced` directly. No `--shots` mode yet — iced 0.13 has no headless surface; deferred until upstream exposes one or the browser migrates to `ui`.
- `crates/<domain>/` — supporting libraries grouped by domain (`collab`, `doc`, `runtime`, `packaging`, `session`, `system`, `text`, `format`). See `Cargo.toml` for the full member list.
- `os/` — `Containerfile`, `Containerfile.test`, systemd units, build and VM scripts.
- `plan/`, `docs/` — Architecture, collaboration, compositor, security design notes. Read these before non-trivial changes in those areas.

Everything lives in this one repository — normal commits, no submodule pointer dance (see "Monorepo workflow" below).

## Build & Test

### First time (clone)

```bash
git clone ssh://git@code.podesta.ai/qos/qos.git
```

### Building Rust components individually (fast inner loop)

```bash
cargo build -p shell        # compositor
cargo build -p fs        # FUSE driver
cargo build -p ui             # UI toolkit
cargo build -p terminal
cargo build -p file-explorer
cargo build -p browser
```

### Build environment — just run `cargo`

**Run plain `cargo`.** `~/.local/bin/cargo` is a shim: it reads `.build-container` at the repo
root, routes the build into `my-distrobox`, enables sccache, and selects the sccache server in
that container's namespace. This works from the main tree and from every worktree with no
per-worktree setup.

```bash
cargo test -p shell
```

**Do not set** `RUSTC_WRAPPER`, `SCCACHE_DIR`, `SCCACHE_SERVER_PORT`, `SCCACHE_CACHE_SIZE`,
`CARGO_TARGET_DIR` or `CARGO_BUILD_JOBS` — the shim handles them and overriding can break the
build. `jobs = 4` lives in `.cargo/config.toml`. There is no build semaphore; memory is bounded
by systemd cgroups, so builds do not need serialising. Do not reintroduce a concurrency cap — it
throttled the machine even when idle.

`QCARGO_DEBUG=1 cargo …` prints the routing decision; `QCARGO_BYPASS=1` skips the shim.

**Never share a target dir or `build.build-dir` between worktrees.** Cargo's artifact key does not
include the package path, and worktrees of one repo share a name and version, so they collide —
you silently get another worktree's binary and cargo reports `Fresh`. Per-worktree targets plus
sccache give the dedup safely: a second worktree building the same deps takes cache hits.

The host is image-based (Bazzite/Silverblue, `/usr` read-only) and has **none** of the native
development libraries — not just capnp. Adding them would mean `rpm-ostree` layering plus a
reboot, re-layered after every base update, with a build environment that drifts as the host
image moves. The container is also what pins the build environment for everyone. Deps it needs
(install via `sudo dnf install` inside `distrobox enter my-distrobox` if it's been rebuilt):

```
xkbcommon-devel          # smithay-client-toolkit (every build)
libseat-devel            # compositor DRM backend
libinput-devel           # compositor DRM input
libudev-devel            # compositor DRM device enum
libdrm-devel             # compositor DRM
mesa-libgbm-devel        # compositor DRM buffer alloc
mesa-libGL-devel         # compositor DRM paint (Phase 5.2)
mesa-libEGL-devel        # compositor DRM paint (Phase 5.2)
pipewire-devel           # portal ScreenCast streams
nasm                     # openh264-sys2 fallback (optional; the `source` feature compiles pure-C without)
```

Missing one of these surfaces as a `pkg-config exited with status code 1` error at the top of the cargo output. Don't `sudo dnf install` on the host — install inside `distrobox enter my-distrobox`.

### Running tests

**Prefer targeted tests over workspace-wide runs** — the `shell` suite alone is ~950 tests and
the compile dominates. Scope to the crate, or to the test names you are proving:

```bash
cargo test -p fs
cargo test -p shell --lib room_activity      # just the ones you're working on
```

`shell --lib` runs cleanly under **default parallel** as of 2026-05-23. The previous SIGSEGV (wgpu Vulkan instance contention from many OffscreenRenderer-using tests creating Instances in parallel) was fixed by a process-wide shared `Arc<Mutex<RenderContext>>` in `ui/src/offscreen.rs`. The `apply_grants_dev_scan_is_memoised` memo-flake (D-6 static state) was fixed via `serial_test::serial` on the 8 `apply_grants`-touching tests.

```bash
cargo test -p shell --lib
```

If a future change reintroduces a parallel-test crash, the historical bisect threshold was 4-threads OK / 8-threads SIGSEGV — restart by running `--test-threads 4` and bisect upward.

The compositor has a headless integration test harness that exercises the Wayland state without a GPU:

```bash
cd shell && cargo test --test interaction_test
```

### Running `ui` examples

```bash
cargo run -p ui --example gallery
cargo run -p ui --example responsive_gallery
# Enable debug overlays:
QUICK_UI_DEBUG=1 cargo run -p ui --example gallery
```

### Full OS image

```bash
./os/build.sh            # production image (requires podman)
./os/verify-test.sh      # test image with validation service
./os/test-vm.sh [path/to/disk.qcow2]    # boot it in QEMU (needs OVMF + KVM)
```

`./os/build.sh` builds the container image (the Rust binaries compile *inside* it) and then calls `os/dev/disk-image.sh` to produce `os/output*/qcow2/disk.qcow2` — `systemd-repart` for the partitions, `bootc install to-filesystem` into them. `bootc-image-builder` is gone: it has no composefs/UKI surface, and it cannot install this image at all, since qOS asks for systemd-boot and the ostree backend refuses it. The container build step uses rootless `podman`; the disk-image step needs `sudo` for loopback mounts and a local registry to push to (the composefs backend will not install from container storage).

**VM login:** `quickuser` / `quickdesktop`. Verify the FUSE mount with `mountpoint /var/workspace`. The test image runs `quick-fs-test.service` on boot — check results with `journalctl -u quick-fs-test`.

## Architecture Essentials

### `ui` — the UI toolkit

Every widget implements the `Widget` trait in `ui/src/widget/mod.rs`. A frame is **five passes**, not one:

1. `sync(state, path)` — pull visual state from the Automerge CRDT document.
2. `build_layout(taffy, theme, node_map)` — build the Taffy tree; widgets that size themselves implement `measure`.
3. `apply_layout(taffy, node, origin)` — taffy's computed layout → widget's `Rect`.
4. `draw(scene, transform, text_renderer, theme)` — render into a Vello `Scene`.
5. `handle_event(event, ...)` — winit events may **mutate the CRDT directly** (e.g. text input splices into an Automerge `Text` object). No message-passing layer.

Key implications when writing or modifying widgets:
- State lives in the CRDT (`State::doc`, an `AutoCommit`), **not** in widget fields. Widget structs hold render/layout caches only.
- Undo/redo is implemented by snapshotting the full CRDT (`State::commit_action`, see `ui/src/state/mod.rs`). Widgets signal "this is an undo point" via `needs_commit()`/`on_commit()`. Ephemeral per-widget state (caret position, selection) is restored across undo via `get_state`/`set_state`.
- Focus is 2D and directional, not tab-order-only. The `App` in `ui/src/app.rs` maintains a `focusable_order` and does spatial navigation for arrow keys.
- Collaboration (`ui/src/collaboration.rs`) is implemented as a Unix-socket protocol to the compositor (`CollabRequest`/`CollabEvent`, serde-encoded). CRDT sync uses `SyncIncremental`. Remote cursors come in as `RemoteCursorMoved` events and render on top of the scene.
- Theming is **OS-controlled**: widgets must read colors/spacing from the `Theme` passed to `draw`, never hard-code. This is how the OS enforces consistent look across system apps.
- i18n via `fluent` — pass translation keys, not literal strings, to user-facing widgets.

### Compositor (`shell`)

- **Spatial model**: `Workspace → Strip → WindowGroup → Tab`. Tabs tile within a group, groups scroll horizontally within a strip (Niri-inspired), strips stack within a workspace, and workspaces are switched via the wheel disc. No overlapping floating windows — reject any design that reintroduces them.
- **Per-workspace sandbox**: each workspace is a Bubblewrap sandbox with its own file-access policy. When an app requests a path not in the workspace's allowlist, the compositor prompts the user; approvals can be granted temporarily or permanently to the workspace.
- **Collaboration**: peers can be invited to a specific workspace. Sharing is via Wayland buffer streaming + remote input injection, coordinated over `libp2p` (gossipsub + mdns). Sandboxing is the security boundary — do not weaken it for convenience.
- The Wayland state is decoupled from the graphical `vello`/`wgpu` paint layer so it can run headless for integration tests.

### `fs`

- Mounted at `/var/workspace`. Uses Iroh for content addressing + `iroh-blobs` for transfer; `blake3` hashes; `moka` for caching.
- **Projection vs. hydration**: metadata is visible immediately via P2P gossip; content is fetched on first read. Don't assume `open()` implies bytes are local.
- Cache lives at `/var/cache/qos-fs` so the root can stay read-only under bootc.

### OS image

- Base: Fedora bootc 44, mirrored to `code.podesta.ai/qos/fedora-bootc` and pinned by digest in `os/base-snapshot.env` — upstream garbage-collects the manifest a digest points at within a day, so a pin straight at quay.io stops resolving. The image is immutable; persistent state goes under `/var`.
- Boot chain: UEFI/GRUB → `greetd` (tuigreet) → `quick-fs.service` mounts the FUSE → `shell` starts as the Wayland session → `shell-firstboot.service` handles first-run setup.
- `Containerfile.test` adds a validation systemd service; use it instead of shipping test code in the production image.

## Monorepo workflow

All components (`ui`, `shell`, `fs`, `apps/*`, `os`, and the top-level crates)
live in this **single repository** — commit normally, no submodule pointer dance.
Prefer explicit paths over `git add -A` so you only stage what you intend.

### Build output is the disk budget

One worktree's `target/` reaches 150+ GB, and targets cannot be shared between worktrees (see
above). Debug profiles are pinned to `debug = "line-tables-only"` in the workspace `Cargo.toml`
for that reason — full DWARF across ~200 test binaries was the bulk of it.

Two things to know when the disk gets tight:

- `target/debug/incremental` is pure cache (~45 GB per worktree). Delete it freely; it costs one
  slower rebuild.
- **After merging a branch, delete its worktree's `target/`**, and prune the worktree once the
  branch has landed. A finished worktree holding 300 GB is the usual cause of a full disk here.

If builds *hang* rather than run slowly — uninterruptible sleep, no progress at all — that is
btrfs metadata exhaustion, not memory, and **deleting files will not help**: freed space returns
inside existing chunks, so `df` moves while `unallocated` stays at 2 MiB and metadata cannot grow.
A daily timer balances automatically; to force it:

```bash
sudo btrfs balance start -dusage=30 -dlimit=40 /var/home
```

### Cargo.lock is committed — do not regenerate it casually

`Cargo.lock` is tracked, and it has to be. A fresh resolve does not currently
produce a working tree: `iroh` pulls a **pre-release** `ed25519-dalek 3.0.0-pre.1`
+ `curve25519-dalek 5.0.0-pre.1` + `signature 3.0.0-rc.10` for its own node
identity, and against newer transitive versions that stack fails to compile. So
`cargo generate-lockfile` will hand you a repository that does not build, and the
failure appears inside a dependency rather than anywhere you touched.

That also means the lock is the only thing pinning a pre-release crypto stack in
an OS image, so treat a lock change as a real change: read the diff, and rebuild
the workspace before pushing it. `--locked` is the safe default for CI and for any
build whose result you intend to trust.

qOS's own identities never touch dalek 3 — every signing path resolves to the
stable `ed25519-dalek 2.2.0` → `curve25519-dalek 4.1.3` → `rand_core 0.6.4`. That
is why `rand` is pinned to 0.8 in `[workspace.dependencies]` rather than current:
rand 0.9+ expose a `rand_core` shape that does not satisfy dalek 2's bound.

### NEVER `git worktree remove --force` before committing untracked files

`--force` deletes the entire worktree directory — including untracked files (`??`).
If a worker created `new_module.rs` but never `git add`ed it, that file is **gone**
(untracked files were never in the object DB, so reflog / lost-found won't help).
Always `git status --short` and commit or stage anything you want to keep **before**
removing a worktree.

## Where state lives across nirios

Adding a field to `Workspace`, `WorkspaceTree`, `Strip`, or any other struct in `shell/src/shell/state.rs` ripples to **every test fixture** that constructs that struct by-field. Symptoms: cargo errors like `E0063: missing field <new_field> in initializer of <Struct>`. Files that historically need updating:

```
shell/src/portal/*.rs              # test fixtures
shell/tests/active_group_wireup_test.rs
shell/tests/cross_strip_drag.rs
shell/tests/maximize_persist_wireup_test.rs
shell/tests/concept_parity_spec.rs (1000+ lines — be careful)
```

Mitigation: prefer a `Workspace::test_default()` builder helper over hand-constructed `Workspace { ... }` blocks. The migration to that helper hasn't happened yet; until it does, expect ripples.

## Verification covenant (`tests/concept_parity_spec.rs`)

The header of `shell/tests/concept_parity_spec.rs` declares an explicit covenant:

- **Never** add `#[ignore]` to weaken a failing test
- **Never** weaken an assertion (replace `==` with `>=`, comment out a checked invariant, etc.)
- **Always** assert on the production code path, not a tautological helper

This applies to **all** test files, not just `concept_parity_spec`. The "verification mantra" surfaces in the `docs/superpowers/archive/2026-04-30-nirios-logical-bugs.md` audit too — assert on the real path, prove the fix.

## Conventions

- **Rust edition:** `ui`, `shell`, `fs` are on edition **2024**; the apps (`terminal`, `file-explorer`, `browser`) are on **2021**. Don't "normalize" these in passing.
- **Container tooling:** use rootless `podman` (no `sudo`) for image builds where possible. `os/build.sh` only needs `sudo` for the disk-image step.
- **Don't run workspace-wide `cargo test`** unless you genuinely need it — it blocks the shared target lock and is slow. Scope to `-p <crate>`.
- Debug overlays in `ui` are gated by `QUICK_UI_DEBUG` **and** `debug_assertions`; they're unavailable in release builds.
- **Never** run `scripts/screenshot-app.py` to "see" the UI. Use `cargo run --bin shots -- --mode offscreen --out target/<dir>` (still images per scenario) or `cargo run --bin shots -- --mode recording --scenario <name> --out target/<file>.h264` (H.264 video). Both produce output **identical** to the live binary.
- For **ad-hoc headless testing** (click + snapshot + check), see `docs/headless-testing.md` — covers `shots`, the `QOS_TEST_CONTROL` JSON socket, and `--backend headless`. The fastest path from "I want to test something" to "PNG on disk":
  ```bash
  ./scripts/headless-examples/full-loop.py
  # spawns compositor, drives it, writes /tmp/full-loop-{palette,final}.png, tears down. ~5s, no display.
  ```
  All five example scripts in `scripts/headless-examples/` are tested and ~50 LOC each; the `_control.py` helper in that dir is the canonical socket-client snippet to inline into your own automation.
- **E2E click targeting**: `ShellDriver::resolve_named_rect(name)` walks the live widget tree via `Widget::test_id` to find click coordinates. Add `fn test_id(&self) -> Option<&str>` to a widget impl to make it addressable by name from scenarios. Path-fallback (`Parent/Child[index]`) works without opt-in.
- **End goal**: nirios is a daily-driver collaborative spatial Linux OS. The current state is "research environment" — feature-complete for single-monitor single-user with full collaboration + sandbox semantics. The road to "daily-driver" needs multi-monitor, accessibility (AT-SPI tree), audio routing, notifications, session-lock + power-management hooks. The CRDT-native UI toolkit (`ui`) is the highest-leverage layer — every system app uses it.

## Where to read first

- `docs/architecture.md` — component overview
- `docs/testing.md` — VM + headless-compositor testing
- `plan/architecture.md`, `plan/collaboration.md`, `plan/compositor.md`, `plan/security.md` — design intent (read before non-trivial changes in those subsystems)
- `DEVELOPMENT.md` — build basics
- `docs/BRANCHBOX.md` — the feature-worktree workflow (`branchbox feature start …`) if the user is working in that model
