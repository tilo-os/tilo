# The `runtime` SDK

Every Tilo app depends on the `runtime` umbrella crate. It re-exports five
submodules and one error type; everything else in the SDK is layered on top.

```rust
use runtime::{collab, fs, identity, portal, portal_xdg};
use runtime::{NotAvailable, is_tilo_native};
```

**Design intent.** Every service reachable through this SDK is optional.
Apps built against `runtime` MUST run as Flatpaks on stock Linux with no
Tilo services present — every `try_connect` / `load` returns
`Result<T, NotAvailable>` and never panics, so the app falls back to a
local-only mode. `is_tilo_native()` is the cheap probe you may use to gate
first-run welcome UI, but you must NOT use it as an authorization gate —
`try_connect` is the source of truth.

## The umbrella (`runtime`)

Re-exports:

| Path                   | What it is                                  |
| ---------------------- | ------------------------------------------- |
| `runtime::collab`      | The collab broker client (`runtime_collab`).|
| `runtime::fs`          | The workspace filesystem client (`runtime_fs`). |
| `runtime::identity`    | Local Ed25519 signing (`runtime_identity`). |
| `runtime::portal`      | Service-fd + Cap'n Proto plumbing (`runtime_portal`). |
| `runtime::portal_xdg`  | XDG desktop portal client (`runtime_portal_xdg`). |
| `runtime::NotAvailable`| The single error type every fallible SDK entry returns. |
| `runtime::is_tilo_native` | `bool` — heuristic ("is `TILO_WORKSPACE_ID` set?"). |

## `runtime::portal` — service discovery and RPC plumbing

Low-level. Most apps don't touch it directly.

```rust
pub enum NotAvailable {
    ServiceFdMissing,                                // launcher didn't pass an fd
    ConnectionFailed(std::io::Error),                // fd was there, socket dead
    ProtocolMismatch { expected: u32, got: u32 },    // handshake version mismatch
    Capnp(capnp::Error),                             // RPC-layer error
}

pub fn is_tilo_native() -> bool;
pub fn service_fd(name: &str) -> Option<RawFd>;
pub struct UnixCapnpClient { … }
pub fn try_open_service<C: FromClientHook>(name: &str) -> Result<C, NotAvailable>;
```

`service_fd("collab")` reads `TILO_SVC_COLLAB_FD` from the environment and
verifies the fd is still open with `fcntl(F_GETFD)`. Names are
case-insensitive; call sites pass lowercase.

## `runtime::collab` — the collab broker

```rust
pub fn try_connect() -> Result<CollabClient, NotAvailable>;

impl CollabClient {
    pub async fn join_channel(&self, /* … */) -> Result<ChannelHandle, NotAvailable>;
}
impl ChannelHandle {
    pub async fn send(&self, op: &[u8]) -> Result<(), NotAvailable>;
    pub async fn send_unsigned_snapshot(&self, data: &[u8]) -> Result<(), NotAvailable>;
    pub async fn send_unsigned_incremental(&self, data: &[u8]) -> Result<(), NotAvailable>;
    pub async fn subscribe(&self) -> Result<OpStream, NotAvailable>;
    pub async fn presence(&self) -> Result<PresenceHandle, NotAvailable>;
    pub async fn subscribe_context(&self) -> Result<ContextStream, NotAvailable>;
    pub async fn fetch_scrollback(&self, /* … */) -> Result<ScrollbackBatch, NotAvailable>;
}
impl PresenceHandle {
    pub async fn publish_cursor(&self, x: f32, y: f32) -> Result<(), NotAvailable>;
    pub async fn publish_caret(&self, key_bytes: &[u8], pos: u64) -> Result<(), NotAvailable>;
    pub async fn publish_selection(&self, /* … */) -> Result<(), NotAvailable>;
    pub async fn publish_arrived(&self, key_bytes: &[u8]) -> Result<(), NotAvailable>;
    pub async fn publish_left(&self, key_bytes: &[u8]) -> Result<(), NotAvailable>;
    pub async fn subscribe_remote_cursors(&self) -> Result<CursorStream, NotAvailable>;
    pub async fn subscribe_all(&self) -> Result<PresenceStream, NotAvailable>;
}
```

The broker is speaking Cap'n Proto over the connected fd the launcher
passed in as `TILO_SVC_COLLAB_FD`. `CollabClient::from_raw` exists for tests
that own their own RPC system.

`send_unsigned_snapshot` / `send_unsigned_incremental` are the raw CRDT
transport channels; `send` is the higher-level op protocol.

## `runtime::fs` — the workspace filesystem client

```rust
pub fn try_connect() -> Result<FsClient, NotAvailable>;

pub enum OpenMode { … }
pub enum EntryKind { … }
pub struct Metadata { … }
pub struct DirEntry { … }

impl FsClient {
    pub async fn open(&self, path: &str, mode: OpenMode) -> Result<FileHandle, NotAvailable>;
    pub async fn list(&self, path: &str) -> Result<Vec<DirEntry>, NotAvailable>;
    pub async fn stat(&self, path: &str) -> Result<Metadata, NotAvailable>;
    pub async fn mkdir(&self, path: &str) -> Result<(), NotAvailable>;
    pub async fn remove(&self, path: &str) -> Result<(), NotAvailable>;
}
impl FileHandle {
    pub async fn read_at(&self, offset: u64, max_len: u32) -> Result<Vec<u8>, NotAvailable>;
    pub async fn read_all(&self) -> Result<Vec<u8>, NotAvailable>;
    pub async fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<u32, NotAvailable>;
    pub async fn flush(&self) -> Result<(), NotAvailable>;
    pub async fn metadata(&self) -> Result<Metadata, NotAvailable>;
}
```

The FUSE tree is mounted at `/var/workspace/` on a live Tilo. This client is
a Cap'n Proto proxy over `TILO_SVC_FS_FD` — the launcher opened a socket to
the FS daemon on your behalf. Prefer it over raw `std::fs` for anything in
the workspace: it participates in the P2P projection/hydration flow and
respects the workspace ACLs.

## App data: resources first, binary content second

Keep what your app owns as versioned resources: the document your widgets
bind to, changed only through actions, so its history replays, merges and
undoes. Files in `.docx`, `.odt` or `.md` are projections of such a resource
(`tilo-doc-codecs`), not where it is stored.

Some bytes are not a resource: a photograph you imported, an image placed in
a document, a mail attachment. Treat those as *binary inputs*. Store them in
a `tilo_content::ContentStore` and let the resource name them by content
hash. An action carries the hash, never the bytes.

```rust
let content = tilo_content::ContentStore::for_app(); // once, at startup; pass it down
let hash = content.put(&bytes)?;                      // or put_reader(..) to stream
dispatch(InsertImage { content: hash, .. });
let bytes = content.read(&hash);                      // verified against the hash
```

The store is a handle, not a global. `ContentStore::memory()` is for tests.
qdoc documents follow the same rule:

- Importing a file returns the images it embeds as `ContentInputs` for you to
  `put_inputs`.
- Exporting reads them back through a `ContentSource`.
- The document itself only carries descriptors.

Content is never garbage-collected by guesswork. Only a caller that holds
every live reference may `collect_unreferenced`.

## `runtime::identity` — local signing

```rust
pub struct SigningIdentity { … }

pub fn load() -> Result<SigningIdentity, NotAvailable>;
pub fn load_from_path(path: PathBuf) -> Result<SigningIdentity, NotAvailable>;
pub fn load_from_bytes(bytes: &[u8]) -> Result<SigningIdentity, NotAvailable>;

impl SigningIdentity {
    pub fn display_name(&self) -> &str;
    pub fn avatar_color(&self) -> [u8; 3];
    pub fn group_public_key(&self) -> [u8; 32];
    pub fn threshold(&self) -> u16;
    pub fn total_shares(&self) -> u16;
    pub fn share_index(&self) -> u16;
    pub fn is_single_device(&self) -> bool;
    pub fn sign(&self, message: &[u8]) -> Result<[u8; 64], NotAvailable>;
}
```

This is the app-level identity — an app-scoped capability for the user's
Ed25519 identity. `load()` consumes the launcher-provided broker descriptor
(`TILO_IDENTITY_BROKER_FD`); public metadata and signatures cross that
connection, but the key does not. `is_single_device()` distinguishes
single-device signing from threshold (multi-device) identities. The legacy
`TILO_USER_IDENTITY` file path remains only for unsandboxed development.

## `runtime::portal_xdg` — XDG desktop portals

The freedesktop portals via `ashpd`. Reachable only when the manifest's
`[sandbox.portals]` block declared a `"via-host"` policy and the launcher
plumbed the D-Bus session bus into the sandbox.

```rust
pub enum PortalError {
    NotAvailable,       // bus unreachable / not proxied
    Cancelled,          // user closed the dialog
    NoResponse,
    PortalNotFound,     // bus reachable but no frontend installed
    Other(String),
}

pub fn is_portal_available() -> bool;  // cheap probe: is the bus reachable?
```

Every portal has a sync form and an `_async` form. The sync one spins up a
current-thread tokio runtime just for that call; use the async one if you
already own a runtime.

```rust
impl ScreenCast {
    pub fn start() -> Result<Vec<ScreenCastStream>, PortalError>;
    pub async fn start_async() -> Result<Vec<ScreenCastStream>, PortalError>;
}
impl Notification {
    pub fn send(title: &str) -> Result<(), PortalError>;
    pub fn send_full(/* … */) -> Result<(), PortalError>;
    pub async fn send_full_async(/* … */) -> Result<(), PortalError>;
}
impl Secret {
    pub fn retrieve() -> Result<Vec<u8>, PortalError>;
    pub async fn retrieve_async() -> Result<Vec<u8>, PortalError>;
}
```

File choosing and opening URIs or files are not part of this module: use
`ui::portal` (`PickRequest`, `LaunchRequest`, `open_uri`/`send_open_uri`),
which validates every returned URI before it becomes a path and is the one
`OpenURI` client.

The graceful-degradation contract is uniform: `PortalError::NotAvailable`
means "no bus reachable; fall back". `PortalError::PortalNotFound` means
"bus reachable but no frontend registered for that interface" (rare;
usually indicates a stock GNOME / KDE setup didn't install
`xdg-desktop-portal-<flavour>`).

## Portal / service fds

The launcher plumbs sockets in via:

```
--file <host-fd>  /run/tilo/services/<name>.sock
--setenv          TILO_SVC_<NAME>_FD  <child-fd>
```

Your app reads the fd number out of the env and hands it to
`runtime::portal::UnixCapnpClient::from_connected_fd`. The higher-level
`runtime::collab` / `runtime::fs` wrappers already do this for you — you
only touch `service_fd` directly if you are speaking a custom protocol.

## The graceful-degradation contract, in code

The reference `tilo-hello` bin exercises every SDK entry. This is what
running it under `tilo-install run` outside a Tilo session (no services)
prints — every call returns cleanly:

```
$ tilo-install run so.tilo.hello --no-services
tilo-hello: a Tilo-native app, running.

== environment ==
  is_tilo_native      : false
  TILO_WORKSPACE_ID   : (unset)
  TILO identity broker: (unset)

== services ==
  collab   : unavailable (no fd — graceful)
  fs       : unavailable (no fd — graceful)
  identity : unavailable (no fd — graceful)

== fds ==
  collab   : absent
  fs       : absent
  spellcheck: absent

== sandbox probes ==
  read(        /proc/self/status): permission denied — blocked (bwrap or Landlock)
  read(              /etc/passwd): entity not found — blocked (bwrap or Landlock)
  stat(                     /tmp): OK
  stat(            /tilo-launcher): OK
  write(/tmp/tilo-hello-probe): OK
  write(/var/tilo-apps/probe): permission denied — blocked (bwrap or Landlock)

== xdg portal probes ==
  is_portal_available: false
  DBUS_SESSION_BUS_ADDRESS: unix:path=/run/user/1000/bus
  notifications : unavailable — graceful (no bus / launcher didn't plumb)
  secret        : unavailable — graceful (no bus / launcher didn't plumb)

tilo-hello: done.
```

The last block shows that the D-Bus bus address is set (bwrap inherited it),
but because the manifest did not declare any `[sandbox.portals]` entry,
the launcher did not bind the bus socket into the sandbox — so
`is_portal_available()` returns false and every portal call surfaces
`NotAvailable`, as an app on stock Linux without a bus would see.

## Depending on `runtime`

Path-dependency (workspace) form:

```toml
[dependencies]
runtime = { path = "crates/runtime/runtime" }
```

For an out-of-workspace crate publish (until we cut a crates.io release),
depend directly by pinned path relative to your worktree. The SDK carries
`ashpd`, `capnp-rpc`, `tokio` at pinned versions and nothing else.

Next: [`cli-apps.md`](./cli-apps.md) — the CLI-flavour of the same
package model.
