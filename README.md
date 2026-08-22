# qOS

A collaborative Linux desktop, shipped as an image-based OS: Niri-like strips of
non-overlapping windows, inside workspaces that carry their own collaborators, permissions
and host. Collaboration lives below applications rather than inside them — apps expose
resources and actions over CRDT state the system owns, which is what makes search,
undo/history, device sync, accessibility and theming system services instead of per-app
features.

Built as a vertically-integrated stack:

- **`shell/`** — Wayland compositor. Niri-inspired scrollable strips of tiled window-groups; wheel-disc workspace switcher; no overlapping windows. Per-workspace bubblewrap sandbox with seccomp deny-by-default. Built on `smithay` + `vello` + `wgpu`. Collaboration over `libp2p` (gossipsub + mDNS) + `automerge` incremental sync.
- **`ui/`** — Retained-mode CRDT-native UI toolkit. Every widget reads/writes a shared automerge document, so undo, redo, persistence, and remote collaboration all share one mechanism. Stack: `wgpu` + `vello` + `taffy` + `cosmic-text` + `winit` + `fluent`.
- **`fs/`** — FUSE3 driver. Iroh content addressing, `blake3` hashes, `moka` cache. Mounted at `/var/workspace`; cache at `/var/cache/qos-fs`. Projects metadata on first byte; hydrates content on demand.
- **`apps/`** — 40+ native applications (terminal, file-explorer, browser, mail, calendar, sheets, music, maps, code_editor, …), each its own crate on `ui`. The browser embeds Servo for page rendering. See [`apps/README.md`](apps/README.md).
- **`os/`** — Bootc-based Fedora image. Greetd → Wayland session → `shell`. Disk image via `os/dev/disk-image.sh` (`systemd-repart` + `bootc install`).

## Repository layout

Single Cargo workspace (monorepo) — no submodules. Key members:

| Path | Binary |
|---|---|
| `shell/` | `shell` (compositor), `qos-control` (test CLI), `shots` (screenshot tool) |
| `fs/` | `fs` (FUSE driver) |
| `ui/` | library only |
| `apps/` | 40+ app crates — `terminal`, `file-explorer`, `browser`, `mail`, … (see [`apps/README.md`](apps/README.md)) |
| `crates/` | supporting libraries grouped by domain (collab, doc, runtime, packaging, …) |
| `os/` | Containerfile + systemd units + image build scripts |

This repo owns the workspace Cargo.toml, the docs, and the OS image build.

## Building

The host probably doesn't have all the build dependencies (`xkbcommon`, `libseat`, `pipewire`, `mesa-gbm`, …). The workflow assumes a [distrobox](https://distrobox.it) container named `my-distrobox` with the deps installed:

```bash
distrobox enter my-distrobox -- bash -c 'cd $(pwd) && cargo build -p shell'
```

See [`DEVELOPMENT.md`](DEVELOPMENT.md) for the full distrobox setup and the list of system packages.

Targeted crate builds (faster inner loop):

```bash
cargo build -p shell                     # compositor + qos-control + shots
cargo build -p fs                        # FUSE driver
cargo build -p ui                        # UI toolkit
cargo build -p terminal                  # terminal app
cargo build -p file-explorer
cargo build -p browser
```

Full OS image:

```bash
./os/build.sh                            # production qcow2 (needs podman)
./os/verify-test.sh                      # test image with boot-time validation
./os/test-vm.sh                          # boot it in QEMU (needs OVMF + KVM)
```

## Testing

Scope tests to the crate you're changing — workspace-wide runs are slow and block the shared target lock:

```bash
distrobox enter my-distrobox -- bash -c 'cd $(pwd) && cargo test -p shell --lib'
distrobox enter my-distrobox -- bash -c 'cd $(pwd) && cargo test -p ui --lib'
```

Current ledger: ~435 shell lib tests + ~269 ui lib tests, all passing under default parallel.

The compositor has a headless integration harness — `shell/tests/interaction_test.rs` exercises the Wayland state without a GPU.

## Headless testing

Every system app can render offscreen to a PNG with no display attached:

```bash
cargo run -p shell --bin shots -- --mode offscreen --out target/shots
cargo run -p shell --bin shots -- --mode recording --scenario palette --out target/palette.h264
```

The compositor exposes a JSON socket (`QOS_TEST_CONTROL`) for remote driving. The `qos-control` CLI wraps it; the `scripts/headless-examples/` directory has Python examples:

```bash
./scripts/headless-examples/full-loop.py   # spawn → click → snapshot → tear down, ~5s
```

Full reference: [`docs/headless-testing.md`](docs/headless-testing.md).

## Where to read first

- [`docs/architecture.md`](docs/architecture.md) — component overview
- [`docs/testing.md`](docs/testing.md) — VM + headless test workflow
- [`plan/architecture.md`](plan/architecture.md), [`plan/compositor.md`](plan/compositor.md), [`plan/collaboration.md`](plan/collaboration.md), [`plan/security.md`](plan/security.md) — design intent
- [`DEVELOPMENT.md`](DEVELOPMENT.md) — build setup
- [`CLAUDE.md`](CLAUDE.md) — conventions used when AI agents work in this repo

## Status

Single-monitor single-user is feature-complete: strip tiling, drag-to-detach tabs, undo/redo everywhere, collaborative cursors, peer-shared tabs (H.264), per-workspace sandbox with portal-mediated grants, FUSE-projected workspaces, headless test infrastructure with the same output pixels as the live binary.

Deferred for daily-driver: multi-monitor, AT-SPI tree (accessibility), audio routing, notifications, session-lock + power management, full DRM hardware paint on bare metal.
