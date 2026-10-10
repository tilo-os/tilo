# Security Architecture

> **Scope note.** This is the *workspace-scoped* threat model: the remote-input
> firewall, the per-workspace sandbox and the portal capability model, in more
> detail than anywhere else. It predates the machine-scoped work, so on identity,
> the boot chain and disk encryption it is superseded by
> [docs/security/](../security/README.md).

The security boundary is the **workspace**. Every workspace runs apps inside its own Bubblewrap sandbox with its own user namespace, mount namespace, seccomp-bpf filter, and a per-workspace Wayland bridge socket. Capabilities (filesystem, hardware, network) require explicit grants surfaced as inline prompts; grants persist in the workspace's CRDT and replicate to peers.

## Threat model

The scenarios we defend against:

1. **Malicious guest** — a peer joins a workspace and attempts to read files the workspace was never granted, snoop on tabs not in the shared workspace, or inject input into the host's private windows.
2. **Compromised app** — a valid sandboxed app has a vulnerability that a remote attacker exploits to attempt sandbox escape, network exfil, or privilege escalation.
3. **Misbehaving peer** — a connected peer issues capability requests the owner didn't authorise. The portal backend gates everything.
4. **Man-in-the-middle** — an attacker intercepts P2P traffic to inject fake input events or steal file data.

## Identity

- One Ed25519 device key per machine at `/var/lib/tilo/device-private/keypair.bin`, provisioned by root before login. Every network and document identity derives from it — no subsystem mints its own. The file is 0640 in a root-owned, non-writable `tilo-identity` directory: the root filesystem daemon and session compositor can read it, but session processes cannot delete or replace it. A key that exists but cannot be read is an error, never a reason to re-key.
- The certified device public key is the Iroh `EndpointId` used by the shell, filesystem, and workspace-host client. It is the stable peer identity everywhere.
- Identity decisions carry the whole peer id (`PeerOrigin`), never a fold of it: an equality check a digest can be ground against is not an identity check.
- The personal signing identity is the user root. It certifies each device key; workspace hosts accept only device certificates chaining to the configured user root and permitted by the workspace membership/revocation state.

Invite tokens are signed ed25519 blobs scoped to a workspace; see `docs/architecture/collaboration.md §7` for the wire format and validation order.

## Sandbox: Bubblewrap profile

Every app launched into a workspace runs through `bwrap`, whose argv is built by `tilo-launcher` (`crates/packaging/launcher`) from the package manifest and the capability grants the shell hands it. The compositor never builds a sandbox: it execs `tilo-launcher` (`shell::shell::launcher`), which is the single enforcement point. Inside the new mount namespace the launcher re-execs itself as `__sandbox-exec` to install Landlock before the app takes over. Landlock ruleset construction and seccomp compilation live in `crates/system/sandbox-core`, a leaf crate the launcher and the `ush` runtime share, so the launcher still depends on nothing in the compositor.

A launch is refused unless the complete Landlock policy is enforced, with one exception. A hosting provider's single-tenant VM or sandbox can be the security boundary itself and offer no Landlock (gVisor implements none; stock Firecracker guest kernels leave it out). An image built for such a machine declares it in `/etc/tilo/provider-sandbox.toml` (`provider_sandboxed = true`, root-owned, not group- or world-writable); the environment cannot. There, apps whose payload lies in the root-owned system store (`/usr/lib/tilo-apps`) run with Landlock not or only partially enforced, which the sandbox shim logs; bubblewrap, seccomp and the network namespace still apply, and a ruleset that fails to build still refuses. Apps a user installs keep the full requirement (`launcher::provider_sandbox`).

What a sandboxed app is actually presented with. It names none of this; the launcher decides all of it, and the read side is identical for every package:

- Read-only: the system read profile (`launcher::profile`) — loader and C library, fonts, GPU loader manifests, `/sys`, CA bundle, timezone data — plus the package payload at `/var/tilo-apps/<pkg-id>` and its runtime bundle. `/etc/resolv.conf` and `/etc/hosts` only when the launch shares the host network.
- Read-write: exactly one directory, `<data home>/tilo-rooms/<room>/<pkg-id>` → `/run/tilo/app`, with `HOME` and every `XDG_*` pointed inside it. Two rooms are two directories, so an app opened in one room cannot read what it wrote in another. Plus the bridge socket → `/run/user/1000/wayland-0`.
- `env`: `WAYLAND_DISPLAY=wayland-0`, `XDG_RUNTIME_DIR=/run/user/1000`, and `TILO_APP_DIR`, `HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME` all under `/run/tilo/app`.
- Private DBus session bus per workspace.
- `unshare(CLONE_NEWNET)` if `net_isolated`. Default: invited peers run isolated; the owner's session runs without isolation unless overridden.

Anything outside that set is a capability the user grants at runtime, which becomes a bind on the app's next launch (`launcher::capabilities`).

### Application audio output

Hosted apps submit output PCM through the launcher's attested collaboration channel;
they receive no PipeWire/PulseAudio socket, device path or microphone grant. The
output-only `audioOutputCommand` service is independent of Chat call membership and
capture. The broker derives the app, workspace and record from the admitted route,
checks its live kernel process custody and admission, and fences its output when
the process disconnects or authority ends. The compositor also fences output on
session lock, onboarding or the sound-output privacy policy.

Each output has a host-issued generation and an eight-packet queue (160 ms of
48 kHz stereo PCM). Sequence checks make exact packet retries safe and reject
changed or reordered retries. Pause, flush, volume and close apply only to that
app's generation. The host pins playback to the device serial and daemon cookie
from its existing native device observer; a foreign link, removed output or lost
link fails closed. Rendered-frame receipts count PCM consumed by the verified host
device callback, rather than a decoder buffer, playhead or fake sink. Music keeps
its existing resource actions as the user, accessibility and automation surface.

The governed implementation uses the Linux host's PipeWire backend. An unavailable
service or device is a playback error. It does not relax the app sandbox or use
capture access as an output fallback.

### Native media plugin discovery

The signed application runtime supplies GStreamer core libraries. Platform
GStreamer decoder plugins remain in the existing read-only system library
profile, as graphics drivers do. GStreamer normally locates plugins beside its
own library; relocating that library into `/run/tilo-runtime/<version>/lib`
therefore needs an explicit search path. The launcher sets
`GST_PLUGIN_SYSTEM_PATH_1_0` to that runtime's `lib/gstreamer-1.0` followed by fixed
system plugin directories already covered by the profile. It does not inherit
session plugin paths, expose a user plugin directory, bind a new host path, or
change Landlock, seccomp, network or audio-output authority. The same native
media libraries and platform plugins must remain ABI-compatible.

## Seccomp-bpf blocklist

`tilo-launcher` compiles the filter for every launch with the `seccompiler` crate (`sandbox_core::seccomp`) and hands it to `bwrap --seccomp <fd>`. It is a default-allow deny-list answering `EPERM`, derived from Flatpak's profile:

```
ptrace, mount, umount2, pivot_root, chroot, unshare, setns,
clone (with any namespace flag), socket(AF_PACKET, …), bpf, keyctl,
kexec_load, {init,finit,delete}_module, reboot, swapon, swapoff,
userfaultfd, perf_event_open, open_by_handle_at, process_vm_{readv,writev},
pidfd_getfd, fanotify_init, fsopen, fspick, fsconfig, fsmount, move_mount,
mount_setattr, quotactl, acct
```

The default profile, `tilo-default`, also refuses any `mmap`/`mprotect`/`pkey_mprotect` asking for writable and executable memory at once. `tilo-jit` drops only that rule, and a manifest's request for it is honoured only for packages the image entitles (`/usr/lib/tilo/jit-entitled.conf`; `launcher::seccomp::resolve_profile`). `tilo-web-engine` is `tilo-jit` for a web engine that confines its own renderers: it also permits `clone`/`unshare` with `CLONE_NEWUSER`, `CLONE_NEWPID` and `CLONE_NEWNET` and `chroot`, the parts of Chromium's renderer sandbox, and still refuses `setns`, `mount`, `pivot_root` and every other namespace kind. It is honoured only for packages in `/usr/lib/tilo/web-engine-entitled.conf` (built in: the browser); a package entitled to the JIT but not to this gets `tilo-jit`. The compiled programs are pinned byte for byte by the launcher's tests, so any change to the filter is reviewed as a policy change.

## App workers for host agents and room actions

A host running agents (`tilo-relay --features agents`) acts on its rooms'
records for agents and for the owner's pages (`RoomActions`). That host
process holds its device signing key, every agent's device key
(`agents.json`) and every room it serves, so no app code runs in it. Each
record is held in its own app worker (`crates/agent/agent-participant/src/worker`):

- **Process.** The host's executable started again in worker mode
  (`worker::dispatch_early`), one per record, with a cleared environment and
  its supervisor's socket as stdin. Registered reads and view discovery of an
  existing record each get a worker too.
- **Confinement**, before it reads a request, from the shared primitives:
  `tilo-sandbox`'s `detach` (inherited descriptors closed, dies with its
  supervisor) and resource caps; on Linux a Landlock ruleset granting no path
  or port (best effort), the seccomp parser policy (`sandbox_core::seccomp::parser_program`:
  application baseline with write-xor-execute, no socket, `connect`, `bind`
  or `exec`) after the `clone3` compatibility filter, and a deny-list
  refusing every syscall that opens, creates, removes, renames or relinks a
  path, `io_uring`, and signals to any other process. The seccomp layers are
  enforced on every kernel, so the state directory stays out of reach where
  Landlock is missing. On macOS, `tilo-sandbox`'s Seatbelt profile with no
  readable path. A worker that cannot confine itself serves nothing and the
  operation is refused; there is no unconfined fallback.
- **Keys.** The protocol (`worker/proto.rs`) carries app and record ids, the
  participant's public name, Automerge bytes and a caller's read policy,
  never a key. The supervisor seals and signs.
- **Untrusted output.** A worker's Automerge bytes are admitted to the
  supervisor's own copy of the record through `tilo-automerge-isolate`
  before they are signed: a whole document must load, an increment must
  apply on top of the record with nothing missing. Incoming changes are
  admitted the same way before a worker sees them, and so are the changes
  of records no worker holds: an agent's unopened records and the room's
  shared desktop, whose tabs agents and room actions read
  (`participant/unopened.rs`). With no isolate those changes wait and the
  record does not open; nothing decodes them in the host instead. A worker
  that dies is replaced from that copy, at most twice per record.

Still in the host process: the app catalogue is linked statically (each
app's `definition()` builds there), and agents' device keys are kept in
`agents.json`. Moving the catalogue to installed packages and the agent keys
to the secret store are tracked follow-ups.

## Per-workspace Wayland bridge socket

The compositor's main wayland socket lives at `$XDG_RUNTIME_DIR/wayland-1` (or whatever the listener registered). For each workspace, the bridge (`crates/session/shell/src/compositor/bridge.rs`) binds an additional listening socket `$XDG_RUNTIME_DIR/tilo-ws-<ws_id>` and proxies its connections to the main socket.

The bridge:

- **Tags** every accepted client with a `WorkspaceTag` `UserData` on the smithay `Client`. Adoption logic in `tab_binding` reads this tag to route surfaces to the correct workspace.
- **Filters** protocol messages against the workspace's grants. Pass-through globals: `wl_compositor`, `wl_subcompositor`, `wl_shm`, `xdg_shell`. Capability-gated globals: `wl_data_device` (cross-workspace clipboard, blocked v1), `zwlr_screencopy_v1` (always prompts).
- **Routes** the per-workspace seat capabilities so input from the compositor's seat only reaches surfaces tagged for the corresponding workspace.

Surfaces from sandbox A literally cannot adopt into workspace B — the bridge tag mismatches.

## Capability grants

Capabilities are CRDT-persisted (`crates/session/shell/src/shell/state.rs`):

```rust
pub enum Capability {
    Filesystem   { path, mode: FsMode,          scope: GrantScope },
    Camera       {                              scope: GrantScope },
    Microphone   {                              scope: GrantScope },
    Screencast   {                              scope: GrantScope },
    RemoteDesktop{                              scope: GrantScope },
    Network      { hosts: Option<Vec<String>>,  scope: GrantScope },
}
pub enum FsMode    { Read, ReadWrite }
pub enum GrantScope { Once, Session, Always, Workspace }
```

`Workspace.grants: HashMap<AppId, Vec<Capability>>` persists Session/Always/Workspace-scoped grants. Once-grants live only in the in-flight prompt's resolution and never hit the CRDT.

Replication semantics:

- Grants replicate to peers as part of the room document.
- **Only the owner can grant.** Non-owners see the grant rows in the Permissions tab but the Revoke button is disabled.
- A grant revocation writes the removal to the CRDT. After the normal durable writer accepts the new policy, the launch supervisor interrupts sandboxes retaining withdrawn app filesystem capabilities, including their open descriptors. New overlapping restrictions and expired grants use the same interruption path. The host binds this decision to the original process and durable room; another app or another room's instance is unaffected. Each launch retains its own private capability file, so another room launching the same package cannot replace its snapshot. Document opening queues its new grant through that writer. The launch worker waits for earlier publication and rechecks its snapshot before the launch gate opens; refused or superseded writes cannot authorize the child. Relaunch uses the reduced profile; revocation does not restart the app or grant background authority.
- **Only the owner can grant over the wire either.** A room document's owner and capability list are accepted only from the peer entitled to write them, so a guest cannot project its own cache into the room and let automerge's last-writer-wins decide who administers it. Every receiver — the native room peer and the shell — admits a room document through the one function `tilo_peer_state::room::admit_room_changes`, and a room whose governor no invite, minting or person names (`Unpinned`) refuses every governance change rather than trusting whoever answers first. See `docs/architecture/collaboration.md`, "Room admission" and "Write authority on a room's governance", for the record that decides it, why the invite's signature is what makes it checkable by the receiver, and what it does not yet cover (a relayed or delegated governance change, which needs the op signed rather than the hop authenticated).

### Inline prompt

When an unscoped capability request arrives, a `PermissionPrompt` widget pushes onto `Chrome::pending_grants: VecDeque<(AppId, Capability)>` and renders inline at the top of the active strip (looks like the notification bar). Buttons: **Allow** / **Allow this session** / **Always allow** / **Deny**. "Always allow" is disabled for non-owners.

### Portal D-Bus backend

The compositor implements the `org.freedesktop.impl.portal.*` backend (`crates/session/shell/src/portal/`) via `zbus`. We expose these impls under the service:

| Interface                               | Methods                                | Capability flow                                   |
|-----------------------------------------|----------------------------------------|---------------------------------------------------|
| `…impl.portal.FileChooser`              | `OpenFile`, `SaveFile`                 | Returns a one-shot file descriptor, not a path    |
| `…impl.portal.ScreenCast`               | `CreateSession`, `SelectSources`, `Start`, `OpenPipeWireRemote` (+ `AvailableSourceTypes`/`AvailableCursorModes`/`version` props) | Session-tracked selection: source types (monitor/window/virtual), cursor modes (hidden/embedded/metadata), `multiple`, and `persist_mode`+`restore_token` so a re-share skips the prompt. `Start` provisions a real PipeWire node per source; `OpenPipeWireRemote` hands back a scoped remote fd |
| `…impl.portal.RemoteDesktop`            | `CreateSession`, `SelectDevices`, `Start`, `NotifyPointer*`/`NotifyKeyboard*` (+ `AvailableDeviceTypes`/`version` props) | `SelectDevices` prompts (`Capability::RemoteDesktop`); `Notify*` is gated on the started session's granted device bitmask, then decoded into a `RemoteAction` and injected into the focused surface via the seat |
| `…impl.portal.Access` (incl. Camera)    | `AccessCamera` flows here              | `/dev/video*` only bound on grant                 |

Per request:

1. Identify the caller's workspace via the bridge tag.
2. Look up the caller's app-id in `Workspace::grants`. If `Always`/`Workspace`-scoped, grant immediately.
3. Else push to `pending_grants`; await user response; cache per `GrantScope` rules.

Step 3 needs something to empty that queue. The compositor drains `PromptQueueHandle` onto `ChromeState::pending_grants` every frame (`ShellOverlayHost::drain_portal_prompts`), and each entry carries the portal's reply channel, so the user's click resolves the D-Bus method that is parked on it. An entry dropped without an answer reads as deny — the failure mode is a refusal, not a hang.

**The portal is served by the compositor**, in-process, in both native and hosted mode (`compositor::native::start_portal_backend`). It is not a separate daemon: the questions the portal answers are all about the live session — which workspace the caller is in, what it has been granted, what the person here decides now — and all of them live in the `WorkspaceTree` the compositor owns. The former `shell --portal-server-only` unit built its context on an empty tree and had no output to draw a dialog on, so it could answer none of them; it is removed (the binary refuses the flag with exit status 2 rather than ignoring it into a second compositor), and D-Bus permits one owner per name, so leaving it in place would have been a race whose loser served nobody while looking healthy. `ServerHandle::owns_name` reports whether the name was actually acquired, and the compositor logs at error level when it was not.

`xdg-desktop-portal`'s config (`os/usr/share/xdg-desktop-portal/portals/tilo.portal`) registers `org.freedesktop.impl.portal.desktop.tilo` as the backend for these interfaces. ScreenCast output streams use PipeWire (`crates/session/shell/src/portal/screencast_pipewire.rs`); the provisioner is a seam (`MockScreenCastProvisioner` for headless/tests, a real libpipewire connection in the production OS build). RemoteDesktop seat injection reuses the same `RemoteInputSink`/seat path as peer-sync remote input.

`tests/portal_test.rs` exercises one round-trip per interface plus prompt-resolution branches. A `PromptResolver::AutoAllow` mode is used in tests so the round-trip doesn't need a human.

## Default-deny enforcement

### Filesystem

- An app names no path. `crates/packaging/launcher` binds a fixed system read profile (`launcher::profile`: loader and C library, fonts, GPU loader manifests, `/sys`, CA bundle, timezone data) plus one read-write directory per room per package at `/run/tilo/app` (`launcher::appdir`), and nothing else. The `[sandbox.fs]` manifest block is retired; a package that still declares one is refused at install and at launch.
- `FileChooser` returns `file://` URIs because `org.freedesktop.impl.portal.FileChooser` answers `OpenFile`/`SaveFile`/`SaveFiles` with a `uris` key rather than file descriptors.
- **The dialog is the prompt.** Choosing a file in the compositor-owned dialog is the act of authorisation, and the grant names exactly the path chosen: read-only for `OpenFile`, read-write for `SaveFile`, session-scoped. A separate prompt must not grant a broad path before the user chooses one.
- A path outside the profile is reachable only through a capability the user grants, which becomes a bind at the next launch of that app (`launcher::capabilities`, the enforcement point).

  **Next launch is the operative phrase.** A confined app's mount namespace is fixed when `bwrap` creates it, so a grant made mid-session cannot add anything to the namespace the app is already living in. `crates/session/shell/tests/portal_grant_reaches_sandbox.rs` asserts the grant→bind chain end to end; what it asserts is the *next* launch. For a running app the portal additionally publishes the pick through `shell::portal::doc_store`, which is where the three candidate mechanisms — a launch-time grant, a live re-bind, and a document store — are compared and one is chosen. The shipped store is `HostPathStore`, which is correct for callers sharing the compositor's namespace and reports `reaches_confined_apps() == false` rather than pretending otherwise; the compositor logs that at startup.

**The mount is the second enforcement point** (`crates/system/fs/src/acl.rs`). `fs --acl FILE` binds a capability list to the mount, and every `open`/`read`/`write` is answered by `collab_types::decide_capability` — the same function the compositor calls, so a path readable through one and not the other is a bug rather than a policy. Without `--acl` the mount keeps the Unix-veneer-only behaviour, which is the documented opt-out.

Two properties this rests on:

- **The list is re-read while the mount is up** (`crates/system/fs/src/caps_file.rs`). `/var/workspace` is mounted at boot and lives as long as the session, so a list read only at mount time could not express a grant — or a revocation — made afterwards. A deleted list denies everything (its absence is the producer saying nothing is granted); an unreadable or malformed one keeps the last good list, because the file is rewritten in place and a reader can catch it truncated.
- **The shipped image enforces.** `os/usr/lib/systemd/system/fs.service` passes `--acl /var/lib/tilo/fs-capabilities.json`, seeded by `tmpfiles.d` from `/usr/share/tilo/fs-capabilities.json`. Be plain about what that default is: it grants the local session read+write on the whole workspace — exactly what an unenforced mount already gave. It narrows nothing on a single-user desktop. What it buys is that the enforcement point is *live*, so a restriction added to that list bites without a remount, and a mount joined from a peer's ticket has somewhere to put a narrower policy. The shipped default uses a human-readable principal (`person:local`); the mount itself presents the same durable, certified device key as the shell, so remote policies may name a stable cryptographic subject.

### Network

- `unshare(CLONE_NEWNET)` isolates per-workspace.
- Owner workspace defaults to "no isolation"; invited-peer workspaces default to "isolated".

**Per-host egress** (`crates/packaging/launcher/src/netgate.rs`). A grant naming specific hosts does *not* share the network namespace. The namespace stays empty and the app's only route out is an `AF_UNIX` socket bind-mounted from the launcher, which speaks SOCKS5 and enforces the allowlist. A grant for `*` still shares the host network — there is no allowlist to enforce, so there is nothing to broker.

Three properties make that a boundary rather than a convention:

- **An app that ignores the gate is offline, not unfiltered.** An unshared namespace has no route, so there is no fallback path to fall back to. `crates/packaging/launcher/tests/netgate_egress.rs` asserts this against a real sandbox.
- **The allowlist is matched against the host *name*,** taken from the SOCKS request, before that name is resolved. So a rotating CDN works, a DNS record that changes mid-session is just two resolutions, and no address is ever pinned.
- **An address literal never satisfies a hostname entry.** Otherwise an app could resolve an allowed name itself and then ask for the address — and anyone controlling a DNS answer would turn a name-scoped grant into an any-host grant.

**One grant grammar, two backends.** A `NetHost` grant means the same thing to every sandbox that reads it. `collab_types::caps::NetHostPattern` is the only parser and `collab_types::caps::EgressPolicy` the only policy: `*` is every host; `example.com` is that host; `.example.com` (spelled `*.example.com` too, normalised on read, never a glob) is the domain and every name under it; names compare lower-case without a trailing dot. An IP literal is not a host: it is refused when written, and a stored one is ignored with a warning, so it narrows and never widens. `decide_capability` (`host_covers`), the launcher's gate and `tilo-egress` all read it, and `crates/collab/collab-types/tests/net_host_vectors.txt` is checked against each of them on its production path. What differs per sandbox is only the backend:

| Sandbox | Backend | Named hosts | `*` |
|---|---|---|---|
| App (`launcher`) | Empty netns + the gate (`netgate.rs`) | Gate matches the name, resolves after policy; device-local only for an exact name | `--share-net`: the host network, its loopback included |
| Windows capsule (`compat-service`) | The same gate | As apps | `--share-net`, as apps |
| Windows guest (`windows-vm::guest_network`) | Lease-scoped HTTP `CONNECT` relay over the gate's connect path | As apps | Every host through the relay |
| Environment, build (`ush`, `env-tools`) | Own netns + DNS stub + nftables (`tilo-egress`) | Stub answers granted names; nftables admits only answered addresses | Own namespace, no filter: the internet, not this device's loopback |

The `*` row is the one deliberate difference. Moving an app's `*` behind a gate in allow-all mode would make it match an environment's, but it would break first-party apps that depend on the host network: Music and Mail (manifest `net.mode = "host"`) take OAuth redirects on a loopback listener (`tilo-oauth`) that the host's browser must reach, and the gate carries TCP only, so QUIC and other UDP stop working. It would also make a `*` grant narrower than no grant at all for a `host` manifest. `launcher::netgate`'s `a_star_grant_keeps_the_host_network_for_apps_deliberately` pins the decision.

DNS is treated as egress. An app with no grant reaches no resolver; an app with an allowlist needs none, because the gate resolves on its behalf after the name has passed the policy. So an app cannot make the gate resolve a name it was not allowed to reach, and cannot signal through query contents because it emits no query.

This needs no privilege and no kernel feature `bwrap` does not already require. Address-based `nftables` rules would not preserve hostname-scoped policy and would require `CAP_NET_ADMIN`, which the launcher does not have.

Not constrained: **ports** (a `NetHost` scope names a host and carries no port) and **content** (the gate relays TCP and deliberately does not terminate TLS).

**A filtered grant is TCP-only.** The namespace is empty, so the sole route out is the gate socket, and both protocols it serves — SOCKS5 `CONNECT` and HTTP `CONNECT` — carry TCP. SOCKS5 `UDP ASSOCIATE` is refused outright, because a connectionless path carries no name for the policy to match. So an app under a host-scoped grant has no UDP egress at all: QUIC and HTTP/3 do not work, and a client that does not fall back to TCP is offline rather than slow. A grant of `*` shares the host network and is unaffected. This is a real behavioural constraint of choosing a name-filtering broker, not an oversight — there is no way to name a host in a UDP datagram.

**What the permissions panel can express is narrower than what the gate enforces.** The composer's scope picker (`crates/session/desktop/src/workspace_settings.rs`, `GRANT_CHOICES`) is a fixed list with one network entry, "Network access", which mints `NetHost("*")`. So the mechanism accepts a host-scoped grant and enforces it exactly, but the only grant a user can *make from the UI today* is the unrestricted one. Two consequences worth stating plainly:

- An app whose manifest enumerates endpoints is still filtered to them, because a `*` grant narrows to a declared allowlist rather than widening past it. This is the common case and it works.
- An app whose manifest says `net.mode = "host"` is unrestricted once the user clicks the one network choice, and the UI offers no way to say "only `mail.example.com`". Closing this needs the composer to offer the package's declared endpoints as individual choices (the manifest is already parsed into `InstalledApp`), or a hostname text field. Until then, a host-scoped grant is reachable through the capability file and the manifest, not through the panel.

If the gate cannot start, the launch is **refused** rather than downgraded — sharing the network would apply "this host" as "every host". `TILO_LAUNCHER_NET_GATE=off` takes the unfiltered path deliberately and logs it — in a developer build only. Like the launcher's other trust and policy overrides (system app store, runtime bundle root, trusted-publisher list, JIT and untrusted-parser policy files, D-Bus proxy binary; see `crates/packaging/launcher/src/dev_env.rs`), a release build ignores it with a one-time warning unless built with `--features dev-overrides`, and a release compositor strips all of them from the launcher's environment.

### Hardware

- Camera / microphone: `/dev/video*` and the PipeWire socket are unbound by default. A grant binds them.
- Screencast: the captured stream is the requesting app's tab content only — never the full output, never another tab.

## Input firewall (remote input)

When a peer publishes `RemoteInput { peer, surface, event }`, the owner's compositor runs it through `State::review_remote_input` (`crates/session/shell/src/compositor/state.rs`) — the single door between the network and the seat. The order below is the order that function reads in:

0. **Host overrides** — the kill-switch latch, the session lock, then physical-input priority. All three are absolute and none looks at the event, so a flooding peer cannot delay any of them. The lock is checked here rather than falling out of the z-order test because the lock surface is not part of the tiled tree: the tiles under it stay laid out and stay topmost as far as the tree knows.
1. The per-peer `read_only` flag. If set, drop.
2. Surface ownership — there must be a share record for this `(peer, surface)` pair. A peer inventing a surface id it was never given is refused here.
3. **Region check** — is the event coordinate inside the shared surface's content rect? A peer sees only the surface it was shared, so it sends *surface-local* coordinates; an accepted one is translated into the owner's output space by adding the tile origin, which is what confines a guest's pointer to the rectangle it was actually given.
4. **Z-order check** — is the surface visible and topmost at that point? (Prevents clickjacking via transparent overlays.) The hit test runs against the live tile list the compositor publishes each tick from `client_input::foreign_tiles` — the same enumeration `foreign_tile_at` routes *local* clicks with, so the two cannot drift into disagreeing about what is on top.
5. **Keyboard scrubber** — system shortcuts are denied for remote users. The denylist is not a second hand-written table: it is `client_input::shell_owns_chord`, the one the shell already uses to decide whether the *local* user's keystroke belongs to the compositor rather than to the focused client, plus the chords the shell never classifies because something below it takes them first — `Ctrl+Alt+F*` (kernel TTY switch) and `Ctrl+Alt+Delete`. The Super key is refused on its own, and `Alt+Tab` is named explicitly. Modifier state is reconstructed from the peer's own keycode stream rather than trusted from the wire's `modifiers` field, which the peer writes and can zero. Focus guard: keystrokes only inject if the shared surface holds the seat's keyboard focus; if the host focuses a private window or shell chrome, the remote keyboard stream mutes instantly.
6. **Rate limit** — max 120 input events/s per remote peer; >10 clicks in 100 ms trips a one-second block on everything from that peer. Every event a peer offers is metered, including the ones another check refuses, so a flood of junk cannot buy a fresh budget.
7. Inject into the compositor's `smithay::input::Seat`.

Steps 3-6 live in `crates/session/shell/src/network/input_firewall.rs`. Every one of them is **deny-biased**: a check that cannot answer refuses. A compositor that has published no screen refuses all remote input rather than passing it, and a button that arrives before any motion has established where the peer's pointer is has no coordinate to check and is refused.

## Host overrides

- **Session lock.** While the lock surface is up, every peer/workspace remote input event is refused. A locked machine has no driver. The existing local device-owner admin socket may submit a Secret password to the normal asynchronous PAM lock verifier; it cannot choose another account, inject app input, or clear the lock. Only the verifier’s ordinary success result unlocks the session.
  Native compositor restarts retain this state within the authenticated PAM
  login. `tilo-session` initializes `lock-state` in its new private logout-channel
  directory; it does not import this channel into the user manager or D-Bus
  activation environment. Before admitting input or painting, the compositor
  validates the owner-only channel and arms a locked recovery record. TERM, INT
  and HUP wake ordered teardown, which publishes the final lock state only after
  presentation, clients and control endpoints stop. A crash, missing record,
  corrupt record or incomplete teardown recovers locked; an invalid owner or
  redirected channel refuses startup. A new authenticated login gets its own
  directory. This is local session state, outside collaborative documents and
  undo. Recovery uses the existing password verifier; there is no new unlock
  action, environment switch, credential or sandbox permission.
- **Physical priority.** Physical input from the host's seat causes a 500 ms block on all remote input — the user can always grab control. Stamped at the backend event funnel in `compositor/native.rs`, so branches that never reach the client router (the lock screen, the VT chord, the power key) still count as the host taking over.
- **Kill switch (`Super+Esc`).** Stops every peer's input at once: the firewall
  latches, held remote keys are released, peer-seat attribution is dropped, every
  active share session is unregistered (terminating its capture stream), and the
  share registry is cleared. Recognised at the same funnel, ahead of the client
  router and ahead of the shell's own `Escape` handling — a panic key an overlay
  or focused client can swallow is not a panic key — and confirmed with a
  persistent notification that bypasses Do-Not-Disturb.

  The broader machine-wide emergency teardown is still incomplete: the
  underlying P2P transport is not disconnected, durable session capability
  grants are not revoked, peer-owned FUSE bind-mounts are not unmounted, and the
  screen is not locked. There is deliberately no UI affordance for clearing the
  latch; restoring a guest's write access requires starting a new share rather
  than silently reviving the stopped session.

## Encryption

- Iroh provides encrypted QUIC transport, authenticated endpoint ids, NAT traversal, and encrypted relay fallback. Workspace hosts add mutual device-certificate authentication.
- **A cross-device action is signed by the device that sends it, not vouched for by the host that routes it.** Device certificates are public, so a workspace host — including a hosted server operated for the user — could otherwise name any member it has seen as the source of an action it invented. The sending device signs the exact action and its issue time; the target broker refuses any effect without that signature, outside a 60-second freshness window, or already admitted, and still checks the certificate against the room's current authority. Payloads are not hidden from the host. See `docs/architecture/collaboration.md`, "Private actions are signed by their source device".
- iroh's FUSE traffic (`fs`) is QUIC + TLS 1.3.
- At-rest encryption for cloud-backed `/var/workspace` is a follow-up.

## Security matrix

| Attack vector              | Defense                                                 | Component                          |
|----------------------------|---------------------------------------------------------|------------------------------------|
| Unauthorised peer connect  | Ed25519 invite token + authenticated Iroh endpoint      | `network/invite.rs`, Iroh          |
| File exfil from sandbox    | bwrap mount namespace + fixed system read profile + one per-room app dir | `packaging/launcher/{profile,appdir}.rs` |
| Hardware exfil (cam/mic)   | Default-deny binds + portal-mediated grant              | `packaging/launcher/capabilities.rs`, `portal/` |
| Cross-workspace input leak | Bridge tag + per-workspace seat routing                 | `compositor/bridge.rs`             |
| Clipboard data leak        | `wl_data_device` blocked at bridge v1                   | `compositor/bridge.rs`             |
| Input injection (remote)   | Region + Z-order + scrubber + rate limit                | `network/input_firewall.rs`, driven from `compositor/state.rs::review_remote_input` |
| Sandbox escape via syscall | seccomp-bpf blocklist + Landlock                        | `system/sandbox-core`, `packaging/launcher/landlock_shim.rs` |
| Host key theft via a hostile record (agents, room actions) | One confined app worker per record; keys and signing stay in the host; worker output admitted through the isolate | `agent/agent-participant/src/worker/` |
| Network exfil to an ungranted host | Empty netns + name-matched SOCKS5 egress gate   | `packaging/launcher/netgate.rs`    |
| Clickjacking               | Z-order verification before remote input inject         | `network/input_firewall.rs`, over the tile order `compositor/client_input.rs` publishes |
| Privilege escalation       | Owner-only grant authority + CRDT-persisted grants       | `crates/session/shell/src/shell/state.rs::Workspace.grants` |
| Room takeover via replication | Governance written only by the room's authority, pinned from the invite signature, never from a first answer; one admission for every receiver | `crates/collab/tilo-peer-state/src/room/admit.rs::admit_room_changes` |
| Action forged by the workspace host | Source-device signature over the exact action + freshness window + replay window at the target broker | `tilo-delivery-protocol/src/peer_actions/signed.rs`, `tilo-action-broker/src/peer_action_broker.rs` |
| Traffic snoop              | QUIC/TLS end-to-end                                     | Iroh                               |
| Token forgery              | ed25519 signature + canonical payload + expiry          | `network/invite.rs`                |
| Host takeover by peer      | Physical-input priority + `Super+Esc` kill switch        | `network/input_firewall.rs`, `compositor/native.rs` |

## Where to read first

- `crates/packaging/launcher/src/{sandbox.rs, run.rs, capabilities.rs, landlock_shim.rs, seccomp.rs}` — the bwrap argv, grants, Landlock shim and seccomp profile choice.
- `crates/system/sandbox-core/src/{landlock.rs, seccomp.rs}` — Landlock ruleset construction and seccomp compilation.
- `crates/session/shell/src/compositor/bridge.rs` — the per-workspace Wayland bridge socket.
- `crates/session/shell/src/portal/{mod.rs, file_chooser.rs, screencast.rs, screencast_pipewire.rs, remote_desktop.rs, camera.rs}` — D-Bus impls.
- `crates/session/shell/src/shell/state.rs` — `Capability`, `GrantScope`, `Workspace.grants`.
- `crates/session/shell/src/network/invite.rs` — invite tokens.
- `crates/session/shell/src/network/input_firewall.rs` — the remote-input checks, with each denial proven through `State::review_remote_input` in its `production_path` tests.

### Display preview transactions from Settings

The Settings worker submits typed `Inspect`, `Identify`, `Apply`, `Keep` and `Revert`
requests through its existing AppSettings capability. This does not enlarge
AppSettings' generic write scope. A display delegate exists only for the exact
private SDK Settings registration and certified route; each call rechecks
admission, certificate validity and the original registration's live connection.
Off-thread work carries an explicit local-host origin or the original published
invocation UUID. The broker resolves a UUID only against its retained original
invocation on that exact worker connection, rechecks live custody and current
caller policy, and derives the caller's certificate itself. Unknown, cancelled,
completed or replaced-channel UUIDs never become local intent. Mutations require
the original Admin contract and the same person/device certificate as the worker;
a foreign workspace Admin or an Agent cannot borrow the worker's owner identity.
The exact certified Settings binary captures the published origin before starting
background work; physical UI intent uses its host-admitted local-person channel.
An equal certificate on a replacement connection cannot revive that delegate.
Public, foreign and agent channels do not acquire this private capability.

The native palette and authenticated owner control socket use the existing app
provider with a separate host-owned local-person invocation entrypoint. It reads
the protected device certificate and selected identity, checks current claimed
ownership and borrows the accepted room custody, retaining only a weak host
reference. The broker captures that exact certificate, room and installed policy
clock origin before asynchronous dispatch, and rechecks them for continuation.
Workspace Viewer/Editor roles remain unchanged; the action contract decides
Read/Write/Admin. Agent callers and supplied parent invocations cannot enter this
door. Network, MCP and application transports retain their original admission.
A host with no native-person service keeps its ordinary legacy actions without
original certificate evidence; an installed service that refuses never falls
back, and missing evidence cannot authorize a privileged continuation.

Inspection is read-only. Mutations additionally require the session's protected
claimed device owner, an exact match to its protected primary device certificate,
and a current accepted person policy granting Settings Admin. An enrolled room
borrows its native committed view; an ordinary desktop room borrows the accepted
host room policy, actual tree and protected writer. A changed owner/capability
vector, retired writer or stale native authority refuses the operation. UI role projection is not this backend
proof. Lost custody or a fenced room fails closed. The host reads the active
compositor's leased output snapshot, checks the proposed output set and supported
configuration, and captures the observed mode, scale and positions for rollback.
Keep and Revert name the host-created transaction ID and must still match the
connected output set. The existing compositor transaction watcher owns preview,
confirmation deadlines, rollback and KMS; the worker receives no DRM handles,
host paths or new filesystem write permissions.
### Native app action authority

Record participation and app management are separate admissions. A native
workspace launch remains `author` (Editor) or `read-only` (Viewer) for its
records. The launcher additionally mounts one host-selected app/person action
policy directory read-only at `/run/tilo/action-authority`; it never adds these
person capabilities to the app's sandbox grant file or changes record roles.
The SDK borrows this current person policy for Admin operations on the bound
app or its declared action. For other operations on that app or action, the
current policy may narrow an already allowed participation decision, never
upgrade a denied one. Other apps, workspace and record resources retain their
original participation. App, agent and token actors cannot borrow this person
authority.

Only accepted workspace-state publication exports policy. The export retains
matching person/membership policy and exact-app/action scopes, excludes host
paths, devices, network and other apps, and checks current native custody before
renewal. The existing launch supervisor holds the projection lease; the normal
idle deadline and durable writer renew it once per second while that supervisor
is live. An expired, future-dated, missing, malformed or foreign binding denies
Admin admission and any own-app permission requiring the current projection.
The maximum lease is three seconds: host death or failed file
replacement cannot preserve stale admission indefinitely. Atomic replacement
is visible through the read-only directory mount, and ordinary hosted model
pumps republish changed authority without forcing a redraw.

This projection is action discovery/admission, not privileged backend proof.
Host-mediated operations must independently authenticate the launcher-bound
package and the current person/owner policy at execution. A self-described
AppHost `app_id`, an SDK Owner string or possession of the projection cannot
approve root pairing, disk enrollment, host-shell grants or arbitrary execution.
