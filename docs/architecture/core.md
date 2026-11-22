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

## The design

### Resources

A resource is a thing a person or an agent can name, share, link to and
open: a note, a task, a mailbox, a calendar, the workspace's folder, its
member list. It has a resource type, which an app declares (a `Note` has a title and
a body), and a stable id. Resources nest: a task can hold tasks, a mailbox
holds messages, and the folder holds folders and files. There is no second
level of "items" inside a resource; its type is a property of a resource, not a
level of the tree. Every resource, at any depth, can be shared, granted,
linked and shown on its own.

A resource is the one unit of everything the system does for it: sync,
permissions, history and undo, search and views. Its fields are kept in a
mergeable structure (Automerge), so two people editing at once both keep
their edits. A very large resource is split into pieces on disk and is still
one resource to everyone.

### Storage

- A change is a signed fact. A device writes it for a member, signs it,
  seals it under the keys of the resource's readers and keeps it. Changes
  are only appended. The current state, and every past state, come from them.
- Every device runs the same engine. Devices differ only in how much they
  keep and which work they take. A server is a device that keeps
  more. A store holds sealed changes and forwards them, but cannot read them.
  A workspace held only by one person's computer is complete.
- Changes move over one link, device to device. Reaching a device through a
  server means reaching it through a device that is online.
- Reads and queries go through one index built from the kept changes. It can
  always be rebuilt from them.
- Files are resources. The folder is one tree, synced and merged like any
  other edit. A file's bytes are sealed, content-addressed blobs the tree
  points at, fetched from any device that has them when they are needed. The
  mount, the native folder and an app's file list are views of that one tree.

### Addresses

An address is the path to a resource, with an action after `#`:
`acme/notes/<note>#update`, `acme/tasks/<task>/<sub-task>#done`,
`acme/files/projects/plan.md#move`. Resource names are user text, so `#` is
the only separator of an action and a name can never be mistaken for one.
People and agents use names, which resolve to stable ids, so renaming
something keeps links and grants. One resolver in `tilo_actions::tree` walks
the published tree from the workspace root for every caller, and grants
use the same addresses.

### Permissions

- One rule. A grant gives a person, a role or an agent Allow, Ask or Deny on a
  pattern over addresses (`acme/notes/*#update`, `acme/tasks/<list>/**`,
  `acme/files/projects/**#read`), optionally limited to a resource type. The most
  specific grant wins, a person's own grant beats their role's, and no grant
  means no. Roles are named sets of grants.
- Membership and grants are a resource of the workspace, changed by signed
  actions. No device and no server governs a workspace: every device that
  can read it works out the same answer from the same signed list.
- The rule is enforced three times with the same function: when an action is
  called (one admission, `admit_invocation`; an agent acting for a person
  gets the narrower of the two), when a change arrives (every receiver drops
  a change whose author was not allowed to make it at the time), and through
  the keys (reading means holding the keys of the resource's readers;
  resources with the same readers share one key group, and removing a reader
  rekeys it). What a device already received cannot be taken back.
- A file action is an action at the file's address. A grant on a folder
  covers what is below it. The mount is one more caller and decides every
  open from the replicated member list; a path a person may not see does not
  exist for them.
- Privacy between members is a rule on a record: a member's drafts are
  readable by that member alone and sealed under their keys, in the same
  workspace. There is no per-person store.
- What an app's code may reach (paths, devices, network hosts) is a separate
  grant, given at install or through a picker. An app acting for a person
  does only what both may.

### Apps

An app is a tilolang package installed in a workspace. It declares resource
types, actions (verbs with a typed input, a typed result and a kind:
read, edit or privileged) and views (functions from a resource to UI whose
controls name fields and actions). Installing it publishes all three into the
workspace's tree. That tree is the only registry.

An app is never "running". Any device that holds the workspace's keys and
takes the work runs an action's code, sandboxed, when it is called; an agent
can complete a task at night with no window open anywhere. Opening an app
means opening a view of a resource, in a window, a chat or a web page.
Storage, sync, permissions, history, undo, search, notifications, presence,
windows and outside effects (sending a mail, calling an API, declared by the
action and carried out by the system under the workspace's grants) are the
system's. An app is its resource types, its actions' code and its views.

### Agents and automations

An agent is not a person. It is shown as itself: its name, the person it
acts for, its turns and what they cost. An automation (a shortcut, a rule, a
script someone set up) is shown as the rule that ran and who set it up.
Underneath, both are actors acting for a person, like a person acting for
themselves, and one call can run through several of them: an automation a
person set up, run by an agent. Every actor in that chain must allow the
call (`tilo_action_policy::Acting`, `admit_invocation`).

An agent or an automation acts with a subset of its person's permissions,
by default the largest one. Granting it less works the
way granting a person less does. It acts on undoable things without asking,
which is most of what happens in a workspace. It runs on the person's
devices, like everything else, and does whatever it or its person starts it
on.

Asking an agent is a resource of its own type: who asked, what, and on which
resource. A mention in a chat message, a note or a task, or an ask from the
canvas creates one, and the agent's reply is a resource linked to it. The
agent engine runs inside each device's one daemon and reads asks; it never
reads an app's own fields. Which eligible device takes a turn is decided in
one function, `tilo_workspace_service::work::takes_turn`: an eligible device
claims the ask with an ordinary change, the earliest claim wins, and a claim
that writes nothing for a minute may be taken over. Everything a turn writes
is a member write for the agent, signed by the device that runs it.

An agent is a reader in its own right. Its member key is in the reader set
of every record it is in: a direct conversation between Ada and her agent
is sealed for Ada and the agent, never for Ada alone. A device that runs the
agent, the tilo.sh server included, holds the agent's keys and so opens
exactly the records the agent reads, and no others. A device that cannot
open the record an ask is about is not eligible to take its turn.

An agent sandbox on tilo.sh is one workspace. Whether that workspace also
has a machine for terminal commands, Browser tabs, or Linux and Windows apps
is what the customer pays for.

There is one agent runtime: the Tilo agent. It works on a workspace's files
and Terminal through the same actions the CLI, MCP and SDKs call, signs in
to outside services from the workspace vault, and uses the model provider
chosen in the workspace's AI settings. Tilo has no code for any particular
outside agent. Claude Code, Codex, `t` and the rest are ordinary commands
in a workspace's Terminal, run by a person or by the Tilo agent. A model
provider (Anthropic, OpenAI, a local model) is one entry in AI settings,
not a code path of its own.

The proof that this works is our own development: in the company workspace,
the Tilo agent takes a small change to the tilo repo from a chat message,
checks it with `t`, commits it and pushes it.

### Calls

A call is an address, a typed input and who acts. It is admitted the same
way, runs the same code, returns the same typed result and writes the same
history entry whoever makes it. Every entry point is a projection of the
published tree with no code per action: a bound control in a view, a typed
call from tilolang (`tasks.done(task)`, checked at compile time), `tilo ls`
and `tilo do`, a few generic MCP tools, and SDK types generated from the
published types. An action names the view that shows it, so every open view
of the resource shows the change and who made it, and an agent can attach
that view to its answer.

Our own code calls only over the device link (`tilo-actions/1` on iroh).
That includes signing in: `tilo login <ticket>` connects straight to the
device that issued the ticket (its iroh endpoint id, a relay hint and a
one-time secret) and calls the certify action there, before the new device
holds a certificate. The issuer is any device of the person: a browser tab,
a desktop or a server. There is no site lookup. `/v1` REST is only an edge
for callers outside Tilo.

### Views

A view is published at an address and takes a resource. Any view can show
any other by naming it and giving it a resource: a task list in a chat
message, a calendar week in a note. The shown view runs with its own app's
code and the viewer's permissions, and edits inside it are calls to its
app's actions. The shell is a view. A person, or an agent in a chat, writes a
new view in tilolang from existing views and actions, without a build. One
view output is drawn as native widgets and as semantic HTML on the web page.

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
- A removal lands whole. Deleting the old copy, moving every caller onto the
  survivor and the survivor's own changes are one branch and one push (or a
  few commits pushed together), never "move now, delete later". Older
  branches and worktrees on the same code are taken into that push or
  deleted, not left parked.
- Test once, at the end. The change is finished first (old code gone,
  callers moved, `t cargo check` clean); then clippy and the focused tests
  run once, and it is pushed. No test or cost runs on a half-done change.
- The words-that-go check fails only when a new name carrying a specific
  word appears. More uses of a name that already exists are printed, not
  failed. Generic words (record, item, kind, document, surface, instance,
  held) are only counted, and nothing that falls ever fails.

---

## 1. Devices

### Today

A device's key and certificate live in four places, and none of them reads
another:

| Store | Code | Location | Size |
|---|---|---|---|
| Native device key and certificate | `tilo-device-identity` `IdentityPaths` (`persist.rs`, `keychain.rs`) | `$TILO_IDENTITY_ROOT`, else `/var/lib/tilo`, else `$XDG_DATA_HOME/tilo`: `device-private/keypair.bin`, `device/cert.bin`, `device/certs/<user>.bin` | 6,922 |
| CLI and agent login | `tilo-device-identity` `login` `Place`, `Device` | `$XDG_STATE_HOME/tilo/device/login.json`, with the device key and certificate above. A key an earlier tilo kept there (`{key,device.json}`) is taken over once. | 365 |
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
4. `tilo login` with a ticket, the certify action on the device that issued
   it (`tilo_device_identity::login`, `tilo_peer_state::new_device`, `tilo-cli`
   `login.rs` and `device.rs`), and the browser page's login codes
   (`tilo-server` `web/login_codes.rs` and `web/sign_in.rs` 1,500).
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
| File tree | `fs` `tree_sync.rs` (the workspace service, the standalone runtime, and the fs daemon through the session's `files-tree` socket), with two `tree_put`s |
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
| `tilo_fs::sync` and the fs ALPNs, the second `tree_put` | ~2,000 |
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
  library. The web page's HTML comes from the same view output, not from a
  second per-widget path. Today the page renders the composed widget tree
  through each widget's `html` (`composition.rs` `HtmlCx`, 277 files); the
  page entry (`PageShow`, `PageAct`) stays as the seam, and a renderer from
  view nodes replaces `Page::render` behind it, deleting `HtmlCx` view by
  view. No new `html` code is written meanwhile.
- **One sandbox for app code**: `tilolang-process`, started through
  `app-launch` and the `launcher` sandbox when an action is called or a view
  is shown, one per workspace and app, serving every window, page viewer and
  windowless call of that app, and stopped when idle. It is how the system
  runs app code, not an app being open. `app-keeper` stays only for programs that are not Tilo
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
| `tilo-server` `coding_agents` (a Claude Code harness) and every other path written for one outside agent (`claude`/`anthropic` outside the model-provider entry), once the Tilo agent does their job | ~4,000 plus what the sweep finds |

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
  and checked against the membership as their author saw it, so every
  member's device reaches the same answer without a coordinator.
- **One authority**: `action-policy` `Role` and `Grant`, read from that log,
  for people and agents alike. Keys follow membership through `key-group`.
- **No governor.** No device administers a workspace by being the device
  that holds it, and the server's own person has no power over a workspace
  it does not belong to.
- **One set of resources per workspace.** Mail, calendar, vault, models,
  connections, agents and skills belong to the workspace. Privacy between
  members is a reader rule on a record, never a private store. The keys
  enforce it, not a filter: a record with its own readers (a direct
  conversation, a private note) is its own resource, sealed under the key
  group of exactly those readers. Every device may keep and forward its
  sealed changes, and only the readers' devices can open them. A private
  record is never a row inside a document that has more readers.
- **What is a person's alone** (their keys, devices, appearance, the list of
  their workspaces, their servers) lives in the person's own workspace,
  where no other person is a member (their agents may be, acting for
  them). There is no second kind of store for it.

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
what it keeps and which work it takes, set per workspace.
The relay is a mode of that daemon. `tilo-api` reads grants from the
workspace, not from its own file. The agent and bot runner
(`tilo-server` `workspace_agents` and `workspace_bots`, the only copy) moves
into the workspace service, so any device that takes agent work
runs it. A device's first run needs no server: it creates the person's
identity and their first workspace, an ordinary workspace whose signed member
list names them as owner.

What goes: `tilo-runtime`'s `Standalone` with its self-governed profile
workspace, `ProfileGrants`, its own device provisioning, `--join` and
`tilo-runtime-mcp` (the CLI holder moves into `tilo-runtime` as the one
daemon); `tilo-api`'s grant file; the separate binaries once their verbs are
actions of the daemon. Owner:
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
| REST `/v1` | `tilo-api` (21,745): `v1/operations.rs`, a generated OpenAPI, `answers.rs`. Each route is turned into an MCP `tools/call` (`session.rs`) and the answer is parsed back, and `src/session/{members,unattended,invocations,models,keys}.rs` intercept some calls by tool name before any provider sees them | HTTPS, SSE; `api.sock` |
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
| The REST-to-MCP bridge in `tilo-api` `session.rs` and the intercepts in `src/session/` (REST calls the provider directly; members and models become providers, as a workspace's servers, apps and mail accounts already are) | ~2,800 |
| The hand-typed vocabularies in `actions/src/remote/` (replaced by `surface.toml` and generated bindings) | ~7,000 net |
| The hand-written REST clients: Rust `client.rs`, the TypeScript and Python runtimes (generated from the OpenAPI, around a kept core of about 600 lines per language for calls, events and errors) | ~6,200 |

About 18k lines, with `tilo-cli` `sdk.rs` growing by about 1k to generate
the clients.

Owner: **actions everywhere** session.

---

## Words that go

Each of these names a concept the design does not have. The word goes with
the code that holds the concept, never by a rename alone, and no new use of
any of them is added.

| Word in the code | What it is today | What the design has instead |
|---|---|---|
| governor, governance document, `GovernedWorkspace`, `NativeWorkspaceOwner`, succession | one device that admits a workspace's changes and owns its record | the signed member list every member's device checks |
| `tilo.host`, `tilo.member` namespaces, host as owner | a server's own records and authority beside the workspace | the workspace's resources; a server is a device |
| keeper | the person an agent belongs to, named apart from members | the person the agent acts for |
| roster, `tilo.workspace.agent` | a second list of agents beside members | members; an agent is a member acting for a person |
| stand-in (host, approver) | a device that signs or answers in a person's place | the person's own devices; an agent acting for them |
| `PersonalStore`, `(app, personal)`, `tilo-personal-store/1` | records outside every workspace, admitted by the identity alone | records in the person's own workspace |
| `person_settings`, `person_vault`, `person_model(s)`, `person_telling`, `person_asking`, `person_accounts`, `workspaces/people/<person>/` | per-person stores and their names on a host | the workspace's resources, or the person's own workspace |
| `AccountStore`, settings `Personal`/`DeviceLocal`/`Shared` scopes, `LiveSettings`, `settings.sock` | accounts and settings outside resources | resources: the workspace's accounts record, the person's and the device's settings as records |
| per-person cluster | one key cluster per person derived from the root | key groups per set of readers |
| own workspace, `own_workspaces`, Servers workspace (as a name) | the person's private workspace under several names | one: the person's own workspace |
| `ProfileWorkspace`, `ProfileGrants`, `Standalone` | a runtime that governs a private profile | the one daemon in an ordinary workspace |
| `own_service`, `OwnService`, `own_device` wrappers | each host wrapping the engine its own way | the one daemon |
| held, holder, `HeldWorkspace`, member copy, replica | names for a device having a workspace | a device that holds the workspace (no type of its own) |
| listing workspace, governed/member/held/own workspace kinds | kinds of workspace by who governs or keeps them | one kind of workspace |
| home peer, home-peer registry, home server | a separate registry and name for a person's server | a device of the person |
| `tilo-server-sibling/1`, sibling | a link between one person's devices outside workspaces | the device link of the person's own workspace |
| `WorkspaceRuntime`, `Membership { devices }` | a device allowlist for a relay | the member list |
| retained store, lineage, `CURRENT` generations, `tilo-server-store` | a host's second store of changes | the one change store |
| `Envelope::Delta`, `EnvelopeV2`, co-edit, anti-entropy | a second way to move changes through a host | the one device link |
| carrier (Direct, Relay, WebTransport) | a choice of path named in callers | the link; how it reaches a device is its own |
| hosted workspace, `-w`, page-host | a workspace on a server treated as another thing | a workspace; some device holds it |
| relay runtime | a server mode that forwards without being a device | a device that keeps sealed changes |
| fleet, `DeviceFleetStore`, `CertifiedFleet` | the person's device list under a third name | the person's devices |
| ceremony, `tilo-identity-ceremony` | a crate for a record format | the identity store |
| threshold identity | key generation across devices with no product use | nothing until a product needs it |
| enrolment, pairing, certification ceremony, handoff | seven ways to add a device | adding a device: a code one of your devices approves |
| document (as the unit), `workspace_documents`, document host | the storage format named as the thing | a resource |
| record, item, `ItemId`, item node | second and third words for a resource and its parts | a resource, inside its parent |
| kind, `KindSet` | the type of a resource under another name | resource type |
| channel ops, broker (two), publication, `CommittedView`, `committed_surface` | the plumbing between an app and the engine, each with its own names | a call, and a change |
| surface, `surface.toml` | the published actions under a second name | the workspace's published tree |
| instance, windowless, showing action, packaged model, native model, `model.wasm` | an app as a running thing, with ways to start it | an app is a package; the system runs its code when called |
| `DynResource`, `DynAction` | a bridge between two app models | one app model |
| `ResourceApp`, `ViewCx`, `#[view]` | Rust apps and their second view model | tilolang |
| outbox, effect receipt | app-side queues of outside effects | effects an action declares and the system carries out |
| `Answering`, `answers` (MCP) | an MCP answer re-typed for REST | the typed result of a call |
| legacy, compat readers | readers of formats main no longer writes | nothing: one migration, then the reader goes |
| `tilo-workspaces`, `(app, workspace, document)` | the old spelling of a workspace on disk and in addresses | the workspace id and an address |
| invite shelf, invitation receipts | a site-side store of invite links and a receipt log | an invitation, a resource of the workspace |
| unattended (`commands.unattended`) | a flag on the governance document | a grant |
| wait-list as a governance verb | a list kept beside members | a resource of the workspace |
| appetite, key holder, store (as a kind of device) | words from this page for what a device keeps | what a device keeps, and whether it can read |
| tilo-control row dialect, `legacy_rows`, `param_row` | a second wire for actions | the one call |
| REST clients in our code (`tilo-api` `client.rs`, SDK HTTP runtimes) | our code calling our REST edge | calls over iroh |

## Plan

Each piece's "What goes" table is the work. This table says where it
stands; the architecture owner keeps it current. A session works down its
rows without being told; nobody waits for a nudge.

When to land. Land whenever a coherent chunk is finished, compiles and
passes its focused tests: typically a whole removal of a few tens of
thousands of lines in one session. Don't let a branch drift far from main
or sit finished. Don't split a removal into a move now and a delete later,
and don't hold a finished removal back to batch it with unrelated work. No
landing dates: report what is done, what is on a branch, and what a push
waits on, if anything.

| Piece | Left on main | On a branch now | Session |
|---|---|---|---|
| Devices, identity | ceremony crate, onboarding's `DiskIdentityStore`, `BootKeyCert` copies, `tilo-device-admin`, `threshold-identity`, the old-login key reader, the browser root and `device-key.mjs` | person store (2 person stores left) | One identity |
| Sync | host retained store (`store/`, `persistence.rs`, `compaction.rs`, `tilo-server-store`), delta and co-edit envelopes, second file path, broker overlap, `settings-store` document layer, `app-state` `FsStore`, `content-device` | one store −33k, one file path −15k, broker step 2 | One engine |
| tilolang store | `index/` (10.6k), `TiloStore` and its mirror, chunks; then the redb pager into resource-store | store move −50.5k | tilolang onto the resource store |
| App framework | Rust Chat (71k), Rust Tasks (13.5k), `apps/tilolang` (21k), 65 `*-model` crates, `tilo-view`, `tilo-app` `framework/` (64k) after the ports, the Rust shell, the toolkit's second action path | Chat −80k (three agent branches, one deletion), Tasks with Notes on the web −11.7k, shell context menu | Delete Rust Chat; Rust apps to tilolang; cleanup; legacy readers |
| Agents | `coding_agents` (4k) and every Claude-specific path, the other agent engines | C −6k | Agent engines |
| Workspace | `PersonalStore`, `AccountStore` and the `accounts` crate, the second workspace list, the person document of `settings-store`, governor role, `Membership`/`WorkspaceRuntime` | `one-workspace-stores` −11.8k | one workspace |
| Hosting | `Standalone`, `tilo-runtime-mcp`, the CLI holder, `tilo-api`'s grant file, the separate binaries | one daemon | hosting |
| Actions | device-mcp and `OnDevice`, the REST-to-MCP bridge and intercepts, tilo-control's row dialect, the hand-typed vocabularies, the hand-written REST clients | none since invocations landed (54797d6eb39) | actions everywhere |

Progress is measured as net lines removed on `origin/main` in each piece's
crates since `d81cc1750ff`, and as copies of each concept left.
