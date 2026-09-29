# Hosts

The toolkit, the apps and the desktop model do not assume they run inside the
native Tilo session. Everything a process needs from its environment arrives
through a small set of interfaces, each with a native implementation in this
repository. Another host — a headless peer on a server, a test harness, or a
runtime on a different platform — supplies its own implementations of the same
interfaces instead of patching call sites.

Native-only code is selected with default-on Cargo features, never with
target-specific `cfg`. A host that cannot use the native implementations builds
`tilo-ui` with `default-features = false` and installs its own.

## Interfaces

| Concern | Interface | Native implementation | Other implementations |
|---|---|---|---|
| Time | `ui::time::{now, wall}` | `std::time` (via `web-time`) | — |
| Background work and timers | `ui::background::{Executor, Background}` | `ThreadPool` | `Pumped` (host-driven, deterministic), `NoBackground` |
| Platform services | `ui::host::HostServices` | `ui::native::services` | `HostServices::null()`, `HostServices::recording()` |
| Process inputs | `ui::host::HostEnvironment` | `ui::native::process_environment()` | built by the host |
| Document storage | `durable_store::DurableStore` | `app_state::FsStore` | `MemoryStore::new()`, `MemoryStore::queued()` |
| Desktop settings | `settings_store::SettingsSource` (`install_source`, `load`, `stamp`) | `settings.json` (nothing installed) | `settings_store::HostDocument` (the browser page) |
| Local person's identity | `ui::local_identity::LocalIdentitySource` | `RuntimeIdentity` (feature `local-daemons`) | `NoLocalIdentity` |
| Collaboration transport | `ui::collaboration::CollabTransport` | `BrokerTransport` (feature `broker-transport`) | `LoopbackTransport` |
| Action invocation | `tilo_actions::remote::ActionHost` | the shell's broker host | any host serving the same vocabulary |
| Peer wire format | `neighbour-wire` | — | — |
| Desktop model | `tilo-desktop-model` | wrapped by the shell's `WorkspaceTree` | — |

### Time and background work

Portable code reads the clock only through `ui::time` and starts work only
through `Background::current()`. Clippy's `disallowed-methods` in
`crates/sdk/ui-core/clippy.toml` rejects `Instant::now`, `SystemTime::now`,
`thread::spawn` and `thread::sleep` elsewhere; native runner modules opt out
with a stated reason. An executor may refuse work with `Unsupported`; callers
report that as unavailable instead of blocking. Services that load data follow
a request/complete shape (`ImageStore::host_driven` with `HostImageLoader`) so a
host can perform the I/O itself.

### Platform services and process inputs

`HostServices` bundles clipboard, file picking and launching, URI opening,
speech, notifications, media session, location and screen capture. A runner
installs one set with `host::install_default`; a process hosting several apps
scopes a set per app with `host::scoped` or `host::enter`; code reads the
current set with `host::current()`. Apps keep calling the toolkit's ordinary
functions (`clipboard_copy`, `PickRequest::send`, `Link`), which resolve the
current host.

`HostEnvironment` carries identity (package, workspace, continuity), paths,
accessibility settings, locale and runner diagnostics. Only
`ui::native::process_environment()` reads environment variables, XDG
directories and settings files; `tools/policy/check_ui_host_inputs_contract.py`
keeps other `env::var` reads out of the toolkit.

### Storage

Every app document is stored through one path: a `DurableStore` holding keyed
values (records, package documents, room indexes, views) and a `DocOwner` /
`DocWriter` from `app-state` that knows about Automerge heads, appends and
compaction. A write is saved only when the store reports it durable, so a
backend that commits asynchronously — `MemoryStore::queued()` drained by its
host — gives the same save status as the filesystem. `FsStore` maps keys onto
the long-standing on-disk layout and keeps its single-writer file locks.
`RecordOwners`, `InstanceViewStore` and `ApplicationHost` accept a store with
`with_store`.

### Collaboration and actions

`CollaborationManager::with_transport` accepts any `CollabTransport`. The
transport authenticates, signs and verifies; the manager holds no keys.

Browsing and invoking actions — from another device, the relay, apps, the
agent, the MCP bridge or the palette — uses the versioned vocabulary in
`tilo_actions::remote` and an `ActionHost`. The caller identity comes from the
transport (`Caller`), never from the payload, and retries with the same
invocation id reach the app once. The relay routes remote-action frames but
never executes them; a host that serves apps plugs an `AppServingPlane` into
it.

### Desktop model

`tilo-desktop-model` holds workspaces, strips, window groups and tabs, their
topology edits, the room projection over a host-owned Automerge document and
tile-to-app resolution. It depends on no rendering, windowing, compositor or
async runtime crate; `tools/policy/check_desktop_model_dependencies.py`
enforces that. Document ownership, admission and authority stay with the host.

## Guarantees in CI

- `cargo check -p tilo-ui --no-default-features --lib` must stay warning-free.
- `neighbour-wire` pins its encoding with golden postcard fixtures.
- The dependency and host-input contracts above run in the policy suite.

## Targets without sockets

`cargo check -p tilo-ui --no-default-features --lib --target
wasm32-unknown-unknown` compiles warning-free. The portable build takes tokio
with tasks and channels only; `native` and `broker-transport` add its socket,
file and thread features. Platform capabilities that a far dependent could
otherwise unify back on — tokio's wasm-refused features, JavaScript entropy,
wgpu's single-threaded `Send` — are selected per target in the crates that
need them, next to their existing `cfg(unix)`/`cfg(windows)` arms.

Compiling is not running: threads and files are still reached at run time.
The local daemon clients — `runtime-portal`, `runtime-identity`, the broker
transport and `settings.json` — sit behind the `local-daemons` feature, which
`native` includes; without it the host installs a `SettingsSource` and a
`LocalIdentitySource` and passes a `CollabTransport`. `app-state` still links
`FsStore` and its file-lock helpers. See
[`browser-runtime.md`](browser-runtime.md).
