# Workspace crate map

The root `Cargo.toml` lists every workspace member, grouped by domain. Package
names and binaries stay stable when directories move. `apps/README.md` describes
the desktop applications; `session/shell/` contains the desktop surface, while
`sdk/ui-core/` implements the application toolkit behind `sdk/tilo-app`,
while `system/fs/` owns the collaborative filesystem service.

| Directory | Role |
|---|---|
| `agent/` | Provider configuration, model gateway, and local-model services |
| `collab/` | Production collaboration protocols, actions, identity primitives, and reference state |
| `dev/` | Developer tools, UI schema/graph support, generated-code checks, and collaboration probes |
| `doc/` | Native document model, format-provider interfaces, import/export adapters, and the separately built WASM codec |
| `hosting/` | Relay and home-peer services |
| `media/` | Shared media primitives, OCR, thumbnail generation, and vision libraries |
| `packaging/` | Package format, registry, installation, sandboxed launch, and app hosting |
| `platform/` | Per-capability platform crates with one API on Linux, macOS and the web; the only place app-reachable code may use target `cfg` |
| `runtime/` | App-facing service clients, CLI helpers, and optional dynamic SDK |
| `sdk/` | Public application SDK (`tilo-app`) and its private UI implementation (`ui-core`) |
| `session/` | Desktop shell, greeter, installer, unlock helper, and power entrypoints |
| `system/` | OS integration, policy, settings, device services, and system tools |
| `text/` | Shared text services such as spellchecking |
| `windows/` | Windows compatibility catalogue, services, and managed VM integration |

## Developer tools and fixtures

- `dev/collab-link`: standalone latency/throughput proxy for collaboration sockets.
- `dev/frame-link`: encoded-frame network simulator and `flatpak-share-demo`.
- `dev/collab-probe`: sandboxed collaboration test app, driven by
  `scripts/sandboxed-app-collab.py`.
- `dev/tilo-app-template-smoke`: compiles generated app code to validate the SDK templates.
- `dev/copy-lint`: the source scan that keeps English copy literals out of UI builders,
  run from the shell's and desktop's tests with their own allow-lists.
- `collab/collab-fixtures`: shared collaboration test fixtures.
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
