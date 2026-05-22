# QuickDesktop (nirios)

A collaborative, spatial Linux OS built on Fedora Bootc. Not a single app — a vertically-integrated stack:

- Immutable container-based OS image
- Custom Wayland compositor with a 2D-cluster tile-first window model (no overlapping windows)
- P2P FUSE filesystem (Iroh + blake3 + moka cache; mounted at `/var/workspace`)
- CRDT-native UI toolkit (`quick-ui`) — every widget reads/writes a shared automerge doc, so undo, redo, collaboration, and persistence all share one mechanism
- Per-workspace bubblewrap sandbox with seccomp + capability grants + a portal D-Bus backend
- Native system apps (terminal, file explorer, browser)

## Key features

- **Spatial desktop**: clusters tile on an infinite 2D canvas; tabs tile within a cluster; sidebar wheel disc rotates workspaces; tabs can be dragged into new clusters or other workspaces.
- **Collaborative by default**: workspaces sync over libp2p (gossipsub + mDNS) + automerge incremental sync. Peer cursors render live; tabs can be shared via H.264 streaming with read-only or read-write input replay.
- **Per-workspace sandbox**: every cluster is a bwrap profile with seccomp deny-by-default for FS/network/camera/mic. Grants are stored in the workspace CRDT (so they sync), surfaced via an `xdg-desktop-portal` backend, manageable via the in-shell Permissions tab.
- **Headless-testable**: `--backend headless` runs the full compositor without a display; the `NIRIOS_TEST_CONTROL` JSON socket + the `nirios-control` CLI drive any UI action; `shots --mode offscreen|recording` produces PNG/H.264 of every catalogued scenario. See [`docs/headless-testing.md`](docs/headless-testing.md).

## Getting started

```bash
git clone <url> quickdesktop && cd quickdesktop
git submodule update --init --recursive
cargo build -p quick-desktop --bin quick-desktop --bin nirios-control

# Run the compositor (host window — needs a Wayland or X11 session):
cargo run -p quick-desktop

# OR headless (writes PNGs via the test_control socket, no display needed):
./scripts/headless-examples/full-loop.py
```

Full setup: [`DEVELOPMENT.md`](DEVELOPMENT.md).

## Documentation

| Topic | Path |
|---|---|
| Architecture overview | [`plan/architecture.md`](plan/architecture.md) |
| Compositor design | [`plan/compositor.md`](plan/compositor.md) |
| Collaboration semantics | [`plan/collaboration.md`](plan/collaboration.md) |
| Security model + sandbox | [`plan/security.md`](plan/security.md) |
| Headless testing | [`docs/headless-testing.md`](docs/headless-testing.md) |
| OS image build + VM testing | [`docs/testing.md`](docs/testing.md) |
| Plans-in-flight + landed | [`docs/superpowers/plans/`](docs/superpowers/plans/) |
| Specs + design RFCs | [`docs/superpowers/specs/`](docs/superpowers/specs/) |
| Working with AI agents (Claude / Codex / Gemini) | [`CLAUDE.md`](CLAUDE.md) |

## License

(Include license information here)
