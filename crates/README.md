# Workspace crate map

The root `Cargo.toml` lists every workspace member. The groups follow the code
layout in `docs/architecture/core.md`, and `tools/policy/check_crate_layout.py`
keeps what the core, platform, services and UI groups link. Package names and
binaries stay stable when directories move. `apps/README.md` describes the
applications; `core/fs/` owns the collaborative filesystem service, and
`ui/ui-core/` implements the toolkit behind `sdk/tilo-app`.

| Directory | Role |
|---|---|
| `core/` | The foundation and the device daemon: wire types, crypto, identity and keys, the resource store, actions, the filesystem, packages and the runtime. Nothing per app, no importers, no product features |
| `platform/` | One API per operating-system capability on Linux, macOS and the web; the only place app-reachable code may use target `cfg` |
| `ui/` | The UI library, which knows nothing about Tilo: ui-core, its paint and backends, rich text, formats, themes and the render protocol |
| `sdk/` | The one app SDK, for tilolang and Rust apps: `tilo-app`, the document model, the view and the app window |
| `lang/` | tilolang: its compiler, runtime and app process |
| `services/` | Device services that run on any device, which the daemon mounts as providers |
| `engines/` | PDF, media, document codecs, search and the other engines apps share, and the engine libraries the runtime loads |
| `os/` | The operating system: install and boot, disks, hardware, networking, rollback and system extensions |
| `vm/` | VMs and sandboxes: Linux VMs, ush environments and their images, tinker layers |
| `compat/` | Running Windows programs: the compatibility catalogue, its broker and the managed Windows VM |
| `integrations/` | Other services Tilo connects to: mail, calendars and DAV, cloud drives, messaging bridges and model providers |
| `desktop/` | The compositor, the session that is a client of the core, the portal and the seat-bound providers |
| `import/` | Reading another product's data (browser profiles, other computers' accounts), behind `tilo migrate` |
| `session/` | Crates on their way out: the Rust shell, which its replacement deletes |

## Developer tools and fixtures

- `../tools/nested-session-bus`: the build environment's dbus-daemon in a cargo-built executable, the
  private session bus of a nested session on hosts without one.
- `../tools/click-test-client`: the minimal Wayland client the shell's sandbox and desktop tests drive.
- `../tools/collab-probe`: sandboxed collaboration test app, driven by
  `scripts/headless-examples/tilolang-e2e.py`.
- `../tools/tilo-app-template-smoke`: compiles generated app code to validate the SDK templates.
- `../tools/copy-lint`: the source scan that keeps English copy literals out of UI builders,
  run from the shell's and desktop's tests with their own allow-lists.
- `../examples/packaging/hello` and `../examples/packaging/notes-cli`: executable
  packaging/SDK examples used by walkthroughs and integration scripts.
- `core/launcher/tests/fixtures/hello`: independent launcher test fixture.

Zero incoming Cargo dependencies do not imply that a crate is unused: binaries
can be invoked by system services, packaging, scripts, or developers. Check those
consumers before removing a workspace member.

## Build boundaries

The root `default-members` is intentionally smaller than the full workspace.
`runtime-dylib` is opt-in under `--profile thin-app`. The WASM document codec,
`fuzz/`, and independent fixture workspaces retain their own build boundaries
and lockfiles. Workspace members use the root `Cargo.lock`.
