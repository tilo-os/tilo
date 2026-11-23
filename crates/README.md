# Workspace crate map

The root `Cargo.toml` lists every workspace member, grouped by domain. Package
names and binaries stay stable when directories move. `apps/README.md` describes
the desktop applications; `session/session/` contains the session's desktop,
`session/compositor/` its display server and `session/desktop-surface/` the
surfaces it shows, while `sdk/ui-core/` implements the application toolkit
behind `sdk/tilo-app`, and `system/fs/` owns the collaborative filesystem
service.

| Directory | Role |
|---|---|
| `agent/` | Provider configuration, model gateway, and local-model services |
| `collab/` | Production collaboration protocols, actions, identity primitives, and reference state |
| `compat/` | Running Windows programs: the compatibility catalogue, its broker and the managed Windows VM |
| `dev/` | The dev-image daemon and its device-admin route; developer tools live in `../tools/` |
| `doc/` | Native document model, format-provider interfaces and import/export adapters |
| `hosting/` | Relay and home-peer services |
| `import/` | Reading another product's data (browser profiles, other computers' accounts), behind `tilo migrate` |
| `media/` | Shared media primitives, OCR, thumbnail generation, and vision libraries |
| `packaging/` | Package format, registry, installation, sandboxed launch, and app hosting |
| `platform/` | Per-capability platform crates with one API on Linux, macOS and the web; the only place app-reachable code may use target `cfg` |
| `runtime/` | App-facing service clients, CLI helpers, and optional dynamic SDK |
| `sdk/` | Public application SDK (`tilo-app`) and its private UI implementation (`ui-core`) |
| `session/` | Desktop shell, greeter, installer, unlock helper, and power entrypoints |
| `system/` | OS integration, policy, settings, device services, and system tools |
| `text/` | Shared text services such as spellchecking |
| `vm/` | VMs and sandboxes: Linux VMs, ush environments and their images, tinker layers |

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
- `packaging/launcher/tests/fixtures/hello`: independent launcher test fixture.

Zero incoming Cargo dependencies do not imply that a crate is unused: binaries
can be invoked by system services, packaging, scripts, or developers. Check those
consumers before removing a workspace member.

## Build boundaries

The root `default-members` is intentionally smaller than the full workspace.
`runtime-dylib` is opt-in under `--profile thin-app`. The WASM document codec,
`fuzz/`, and independent fixture workspaces retain their own build boundaries
and lockfiles. Workspace members use the root `Cargo.lock`.
