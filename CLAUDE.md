# CLAUDE.md

Guidance for Claude Code working in this repository.

## Project Scope

qOS is a collaborative Linux OS and desktop environment on **Fedora Bootc**: Niri-like strips
of non-overlapping windows, inside workspaces that carry their own collaborators, permissions
and host. It is a stack — an immutable container-based OS image, a Wayland compositor, a P2P
FUSE filesystem, a CRDT-native UI toolkit, and ~40 native apps. These are interrelated: a
change in `ui` or `fs` can break the image build; compositor changes affect sandboxing and
collaboration semantics.

Collaboration lives *below* applications rather than inside them: apps expose **resources**
and **actions** over CRDT state the system owns. That is what makes search, undo/history,
device sync, accessibility and theming system services instead of per-app features, and it
decides most user-facing design questions.

Vision docs are in `../quickdesktop-planning/` (`main.md`, `blog/`) — read them before making
user-facing design decisions rather than inferring intent from the code.

## Repository Layout

Single Cargo workspace, no submodules. Commit normally; prefer explicit paths over `git add -A`.

- `shell/` — Wayland compositor (`smithay` + `vello`/`wgpu` + `libp2p`). Workspace/strip/
  sandbox/collaboration logic. Also `qos-control` (test CLI) and `shots` (renderer).
- `ui/` — the UI toolkit: retained-mode, CRDT-native. `wgpu`+`vello`, `taffy`, `cosmic-text`,
  `automerge`, `winit`, `fluent`.
- `fs/` — FUSE3 driver, Iroh P2P, mounted at `/var/workspace`, cache at `/var/cache/qos-fs`.
- `apps/` — ~40 app crates on `ui`. `--shots PATH` gives most a headless capture.
- `crates/<domain>/` — supporting libraries (collab, doc, runtime, packaging, session, …).
- `os/` — Containerfile, systemd units, image build scripts.

Editions differ deliberately: `ui`, `shell`, `fs` on 2024; the older apps on 2021. Don't
normalise them in passing.

## Build & Test

**Run plain `cargo`.** `~/.local/bin/cargo` is a shim that routes builds into `my-distrobox`
and manages sccache, from the main tree and every worktree. **Do not set** `RUSTC_WRAPPER`,
`SCCACHE_*`, `CARGO_TARGET_DIR` or `CARGO_BUILD_JOBS` — the shim owns them. `jobs = 4` lives
in `.cargo/config.toml`; do not add a concurrency cap, it throttles the machine even when
idle. `QCARGO_DEBUG=1` prints the routing decision, `QCARGO_BYPASS=1` skips the shim.

The host has none of the native dev libraries and cannot get them (`/usr` is read-only); the
container pins the build environment. A missing one shows up as `pkg-config exited with
status code 1`. Install inside `distrobox enter my-distrobox`, never on the host.

**Scope tests to what you are proving** — `shell --lib` alone is ~1600 tests and the compile
dominates:

```bash
cargo test -p shell --lib room_activity
cargo test -p shell --test interaction_test   # headless Wayland state, no GPU
```

`cargo check -p shell --tests` before finishing: a `--lib` run never compiles integration
targets, which is how test files have silently stopped building for days at a time.

Full image: `./os/build.sh` (rootless podman; the disk-image step needs sudo and a local
registry). `./os/test-vm.sh` boots it. VM login `tester` / `quickdesktop`.

## Architecture Essentials

**`ui`** — every widget implements `Widget` (`ui/src/widget/mod.rs`). A frame is five passes:
`sync` (pull from the CRDT) → `build_layout` (Taffy) → `apply_layout` → `draw` (into a Vello
scene) → `handle_event` (which may mutate the CRDT directly; there is no message layer).
Consequences: state lives in the CRDT, not widget fields; undo/redo snapshot the document
(`State::commit_action`); focus is 2D and directional; **theming is OS-controlled — read
colours from the `Theme` passed to `draw`, never hard-code**; user-facing strings are
`fluent` keys.

**`shell`** — `Workspace → Strip → WindowGroup → Tab`. Tabs tile within a group, groups
scroll horizontally within a strip, strips stack vertically. No overlapping floating windows;
reject designs that reintroduce them. Each workspace is a bubblewrap sandbox with its own
file-access policy — **the sandbox is the security boundary, do not weaken it for
convenience**. Wayland state is decoupled from the paint layer so it runs headless.

**`fs`** — Iroh content addressing, `blake3`, `moka` cache. Metadata is visible immediately
via gossip; content hydrates on first read, so `open()` does not imply bytes are local.

## Disk, and why builds hang

One worktree's `target/` reaches 150+ GB and targets **cannot be shared between worktrees**
(cargo's artifact key omits the package path, so worktrees of one repo collide and you
silently get another's binary). Delete `target/` from worktrees whose branches have landed.

If builds *hang* rather than run slowly, that is btrfs metadata exhaustion, and deleting
files alone will not fix it — freed space returns inside existing chunks, so `df` moves while
`unallocated` stays at 2 MiB. Free something first, **then**:

```bash
sudo btrfs balance start -dusage=80 -dlimit=25 /var/home
```

Use 80, not 30: data chunks here average ~96% full, so `-dusage=30` relocates nothing and
exits 0, which looks like the remedy failing rather than not applying.

## Cargo.lock is committed — do not regenerate it

A fresh resolve does not produce a working tree: `iroh` pulls a pre-release
`ed25519-dalek 3.0.0-pre.1` stack that fails to compile against newer transitive versions, and
the failure surfaces inside a dependency rather than where you touched. qOS's own identities
resolve to the stable `ed25519-dalek 2.2.0`, which is why `rand` is pinned to 0.8. Treat a
lock change as a real change: read the diff and rebuild before pushing. `--locked` for CI.

## Traps that have cost real time

- **`git worktree remove --force` deletes untracked files permanently.** `git status --short`
  and commit anything you want to keep first.
- **`git stash` is unsafe here** — the stash stack is shared across worktrees *and* concurrent
  sessions. Commit WIP instead.
- **Adding a field to `Workspace`/`WorkspaceTree`/`Strip` in `shell/src/shell/state.rs` ripples
  to every by-field test fixture** (`shell/tests/concept_parity_spec.rs` is 1000+ lines — be
  careful). Symptom: `E0063: missing field`.
- **Never write a `pkill`/`pgrep -f` pattern that matches your own command line** — it kills
  your own shell.
- **Clean up compositors and runtime dirs you spawn.** Leaked compositors hold DRM file
  descriptors and starve the GPU for every session on the machine.

## Verification covenant

Declared in the header of `shell/tests/concept_parity_spec.rs` and applying to **all** tests:

- Never add `#[ignore]` to weaken a failing test.
- Never weaken an assertion (`==` → `>=`, commenting out an invariant).
- Always assert on the production code path, not a tautological helper.

Verify a "known-red" list at your own base commit rather than trusting a handed-down one.

## Looking at the UI

**Never** run `scripts/screenshot-app.py`, and never screenshot the user's real desktop.

```bash
cargo run --bin shots -- --mode offscreen --out target/<dir>          # stills
cargo run --bin shots -- --mode recording --scenario <n> --out <f>.h264   # motion
```

Output is identical to the live binary except for live-surface-only bugs. `QOS_SHOTS_THEME=
light|high-contrast|dark` picks the theme — check more than one, single-theme blind spots
have shipped repeatedly.

For click-and-check work see `docs/headless-testing.md` (the `QOS_TEST_CONTROL` socket and
`--backend headless`); `scripts/headless-examples/` has working ~50-LOC examples.
`ShellDriver::resolve_named_rect(name)` finds click targets via `Widget::test_id`.

Debug overlays need `QUICK_UI_DEBUG` **and** `debug_assertions`.

## Where to read first

`docs/architecture.md`, `docs/testing.md`, `docs/headless-testing.md`, `ui/HIG.md`, and
`plan/{architecture,collaboration,compositor,security}.md` before non-trivial changes in
those subsystems.
