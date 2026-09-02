# qOS

A collaborative Linux desktop, shipped as an image-based OS.

The premise is that collaborating with people and collaborating with AI agents is one
problem. Both need permissions you can reason about, visibility into who did what, and the
ability to undo it. None of that can live inside applications — if it only has to work in the
document editor it is a feature of the document editor, but it also has to work in the ebook
reader, the file manager and the terminal. So it lives underneath them.

**Applications own neither their pixels nor their storage.** An app declares its interface
through the toolkit, which the desktop draws, and exports **resources** — hierarchical and
typed, like an ebook and the highlights and annotations inside it — each carrying its own
**actions**. The state behind a resource is a CRDT the system owns.

Most of the rest follows from that:

- **Accessibility and automation are the same door.** A screen-reader user, a script and an
  AI agent all browse resources and invoke actions rather than seeing and clicking. An agent
  is an ordinary collaborator: it gets a cursor colour, a presence indicator, and attributed
  history you can review and revert.
- **Search reaches everything.** Apps register a search provider and a renderer per resource
  type, so you can search across authors, titles, an ebook's full text and your own
  annotations, and land in the app at that position.
- **Undo, history, device sync and theming are system services**, not forty implementations.
- **Windows do not overlap.** Horizontally scrolling strips, Niri-style, inside workspaces
  that carry their own collaborators, permissions and host.
- **Apps are ~2 MB and are not installed.** Search, press enter, and it downloads and opens;
  unused copies are evicted; keeping one is an explicit opt-in. Sandboxing — Landlock,
  Wayland, portals — is what makes that safe.
- **The filesystem is collaborative and versioned.** Apps register (de)serialisation for
  formats like `.docx`, so the FUSE mount understands both native resources and legacy files;
  history, diff and undo look the same in the file manager as inside the app. Metadata syncs
  for browsing, content arrives from whichever peer has it when you open a file.
- **Rendering and state are decoupled**, so a workspace can be hosted on a NAS or a rented
  machine and still render locally at full speed rather than feeling like a remote session.

## The stack

| Path | What |
|---|---|
| `shell/` | Wayland compositor: scrollable strips, wheel-disc workspace switcher, per-workspace bubblewrap sandbox. `smithay` + `vello` + `wgpu`, collaboration over Iroh and `automerge`. Also `qos-control` (drives a running desktop) and `shots` (offscreen renderer). |
| `ui/` | Retained-mode CRDT-native toolkit. Every widget reads and writes one automerge document, so undo, persistence and collaboration share a mechanism. `wgpu` + `vello` + `taffy` + `cosmic-text` + `winit` + `fluent`. |
| `fs/` | FUSE3 driver. Iroh content addressing, `blake3`, `moka` cache. Mounted at `/var/workspace`; projects metadata immediately, hydrates content on first read. |
| `apps/` | 46 native applications — office suite, WebKit-based browser, mail, calendar, maps, video editor, terminal, files. See [`apps/README.md`](apps/README.md). |
| `crates/` | Supporting libraries by domain: collab, doc, runtime, packaging, session, system, text, format. |
| `os/` | Bootc-based Fedora image. greetd → Wayland session → `shell`. |

Single Cargo workspace, no submodules.

## Building

Needs the pinned toolchain (rustup reads `rust-toolchain.toml` automatically) and a set of
native libraries. `tools/check-build-env.sh` names any that are missing — Rust otherwise
reports an absent one as `pkg-config exited with status code 1` inside a `-sys` crate without
saying which. [`DEVELOPMENT.md`](DEVELOPMENT.md) maps them to packages.

```bash
cargo build -p shell        # compositor, qos-control, shots
cargo build -p ui
cargo build -p fs
cargo build -p file-explorer
```

Full OS image — `./os/build.sh` produces a qcow2 (rootless podman; the disk-image step needs
sudo), and `./os/test-vm.sh` boots it under QEMU.

Host-session packages for Ubuntu/Debian, Fedora, Arch, and NixOS are documented
in [`docs/packaging.md`](docs/packaging.md). CI builds and install-tests both
x86-64/amd64 and AArch64/arm64 before publishing a tagged release.

## Testing

Scope tests to what you are changing; the compile dominates.

```bash
cargo test -p shell --lib room_activity
cargo test -p shell --test interaction_test   # headless Wayland state, no GPU
```

Every app renders offscreen to a PNG with no display attached, and a running desktop can be
driven over a socket — semantically through its exported actions, or with synthetic input
when the thing under test is the pointer itself:

```bash
cargo run --bin shots -- --mode offscreen --out target/shots
cargo run -q --bin qos-control -- actions list
./scripts/headless-examples/full-loop.py     # spawn → drive → snapshot → tear down, ~5 s
```

Full reference: [`docs/headless-testing.md`](docs/headless-testing.md).
The required application-facing resource/action contract is documented in
[`apps/README.md`](apps/README.md#application-contract-required).

## Where to read first

- [`docs/architecture.md`](docs/architecture.md) — component overview
- [`plan/`](plan/) — design intent for the compositor, collaboration, security, distribution
- [`DEVELOPMENT.md`](DEVELOPMENT.md) — build environment and running qOS on your own machine
- [`CLAUDE.md`](CLAUDE.md) — what agents working in this repository are told

## Status

Single-monitor, single-user is feature-complete: strip tiling, drag-to-detach tabs, undo
everywhere, collaborative cursors, peer-shared tabs over H.264, per-workspace sandboxing with
portal-mediated grants, FUSE-projected workspaces, and headless test infrastructure that
produces the same pixels as the live binary.

Collaboration between different people works today. Syncing your own devices to each other is
the part that is not finished. Also outstanding for daily-driver use: multi-monitor, the
AT-SPI accessibility tree, audio routing, notifications, session lock and power management.

## License

AGPL-3.0. See [`LICENSE`](LICENSE).
