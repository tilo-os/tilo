# The core

Tilo is a small core and thin products on top of it. This page is the
specification of the core. For each piece it gives how the code works on
`main` today, every copy of the piece with its size, the one design that
survives, and what is deleted to get there.

The core is six pieces and nothing else:

1. **Devices.** A device has its own key, certified by a person's identity.
2. **Identity.** A person is one identity root. It certifies, revokes and
   configures that person's devices.
3. **Sync.** Resources and files replicate between the devices of a
   workspace, end to end encrypted.
4. **The app framework.** An app declares resources, actions and views, and
   each action is tied to the view and controls that show it.
5. **The workspace.** A workspace holds apps, resources, files and
   collaborators (people and agents), under the permissions its owners set.
6. **Actions everywhere.** An action is called the same way from a view, the
   CLI, MCP, the SDKs, an agent or an automation.

Everything else is a product built on these: the desktop on each platform,
the ARM devices, the server distro, the web version, the native assistants,
the always-on agent, agent sandboxes on tilo.sh, and the productivity apps. A
product adds what is its own. When it needs a second copy of a core piece,
the core piece is improved instead.

Sizes are lines of Rust, TypeScript, JavaScript, Python and tilolang in the
files git tracks, tests included, measured on `main` at `d81cc1750ff`
(2026-11-21). Main holds about 7.48M lines in all, including 807k of
`vendor/`.

## The rule for every piece

- One type, one store, one protocol for each concept. A second copy is a
  defect to delete, not a variant to keep.
- The surviving copy is the one closest to the model below, not the largest
  or the newest. The others are deleted in the same piece of work that moves
  their callers. Nothing is kept beside its replacement "for now".
- No work adds a new copy of a core concept while an old one is still on
  `main`. Work that would do so either deletes the old copy first or stops.
- No legacy readers. Tilo is pre-alpha, and a format that changes moves its
  data once, in one migration action, and then the old reader goes.
- No UX loss. A deletion that would lose something a person does in their
  first hour is blocked until the surviving path does it.

---

## 1. Devices

### Today

A device's key and certificate live in four places, and none of them reads
another:

| Store | Code | Location | Size |
|---|---|---|---|
| Native device key and certificate | `tilo-device-identity` `IdentityPaths` (`persist.rs`, `keychain.rs`) | `$TILO_IDENTITY_ROOT`, else `/var/lib/tilo`, else `$XDG_DATA_HOME/tilo`: `device-private/keypair.bin`, `device/cert.bin`, `device/certs/<user>.bin` | 6,922 |
| CLI and agent login key | `tilo-api` `client/device.rs` `Device` | `$TILO_DEVICE_DIR`, else `$XDG_STATE_HOME/tilo/device/{key,device.json}`. `tilo login` mints a fresh key here even when the computer already has a device key. | 201 |
| Browser device keys | `tilo-browser-runtime` `web/device-key.mjs` | IndexedDB, a pool of non-extractable keys per tab | 423 |
| Boot unlock key | `tilo-auth` `userkey.rs`, `record.rs`; `tilo-unlock-protocol` `BootKeyCert` | `/var/lib/tilo/auth/`, the root keyring | 445 + 1,818 |

There are three certificate formats: `tilo_user_identity::DeviceCert`
(postcard, `cert.rs`), `tilo_unlock_protocol::BootKeyCert`, and the
`tilo-auth` sibling `Token` (`sibling/`, 2,085) that wraps the second. MLS
credentials in `key-group` are a separate layer and stay.

There are four device lists: `CertifiedFleet` (`tilo-user-identity`
`fleet.rs`, a verifier), `DeviceFleetStore` (`tilo-identity` `device_fleet/`,
on disk at `<identity_dir>/fleets/<id>.json`), the `fleet.json` and
`registry.json` that `tilo-enrollment` `install.rs` writes itself, and the
per-workspace `WorkspaceDirectory` (`tilo-user-identity` `directory.rs`).
Revocation has a defect: `tilo-server` `admin.rs` `persist_fleet_revocation`
defaults to `$XDG_CONFIG_HOME/tilo/identities`, while every other reader
uses `tilo/identity`. It finds no fleet there and returns success, so by
default a revocation never reaches the fleet.

There are seven ways to add a device:

1. `tilo-enrollment`: `join_with_token`, `pair_device`, approval and
   transport (5,718).
2. `tilo-identity` `certification_ceremony` with `pairing_code`,
   `enrollment/` and `enrolled_transport` (about 5,300).
3. The `tilo-enroll` and `tilo-approve` binaries in `tilo-server` (1,741).
4. `tilo login` against the server's login codes (`tilo-cli` `login.rs`
   2,046, `tilo-server` `web/login_codes.rs` and `web/sign_in.rs` 1,500).
5. The browser's `web/pairing.mjs` (348).
6. The boot-key enrolment in `tilo-auth` `sibling/enrol.rs` (1,121).
7. `tilo-device-admin` `device.unlock.enrol` (2,211).

### What survives

- **One key per device, in one store.** `IdentityPaths` is the only place a
  native device keeps its key and certificate. The browser keeps its key in
  IndexedDB because it has no file system, but it holds the same `DeviceCert`
  bytes. The CLI login keeps only `site`, `workspace` and `server_cert` as a
  `login.json` beside `device/`, and signs with the device key that is
  already there.
- **One certificate format.** `DeviceCert`, with capabilities. A boot-only
  capability replaces `BootKeyCert`.
- **One device list.** `DeviceFleetStore` persists it, and `CertifiedFleet`
  verifies it. A workspace's device view is derived from the fleet, not
  stored a second time. Revocation is an entry in the fleet and reaches every
  device of the identity.
- **One way to add a device.** `tilo-enrollment`. A login code, a pairing
  code, a QR code and a boot-key request are only token sources feeding
  `join_with_token`. Adding a computer is one command, `tilo login`, and
  that computer is then a device of the person like any other.

### What goes

| Delete | Size |
|---|---|
| `tilo-api` `Device` key file and `TILO_DEVICE_DIR` | ~120 |
| `BootKeyCert` and the duplicate verification in `tilo-auth` `sibling/token.rs` | ~550 |
| `certification_ceremony` beyond `pairing_code` | ~600 |
| Enrolment logic in the `tilo-enroll` and `tilo-approve` binaries (thin CLIs over `tilo-enrollment`) | ~800 |
| `tilo-auth` `PeerRegistrar` enrolment (moves onto the enrolment transport) | ~400 |
| `tilo-device-admin` `install_cert` (duplicates `tilo-enroll install-cert`) | ~200 |
| Direct `fleet.json` and `registry.json` writes in `tilo-enrollment` `install.rs` | ~300 |

Owner: **One identity** session.

## 2. Identity

### Today

A person's identity root is kept in more than one place:

| Store | Code | Location | Size |
|---|---|---|---|
| Native person records | `tilo-identity` `store/` (`unix.rs`, `backup.rs`, `recovery_words.rs`, `secure_store.rs`) | `$XDG_CONFIG_HOME/tilo/identity/{<id>.json,active,secrets/}` | 4,986 |
| Record format | `tilo-identity-ceremony` `IdentityRecord`, `MemoryIdentityStore` | the same JSON | 315 |
| Onboarding writer | `onboarding-session` `identity_provisioning.rs` `DiskIdentityStore` | `$XDG_CONFIG_HOME/tilo/identity/primary.json` | 562 |
| Registry | `tilo-identity` `registry/` `IdentityRegistryStore` | `<identity_dir>/registry.json` | 1,061 |
| Browser root | `tilo-browser-runtime` `root_identity.rs`, `web/identity.mjs` `RootEnvelope`, wrapped by a passkey PRF | IndexedDB | 1,466 |
| Old CLI location | `tilo-cli` `identity.rs` `earlier()` | `$XDG_STATE_HOME/tilo/identity/user-identity/pubkey.bin` | ~20 |

`tilo-identity` as a whole is 22,760 lines, with `registry/`,
`identity_store`, `device_fleet/`, `certificates` and `local_person` layered
over each other. `DeviceIdentity` means two different things: public
metadata in `tilo-user-identity` `lib.rs`, and a certificate plus signer in
`tilo-identity` `identity_store.rs`. `resolve_active_identity` exists in both
`tilo-identity` and `tilo-identity-client`.

There are three recovery formats (the 24 words, the browser `RecoveryKey`,
and `tilo-auth`'s modhex on disk). There are five sign-in paths: the greeter
through `pam-tilo` to the `tilo-auth` daemon, `boot-signin`, `tilo-api`
`sign_in.rs` with `oauth.rs` and `SignInProof`, the browser `sign-in.mjs`,
and `tilo login`. `threshold-identity` (2,644) implements key generation
across devices, but outside tests only its one-of-one and word paths are
used.

### What survives

- **One identity root type**, `tilo-user-identity` (`RootEnvelope`,
  `DeviceCert`, `SignInProof`), shared by the OS, the CLI and the browser.
- **One person store**, `tilo_identity::store` with `IdentityRegistryStore`,
  holding `IdentityRecord`. The browser keeps its wrapped root in IndexedDB,
  in the same record format.
- **One recovery format**: the 24 words.
- **One sign-in proof**, `SignInProof`. The greeter, the web page, the API
  and `tilo login` all prove the same thing (a current device of this
  identity) and differ only in how the person approves it.
- The runtime's `DeviceIdentity` is renamed `DeviceSigner`.

### What goes

| Delete | Size |
|---|---|
| `tilo-identity-ceremony` (its `IdentityRecord` moves into `tilo_identity::store`) | 315 |
| Onboarding's `DiskIdentityStore` (calls `tilo_identity::provisioning::create_person`) | ~250 |
| The old `$XDG_STATE_HOME/tilo/identity` probe, after one migration | ~20 |
| The second `resolve_active_identity` | ~100 |
| The browser `RecoveryKey` and `tilo-auth` modhex recovery formats | ~400 |
| `threshold-identity` paths with no caller, until a product needs them | ~2,000 |
| Overlapping layers inside `tilo-identity` `store/` | to be measured as they fold |

Devices and identity together come to about 6–7k lines, and they leave one
store of each kind where there are three today. Owner: **One identity**
session.

---

## 3. Sync of resources and files

### Today

A resource is an Automerge document. Its changes are carried as
`neighbour_wire::NeighbourMsg` frames (`crates/collab/neighbour-wire`,
5,046), sealed under MLS content keys from `key-group` (9,993) and
`workspace-keys` (8,337).

An app's write goes from the app to the collaboration broker client
(`collaboration`, 56,611), over the `runtime-collab` wire (14,012) to the
workspace service's broker (`tilo-workspace-service` `apps/collab_app_broker`,
65,255), and into `tilo-peer-state`'s owner journal. From there it travels
one of two ways:

- over `tilo-server/1` as `Envelope::Delta` (`tilo-server-protocol`,
  `tilo-delivery-protocol` 1,946, `tilo-server-session` 4,819,
  `tilo-server-client` 3,982), admitted by the host in `peer/` (16,274) and
  kept by `persistence.rs` and `store/` (9,264);
- or over the direct device link `tilo-peer-state-direct/3`
  (`tilo-peer-state` `direct.rs`), kept by `tilo_peer_state::store::Store`.

`workspaces/link_follow.rs` copies between the two.

The copies:

| Concept | Copies |
|---|---|
| Frame and document stores | `tilo-peer-state` `Store` (one frame file per write); the host's retained store (`tilo-workspace-service` `store/`, `persistence.rs`, `compaction.rs`) and `tilo-server-store` (1,298); `app-state` `FsStore` (8,305) with `durable-store` (1,724); `collaboration` `portable_storage.rs` for the page; `fs` `kept_documents` |
| Compaction | `tilo-workspace-service` `compaction.rs` and `store/compaction.rs`; `tilo-peer-state` `sealed_compaction`; `tilo-workspace-service` `sealed_documents` and `repaired_documents`; `app-state` `DocWriter` |
| Document sync protocols | `tilo-server/1` deltas with co-edit and anti-entropy; `tilo-peer-state-direct/3`; `tilo-personal-store/1`; `tilo-server-sibling/1` |
| Resource index | `resource-store` (7,475, used only by the workspace service); `tilolang-tilo` `index/` on redb (10,614); `collaboration::resource::store`; `tilolang-tilo` `TiloStore` |
| Brokers | `collaboration` `broker*.rs` (13,813); `collab_app_broker` (65,255) |
| Settings | `settings-store` (19,419), two Automerge documents behind its own `settings.sock`; the host's `person_settings` |
| File tree | `fs` `tree_document.rs` (1,046, the fs daemon) and `tree_sync.rs` (2,027, the workspace service and the standalone runtime): the same tree written twice, with two `tree_put`s |
| File bytes and keys | `fs` `FileArea` with iroh-blobs and its own ALPNs (`tilo-fs/session-link/1`, `tilo-fs/file-keys/1`, `tilo/fs/certify/1`, `tilo-fs-workspace-authority/1`); `tilo-content` (4,004) with `content-device` (706); `runtime-fs` blobs |
| File presentation | FUSE in `fs/src/fs/fuse.rs`; native Btrfs and fanotify in `fs-native` (10,340); the mirror folder `sync_folder`; `runtime-fs` (14,011) with its own FUSE in `runtime-fs-fuse` (1,771) |

Cloud mounts (`fs/src/cloud`, 6,815) talk to outside providers and are a
separate engine by design. Live media (`tilo-live`, MoQ) carries voice, video
and screen share only, never documents, and stays as it is.

### What survives

- **One replica engine**: `tilo-peer-state` `StatePeer` and its owner
  journal, on every device, the host included. A server is a device that
  keeps more frames.
- **One frame store**: `tilo_peer_state::store::Store`. A write is staged,
  signed, made durable and only then projected into the resource store, the
  same way for every carrier. The rows the resource store reads can always be
  rebuilt from the kept signed frames.
- **One wire and one link**: `NeighbourMsg` on one device link. Device to
  device and device to host are the same link, over iroh, and the page uses
  the same framing over WebTransport. `tilo-server/1` keeps only what comes
  before a session (join and address lookup).
- **One compaction**: `tilo-peer-state` `sealed_compaction`.
- **One resource index**: `resource-store`, with a persistent backend taken
  from the tilolang redb pager. Every host of resources (the workspace
  service, tilolang, the toolkit) reads it.
- **One broker**: `broker-core` with `collaboration` as its client. The
  workspace service's broker keeps only what is its own.
- **Files are resources.** The tree syncs as ordinary records through
  `tree_sync` only. Bytes go through one content store, moved by
  `RouteContent`, with keys from `workspace-keys`. Presentations (FUSE,
  native, mirror) are views of that one tree.
- **Settings are resources.** A person's settings are records of their
  workspace, and a device's settings are records of that device.

### What goes

| Delete | Size |
|---|---|
| `Envelope::Delta`, co-edit and anti-entropy (`tilo-server-session`, `tilo-server-client` co-edit, delta coding in `tilo-delivery-protocol`) | ~3,500 |
| The host retained store: `tilo-workspace-service` `store/`, `persistence.rs`, `compaction.rs`, and `tilo-server-store` | ~17,500 |
| `link_follow.rs` and the two-carrier path | ~1,500 |
| `tilo-personal-store/1` and `tilo-server-sibling/1` | ~1,500 |
| The tilolang redb index and the `TiloStore` mirror, keeping the pager | ~15,000 |
| The overlap in `collab_app_broker` | ~30,000 |
| `settings-store`'s document layer and socket, keeping the typed readers | ~8,000 |
| `fs` `tree_document.rs`, `tilo_fs::sync` and the fs ALPNs, the second `tree_put` | ~3,000 |
| `fs` `kept_documents`, `runtime-fs` blobs, `content-device` | ~3,000 |
| `app-state` `FsStore` once every document is a replica scope | ~10,000 |

About 90k lines. Owners: **One engine** session (replica, frame store,
link, compaction, broker, files); **tilolang onto the resource store**
session (the index and `TiloStore`); **one workspace** session (settings).

---

## 4. The app framework

### Today

There are two ways to write an app over one model, `tilo-resource`
(`#[derive(Model)]`, `#[resource]` with `#[action]` methods; 57,051 plus
7,556 of macros):

- **Rust.** `tilo-app` (92,545, of which `framework/` is 61,817):
  `ResourceApp`, `framework::run`, `ViewCx`, `Dispatcher<R>`,
  `ResourceSurface`, `ui::main!`. Views are written imperatively against
  ui-core widgets. 64 apps under `apps/`, 1.28M lines.
- **tilolang.** One file declares types, actions and views, and a view names
  its actions directly (`apps/tl/counter/app.tl`). The compiler `tilolang`
  (54,152), the WebAssembly host `tilolang-runtime` (67,069), the bridge
  `tilolang-tilo` (96,643), and `tilo-app-window` (13,771), which turns view
  patches into ui-core widgets. 13 apps under `apps/tl`, 16,663 lines.

The copies:

| Concept | Copies |
|---|---|
| Action registries and dispatchers | `tilo_actions::ActionRegistry` (`actions` 47,473); `tilo-app` `Dispatcher<R>`; ui-core `bound_dispatch` and `LocalDispatch`; ui-core `headless::RootDispatch`; `tilolang_runtime::registry::Registry`; `tilolang-tilo` `DynAction` republishing into the first; `os-actions` `Dispatcher` (17,561); `session-actions` (3,651) with the hand-written `apps/tl/session/surface.toml`; `ush-shell` `commands::Dispatch` |
| Rust view models | `ViewCx` views; `tilo-view` `#[view]` with `view-macros` (1,682, used only by Chat) |
| Widget output | ui-core paints to a canvas, and 162 widget files also write HTML (`composition.rs` `HtmlCx`) |
| tilolang hosts | `apps/tilolang` (21,346, one process per installed app, toolkit linked in); `tilolang-process` (14,358, one process per workspace and author) |
| Rust model logic | each app's own code, and again as `apps/*-model` (34 crates) plus `apps/*-model-wasm` (31 crates) with `tilo-model-guest` |
| Launch paths | a Rust app binary through `app-launch` and the `launcher` sandbox; `apps/tilolang`; `tilolang-process`; in-process written views and `shell-surface`; `app-keeper` for non-Tilo programs; Rust models as WebAssembly |
| Permission checks | `action-policy` (12,437); tilolang's own `tl:host` commit and `SimpleAuthority`; the SDK's `PermissionContext`; Chat's `ChatPermissions` |
| Shell | `session/desktop` and `desktop-surface` in Rust (about 147k); `apps/tl/shell` (6,990) |
| App models compiled into hosts | `tilo-server` links Chat and Tasks; `tilo-agent-session` links most models natively; the WebAssembly `tilo-model-engine` is the third |
| Apps in both languages | Chat (68k Rust, 963 tilolang), Tasks, Notes and Word Processor, Mail and Messages |

### What survives

- **One declaration language for apps: tilolang.** An app is types, actions
  and views in `.tl`. Rust remains for the core and for engines an app uses
  as host capabilities (pixels for Imager, the browser engine, PDF, video,
  the terminal), never for app UI.
- **One model ABI**: `tilo-resource` (`DynResource`, merge kinds,
  `ActionCx`), with models running as WebAssembly in every host.
- **One action registry**: `tilo-actions` (the published tree,
  `ActionEnvelope` replay, `ActionProvider`), with `action-policy` deciding
  every call and `automation` on top. The OS and session verbs are
  providers in it, and their descriptions are generated.
- **One view path**: tilolang view patches, through
  `tilo-app-window::reconcile`, to ui-core widgets. ui-core is the one widget
  library. A widget's HTML for the web page comes from the same patches, not
  from a second per-widget path.
- **One app process**: `tilolang-process`, one per workspace and app, started
  through `app-launch` and the `launcher` sandbox. It serves every window,
  every page viewer and every windowless agent launch of that app, and
  stops when idle. `app-keeper` stays only for programs that are not Tilo
  apps.
- **One undo boundary and one executor in the toolkit**: `LocalDispatch`.
  Widgets change things only through bound actions.

### What goes

| Delete | Size |
|---|---|
| `tilo-view` and `view-macros` (Chat's views port) | ~1,700 |
| `apps/tilolang` (folded into `tilolang-process`) | ~21,000 |
| `apps/*-model`, `apps/*-model-wasm`, `tilo-model-guest`, as each app's types move into `.tl` | ~35,000 and 65 crates |
| Native model links in `tilo-server` and `tilo-agent-session` | to be measured |
| `tl:host` commit and `SimpleAuthority`, the SDK `PermissionContext`, `ChatPermissions` | to be measured |
| ui-core `bound_dispatch`, `RootDispatch` and the widget callbacks (`on_change`, `on_view`, `take_action`) | ~15,000 |
| `tilo-app` `framework/` (`ResourceApp`, `Dispatcher<R>`, `ViewCx`, `ResourceSurface`) after the Rust apps port | ~62,000 |
| The Rust apps, each replaced by its tilolang port in the same push | ~900k of 1.28M; the rest becomes engine crates |
| The Rust shell, replaced by `apps/tl/shell` | ~140,000 |

This is the largest piece by far. A port lands together with the deletion of
the Rust app, after a side-by-side walk of the first hour of use, and gaps
are fixed in the shared framework, not in the app. Owners: **tilolang onto
the resource store** session (the language, its store and the single host);
**cleanup** session (app host off the session, onto the server, one process
per workspace and app, the web page); **legacy and compatibility readers**
session (the toolkit's second action path); each app's area owner for its
port.

---

## 5. The workspace

### Today

`collab_types::WorkspaceId` (a UUID) names a workspace, but eight structs
called `Workspace` describe it (`tilo-server-protocol` `workspaces.rs`,
`tilo-api` `server.rs` and `v1/answers.rs`, `desktop-model` `workspace.rs`,
`session/client` `presence.rs`, `fs` `sync.rs`, `actions`
`remote/mcp_server.rs`, `workspace-keys` testing), and `ush` and `tilo-net`
have their own id types.

Membership is the signed governance log in `tilo-peer-state`
(`WorkspaceGovernance`, `members/`, `workspace_membership.rs`), with roles
and grants decided by `action-policy` `WorkspacePolicy`. Beside it are six
more membership records: `tilo-server-protocol` `WorkspaceRoles` and
`desktop-model` `grants.rs`, a second role vocabulary; the
`tilo-workspace-service` `workspace.rs` `Membership` device list
(`WorkspaceRuntime`, 2,004, used in 23 files); `workspace-join`
`MemberList` and `MembershipProof` (3,173); `tilo-api` `store.rs` `Grant`
with `collab_types::WorkspaceGrant`, the API server's own grant file;
`apps/workspace_memberships`, a path for agents; and `PersonalStore`, which
is not governed at all.

There are two governors: `tilo-peer-state` `owner/` `NativeWorkspaceOwner`
(19,843, "workspaces this device governs itself") and the workspace service
as governor (`workspaces/authority.rs`, `own.rs`, `own_device.rs`), in which
the server's own person administers every workspace it holds. "Governor"
still appears in 347 Rust files.

Per-person stores still on `main`:

| Store | Code | Size |
|---|---|---|
| Personal records `(app, personal)` | `tilo-peer-state` `state/personal/`, `tilo-workspace-service` `apps/personal.rs`, `session` `personal.rs` | ~6,000 with tests |
| The person's workspace list | `workspaces/personal.rs` `KeepPersonalList` and `tilo-user-identity` `directory.rs` | 2,226 |
| The person's roaming settings | `settings-store` person document | part of 19,419 |
| Online accounts as JSON | `accounts` `AccountStore` | 11,612 |
| Connections under the person's root | `tilo-workspace-service` `connections` | 5,661 |
| Model choice per person | `workspaces/person_model`, `person_models` | 6,071 |
| A person's servers | the home-peer registry file, and again as the Servers workspace (`workspaces/servers.rs`) | 4,060 |

`person_settings` and `person_vault` already keep their data in the
workspace vault, despite their names.

### What survives

- **One workspace type and one id**, `WorkspaceId`, in `collab-types`.
  Other crates use it and do not redefine it.
- **Membership is a resource.** The signed governance log in
  `tilo-peer-state` is the one membership record. Its operations are signed
  and checked against the membership as their author saw it, so every key
  holder reaches the same answer without a coordinator.
- **One authority**: `action-policy` `Role` and `Grant`, read from that log,
  for people and agents alike. Keys follow membership through `key-group`.
- **No governor.** No device administers a workspace by being the device
  that holds it, and the server's own person has no power over a workspace
  it does not belong to.
- **One set of resources per workspace.** Mail, calendar, vault, models,
  connections, agents and skills belong to the workspace. Privacy between
  members is a reader rule on a record, never a private store.
- **What is a person's alone** (their keys, devices, appearance, the list of
  their workspaces, their servers) lives in the person's own workspace,
  whose only member is them, like the Servers workspace does today. There is
  no second kind of store for it.

### What goes

| Delete | Size |
|---|---|
| The self-governing half of `NativeWorkspaceOwner` and the service's governor role | ~10,000 |
| `WorkspaceRoles.custom_roles`, `desktop-model` `grants.rs` | ~1,000 |
| `workspace.rs` `Membership`, `WorkspaceRuntime` and its relay path | ~15,000–25,000 with `peer/` |
| `tilo-api` grant file, `WorkspaceGrant`, `apps/workspace_memberships` | ~3,000 |
| `PersonalStore` and `tilo-personal-store/1` | ~6,000 |
| The second workspace list | ~2,000 |
| `accounts` `AccountStore` (the workspace vault holds accounts) | ~9,000 |
| The person document of `settings-store` | ~4,000 |
| The home-peer registry file (the Servers workspace holds a person's servers) | to be measured |
| The duplicate `Workspace` structs | to be measured |
| `fs` `workspace_authority` ACL and its ALPN | ~1,100 |

About 50–60k lines. Owners: **one workspace, no per-person stores** session
(per-person stores, accounts, settings, the workspace list); **One engine**
session (membership as a resource, no governor); **hosting** session (one
daemon, the person's servers).

### Hosting

A server is a device that stores more. Today the server side is
`tilo-server` (122,096: the daemon with `init`, `run`, `dev`, `workspace`,
`page-host` and `status`, its own iroh relay, the web entry, and agents and
bots in `workspace_agents` (13,311) and `workspace_bots` (15,464)), with the
`tilo-server-admin`, `tilo-enroll`, `tilo-approve` and
`tilo-home-peer-control` binaries beside it. `tilo-api` (21,745) is the MCP
and OAuth endpoint with its own grant and token store. A device without a
desktop is a third copy of the same job: `tilo-runtime` (17,096) with
`tilo-runtime-mcp`, and the CLI's held device (`tilo-cli` `mcp/held.rs` and
`own_device`). All three open the same `tilo-workspace-service` through
`own_device`, but each has its own wrapper around it.

What survives: **one daemon per device**, the workspace service opened
through `own_device`. On a desktop the session is that daemon. Elsewhere it
is one binary that the CLI, a server and tilo.sh all run, with one MCP socket
locally and `device-mcp` as its remote face. A server differs only in its
appetite (how much it stores and which work it takes), set per workspace.
The relay is a mode of that daemon. `tilo-api` reads grants from the
workspace, not from its own file. Agents and bots on a host are workspace
work in the service's `workspaces/actions/agents`, not a second
implementation in `tilo-server`.

What goes: the `tilo-runtime` standalone wrapper, the CLI holder, the
duplicate agent and bot hosting in `tilo-server` (~10–15k), and the
separate binaries once their verbs are actions of the daemon. Owner:
**hosting** session.

---

## 6. Actions everywhere

### Today

The registry is already shared. Every entry point lists and invokes actions
through `tilo_actions::remote::ActionProvider` (`crates/collab/actions`
`remote.rs`; `RemoteActionFrame`, `REMOTE_ACTIONS_VERSION = 2`), and about
30 types implement it: the session, the workspace service, the agent
session's `WorkspaceActions`, `SessionActions`, the standalone runtime,
settings panes. The static schema is `surface.toml` (77 files), written from
code by `tilo-app surface`. Devices carry calls to each other over
`tilo-actions/1`.

The entry points on top of it:

| Entry point | Code | Transport |
|---|---|---|
| MCP server | `tilo-mcp` `McpServer` (30,360): about twelve generic tools over the catalogue (`tilo_ls`, `tilo_do`, `tilo_view` and others) | stdio |
| Session MCP socket | `session` `mcp_server.rs` and `mcp_server/` (4,700) around `tilo-mcp` | `agent.sock` |
| Device MCP | `device-mcp` (1,336) | iroh, `tilo-mcp-server/1` |
| `tilo mcp` | `tilo-cli` `mcp.rs`, `mcp/` (3,258), with its held device | stdio, then iroh |
| tilo-control | server in `session` `test_control.rs` (about 700 lines for actions, re-encoded as "legacy rows"); client `control-cli` (5,163); `console-control` (1,176) with its own `wire.rs` and `catalog.rs` | unix socket, virtio-serial |
| `tilo do` without `-w` | `tilo-cli` `desktop.rs` | tilo-control, falling back to MCP |
| `tilo do -w`, `tilo run` | `tilo-cli` `hosted.rs` (2,746) | HTTPS `/v1`, or `tilo_api::direct` in process |
| REST `/v1` | `tilo-api` (21,745): `v1/operations.rs`, a generated OpenAPI, `answers.rs`. Each route is turned into an MCP `tools/call` (`session.rs`) and the answer is parsed back, and `src/session/{members,apps,hosts,mail_accounts,unattended,invocations,models,keys}.rs` (2,275) intercept some calls by tool name before any provider sees them | HTTPS, SSE; `api.sock` |
| Rust REST client | `tilo-api` `client.rs` (1,515), hand-written | HTTPS |
| TypeScript SDK | `sdk/typescript` (26,401): 18,871 of types generated by `tilo sdk gen`, about 3,500 of hand-written runtime (`models.ts`, `routes.ts` repeating `operations.rs`, `endpoint.ts`, `client.ts`) | HTTPS, or napi into `tilo_api::direct` |
| Python SDK | `sdk/python` (32,350): 25,299 generated, about 3,450 hand-written | the same |
| Agents | `agent-turn` (2,275) through MCP `tools/list`; `agent-session` serves the catalogue | in process |
| tilolang CLI | `tilolang-cli` `session.rs` | tilo-control and the REST client |

So there are three action wire dialects (`RemoteActionFrame`, MCP JSON, and
tilo-control's rows with two server copies), REST stacked on MCP, two
parameter schemas (`ActionParam`/`ParamType` and the surface field types,
plus `param_row`), three hand-written REST clients, and 26 modules in
`actions/src/remote/` (9,869) that hand-type a domain's provider name,
action ids, arguments and answers (`person_accounts`, `person_vault`,
`workspace_activity`, `person_telling` and others), which is what
`surface.toml` exists to declare. Two crates are not entry points at all:
`os-actions` is a hardware backend with no `ActionSpec`, and
`messaging-protocol` holds messaging codecs.

### What survives

- **One registry and one dispatch**: the `tilo-actions` published tree and
  `ActionProvider`, with the workspace service's broker as the dispatcher
  and the caller certified. Every action has a typed input, a typed result,
  and the view that shows it.
- **One wire**: `RemoteActionFrame` v2, framed the same way on iroh
  (`tilo-actions/1`), on the local sockets and on virtio-serial. MCP and
  `/v1` are projections of it: `/v1` calls the provider directly, never
  through MCP, for callers that cannot speak iroh.
- **The CLI, MCP and the SDKs are projections, not implementations.**
  `tilo do`, the MCP tools, and the TypeScript and Python SDKs list the
  actions the registry publishes and call them. Their types are generated
  from the published schemas. None of them hand-writes a per-action handler,
  schema or wrapper.
- Every call shows: an action an agent invokes attaches its view, so the
  person sees what was done and by whom.

### What goes

| Delete | Size |
|---|---|
| tilo-control's row dialect: the action part of `test_control.rs`, `console-control` `wire.rs` and `catalog.rs`, the row rendering in `control-cli` `front.rs` (tilo-control keeps only input and screenshots) | ~2,200 |
| The REST-to-MCP bridge in `tilo-api` `session.rs` and the intercepts in `src/session/` (REST calls the provider directly; members, apps, hosts, accounts and models become providers) | ~2,800 |
| The hand-typed vocabularies in `actions/src/remote/` (replaced by `surface.toml` and generated bindings) | ~7,000 net |
| The hand-written REST clients: Rust `client.rs`, the TypeScript and Python runtimes (generated from the OpenAPI, around a kept core of about 600 lines per language for calls, events and errors) | ~6,200 |

About 18k lines, with `tilo-cli` `sdk.rs` growing by about 1k to generate
the clients.

Owner: **actions everywhere** session.

---

## Sessions

| Piece | Session |
|---|---|
| Devices, identity | One identity: add a computer with one command |
| Sync: replica, frame store, link, compaction, broker, files; membership as a resource | One engine: documents, sync, app processes |
| Resource index, tilolang store, one tilolang host | tilolang onto the resource store |
| App host off the session, one process per workspace and app, the web page | cleanup: the core migrations |
| The toolkit's second action path, old readers | legacy and compatibility readers |
| Per-person stores, accounts, settings, workspace list | one workspace, no per-person stores |
| One daemon per device, servers, tilo.sh, the relay | hosting |
| One action registry, CLI, MCP, SDKs | actions everywhere |

Progress is measured as net lines removed on `origin/main` in each piece's
crates since `d81cc1750ff`.
