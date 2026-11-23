# Compatibility

Tilo's parts change every few days, and compatibility with older versions is
not kept. Instead every device runs the newest version, automatically, and two
parts speak only when they speak the same version of the interface between
them. A mismatch is a state the person sees, with its way out, never undefined
behaviour. The one thing that survives every change is the person's data.

## Everyone on the newest version

- **Apps** (and the runtime builds they link) update in the background:
  `tilo-app-update.timer` runs `app update` hourly, except on a connection the
  owner marked metered. No restart: a running app keeps its build until it
  next starts.
- **The system image** is fetched, verified and staged by
  `bootc-fetch-apply-updates.timer`, at least daily, and then applied live,
  component by component, without a reboot (below). Only a kernel,
  bootloader or initramfs change, or an update that could not apply live,
  waits for a restart of the machine. The session asks for it, and restarts the
  desktop to finish an update, at most every two days together
  (`tilo_system_service::update`), so the updates of those days apply with one.
- Something that needs a newer system than this one (an app build, a
  member's device) starts the system update at once
  (`tilo-deployment-update.service`): what applies live has applied when it
  returns, the desktop finishes the rest at the next moment the person is
  away, and a restart is asked for at once if one is needed.

### Live system updates

`tilo-live-update.service` (`tilo-extension live:system`,
`crates/os/extension-helper/src/live.rs`) runs after every staging, with
the machinery `tilo device swap` uses on a development machine:

1. It opens the staged and the booted deployment read-only: on the sealed
   layout each is mounted from its composefs image with the fs-verity digest
   bootc reports, so it reads exactly the verified, signed image; on the
   ostree layout each is its deployment checkout.
2. It compares their `/usr` and classifies every difference by the staged
   image's `/usr/share/tilo/components.toml`: a system component (`vm`
   scope), a session component, the image's apps and runtime, the base OS,
   or what only a boot changes (`/usr/lib/modules`, firmware, bootloader,
   initramfs). `/usr/etc`, os-release and the rpm database stay as booted
   until the next boot.
3. It builds one system extension of everything but the session components
   (and the image's apps when `system-interfaces.toml` changed), in
   `/var/lib/tilo/live-update/layers`, linked from `/run/extensions`, merges
   it through `tilo-extension activate` (the only merger on the sealed
   image), and try-restarts the changed system components' units, system and
   user. Apps and the runtime need no restart. The greeter under a login
   runs its new build next time it starts.
   A newer image staged over one already applied live restarts only the
   components whose files differ from what is live, and the system layer
   keeps what the desktop already runs.
4. When part waits for the desktop and the screen is locked or blanked for
   two minutes, the session starts `tilo-live-update-session.service`
   (`live:session`): it merges the whole update, restarts the changed session
   services and restarts every desktop in place through `tilo-session`
   (SIGUSR1): the login and the lock stay, and so do the windows. A Tilo
   app's window is reopened by the new desktop in the tab it stood in. Every
   other app (one of the machine's own, anything the Tilo runtime does not
   draw) was started behind an app keeper (`tilo-app-keeper`) and keeps
   running: the keeper is that app's display and holds its windows while no
   desktop is there. The launcher starts the keeper inside the app's scope and
   the app as the keeper's child, and the keeper's directory under
   `$XDG_RUNTIME_DIR/tilo-session/kept/` records the launch. The desktop that
   ends leaves such an app untouched only when this is an in-place restart and
   its keeper answered the desktop in that run; a logout ends it with
   everything else. The desktop that follows takes each keeper back: it checks
   that the app's process is the one the keeper started, counts it as a launch
   of its own again (memory tiers, the pressure watch, its end report, ending
   it at logout), lets the saved tab of its app wait for its window instead of
   starting the app again, and attaches the keeper, which shows the windows'
   last pictures at once. What the app wrote goes on being kept as well: the
   keeper holds the newest 64 KiB of each of its two streams in a bounded file
   in that directory (`output.stdout`, `output.stderr`), and the desktop that
   follows reads it from the first line still held and keeps following it
   while the app is there. Nothing watches memory pressure in the seconds
   between the two desktops; the kernel's OOM kill of the app's group is the
   backstop, and the end is reported as a memory end from the group's events
   once the new desktop takes it back. An app that ended while no desktop was
   there is reported from the record its keeper left.
   A Windows program in a Wine capsule is one of these launches: the
   compatibility service has the desktop start its sandbox behind a keeper
   (`tilo-launcher windows-program`), and the service the new desktop starts
   lists it open again, can quit it, and reports its end. A Proton program is
   one too, on an X display of the keeper's own: UMU-Proton carries no Wayland
   driver, so the keeper gives its sandbox that display's socket and authority
   file and the program's window opens through Xwayland. A window of the
   Windows appliance (FreeRDP RemoteApp on such a display, or the SPICE viewer
   of a setup) is one too, without a Quit: it closes inside Windows. A keeper
   started by an older desktop that speaks another version of its interface
   cannot be shown: the new desktop counts it as a launch (tiers, memory watch, ended at
   logout), starts no second copy of its app, and offers to restart it, which
   ends the old app (process group asked first, then the launch's own scope)
   and starts it again. One part of that interface is not versioned and never
   changes, so this works between any two builds: the hello, the
   version-mismatch answer followed by `"holding"`, after which the open
   connection keeps the keeper from ending its app for want of a desktop, and
   the `app` file (pid and start time) in the keeper's directory
   (`crates/desktop/app-keeper/src/wire.rs`, "The part that never changes").
   Keepers from before that rule close the connection and end their app 5
   minutes after their desktop went.

   The tab a Windows program stood in opens it again after any start, not only
   after an in-place restart. The session keeps the window class a program
   showed under with its entry and the workspace whose tab it stood in
   (`tilo/windows-compat/window-identities.json`), and after a start asks the
   service as itself to open that entry there; the service checks the entry
   still exists and the workspace is one this desktop serves, then plans it as
   it plans a manager's launch. An entry removed meanwhile leaves the tab saying
   it is not installed.
5. A restarted unit that fails, or a desktop that does not become usable again
   within 60 s, puts the previous layer back, restarts the same parts again and
   marks the update failed; it then applies with the next restart of the
   machine, which the session asks for. "Usable" is not "a compositor process
   is running": the session publishes a record once every enabled output has
   shown its first frame — the lock surface instead, when it starts locked —
   and the windows this device left open are being asked for. That record names
   the process and its kernel start time, so one a previous run left is never
   taken for this one's. A desktop that comes back and crashes within a few
   seconds still counts as not back.

The state the session reads is `/run/tilo/live-update/state.json`. A layer is
immutable once merged, and every apply builds a fresh one from the mounted
image instead of rewriting one. A layer is removed as soon as nothing mounts it
any more: an app started before the update keeps the merged `/usr` it started
with in its own mount namespace, and that mount still reads the layer, so the
apply keeps it. It goes when the sandbox view built over it is retired, or
with the next apply after its last reader ended. Every boot deletes them all,
because it boots the staged deployment that holds them.

## One version per interface

| Interface | Version | Schema | Checked |
|---|---|---|---|
| App ↔ shell (shell-ui) | `ui::floating_surface::FLOATING_SURFACE_PROTOCOL_VERSION` | `crates/ui/ui-core/interface.toml` | the app's first frame on the shell-ui connection the launcher gives every window |
| App ↔ runtime (the session services the launcher connects) | `runtime_portal::RUNTIME_INTERFACE_VERSION`, served at every version from `runtime_portal::OLDEST_RUNTIME_INTERFACE_VERSION` to it | `crates/core/runtime-portal/interface.toml` | the launcher, before it connects any service |
| Device ↔ device (handshake, relay frames, peer-sync messages) | `neighbour_wire::PEER_PROTOCOL_VERSION`, working with the versions `neighbour_wire::works_with_peer_version` names (below) | `crates/core/neighbour-wire/interface.toml` | the first field of the certified handshake's `Hello` and `HelloAck` (server sessions, compute, environments, the HTTPS carrier), and of the `IdentityHello` a device announces on a workspace, whose index never moves |
| Session ↔ app keeper (the control socket of a kept app) | `tilo_app_keeper::wire::INTERFACE_VERSION` | `crates/desktop/app-keeper/interface.toml` | the session's `Hello`, the first message on every connection: a keeper at another version (one started by an older session build, still running its app) answers `VersionMismatch`, then `"holding"`, keeps displaying, and holds its app while that connection stays open (the unversioned part, never changed) |
| Session ↔ a tilolang process (the process link) | `runtime_portal::packet::VERSION` | the framing itself (`crates/core/runtime-portal/src/packet.rs`): there is no schema file, because the packet is the message | every packet, in its first bytes: a side built before the link carried a version, or at another one, refuses the packet naming both versions, so a message that changed is a build mismatch a person is told, never bytes read as another message |
| Store package format (`.tapp`, its manifest, and the registry's snapshots, version indexes and channel authorizations) | `registry_types::PACKAGE_FORMAT_VERSION` (`tilo_app_package::TAPP_FORMAT_VERSION` is it) | `crates/core/tilo-app-package/interface.toml` | the archive header, when a package is read; a snapshot's `protocol`, when the catalogue is verified |

- A reader keeps every format it understands: `tilo_app_package::tapp::READABLE_FORMATS`
  for the archive and `registry_types::READABLE_CATALOGUE_FORMATS` for the
  registry's catalogue. A format that changes a layout adds its reader there
  before anything ships at it, so a newer format never stops a reader from
  reading the ones it already knew. The catalogue's signed form is positional,
  so a format that adds to it (format 4 added each version's surface) cannot
  be read beside the one before: that format is dropped, and a registry
  publishes its catalogue again. There is no negotiation and no code for a
  number no reader has: a side at a newer format than the reader says so and
  stops, naming the update that reads it ("update Tilo to read this
  catalogue"); a reader that no longer understands an older format says the
  writer has to publish at the newest one.
- Packages are read at every package format that shipped
  (`tilo_app_package::tapp::READABLE_FORMATS`), apps and environment images
  (`env-runtime`, `tool`) alike: a reader installs every package format it can
  verify, and publishing at a new format moves the writer, not the packages
  already published. An environment names its images by package id, the hash
  of a signed root that includes the package format, so an image cannot be
  rewritten at a new format without becoming another image. A format that
  changes the archive, signed root or chunk list layout adds its reader there
  before anything ships at it; until then it is a catalogue change and one
  package reader reads both, as formats 2 and 4 were.
- The Rust runtime a native app links has no stable ABI, so a build names the
  exact runtime build it links (`package.runtime`), and the store fetches that
  runtime with the app.
- The image declares the versions it serves in
  `os/usr/share/tilo/system-interfaces.toml`, one per interface and, for the
  runtime interface, the oldest one it still serves (`[runtime] oldest`, the
  code's `runtime_portal::OLDEST_RUNTIME_INTERFACE_VERSION`); every service an
  app can be handed belongs to the runtime interface unless it is one of its
  own. An app's manifest names the version it speaks of each service it uses
  (`[sandbox.services]`, `required@N` or `optional@N`); it runs where every one
  is served, a runtime service at any version from the oldest to the current
  one, every other interface exactly, optional ones included. A version bump
  leaves the oldest where it is, so an app built against it keeps running; it
  moves by hand only when the change is breaking, to the version before the
  change, once this build no longer reads or writes that version's wire.
- `interface.toml` is written from the code, the way `tilolang surface` writes
  an app's `surface.toml`: every type the interface carries, read from its
  sources with the serde attributes that shape the wire.
  A list of words both sides hold alike though no type carries it is recorded
  there too, a line a word (`words` in the file's header): the keywords a
  declared shape may use travel inside a string, so the runtime interface
  lists them, and a word added or gone needs a higher version.
  `tools/policy/check_interfaces.py` fails when the file, its version, the
  versions the image serves or an app's manifest differ from the code (`check`,
  in the pre-commit hook and the policy suite) and when the interface changed
  incompatibly since main without a higher version (`against`). An added
  optional field of a JSON message, a new type, and a postcard field under
  another name that keeps its position and type are compatible; every other
  change to what one side sends or reads is not.

## A mismatch is a state

The launcher never runs a build that speaks a version the system does not
serve: a runtime service outside its range, or another interface at another
version. The store judges each app against the interfaces the system declares
(`package_store::interfaces::unmet_at`) and the session shows what that says of a
place; a launch from a terminal (`tilo-launcher
run-installed`, the PATH shims, `exec-installed`) does the same, saying so on
stderr: it updates a build that is behind (or recorded at a package format
this build does not read) and runs the new one, starts the system update for
one that is ahead, and exits 75 when the update is not there yet:

- **The app is behind** (built for older versions): its place says "Updating
  Mail…" while its update downloads, and it opens there once it fits. After a
  system update the restored windows of such apps do the same in their tabs.
- **No build fits yet** (its developer has not published one): "Waiting for an
  update from the developer". It does not start.
- **The app is ahead** (it needs a newer system): "Opens once Tilo has
  updated", and the system update starts.
- An app whose shell-ui hello names another version than the session's is
  answered with the session's version and closed, and the side behind updates.

Between devices (`neighbour_wire::PEER_PROTOCOL_VERSION`), a device at a peer
version this one does not work with (below, "Two versions at once") is
answered with this one's and admitted to nothing, so nothing it sends arrives
half-applied; each device keeps its own changes, and sync resumes by itself
once both speak versions that work together:

- **A member is behind**: "Ana needs to update to keep working
  together".
- **This device is behind**: "Updating Tilo to keep working with Ana"; the
  system update starts at once and the restart is asked for as soon as it is
  staged. A browser page behind its host says "This page is out of date.
  Reload it to keep working together."

What is added to an interface within one version is something a side built
before it reads as what it read before. The handshake's refusal that says why
(`tilo_server_protocol::auth::HelloRefused`, added at peer version 53) stands
behind an empty frame, which no handshake answer is: a device built before
refuses that frame and reads the refusal as it always read one, a hello nobody
answered, and a server built before sends none, which a newer device reads the
same way.

## Two versions at once

A person's servers update one at a time (each restarts into an update only
while it holds its person's admin workspace's update lease,
`tilo_server::rollout`), so while an update rolls out some of them are on it and
some on the build before, and their devices and the other people's servers
talk to both. Between devices this is the one place two versions work together, and
the window is exactly one peer version wide:

- A device works with a peer at its own version, at the next one, and at the
  one before while it still speaks that one's main flows
  (`neighbour_wire::works_with_peer_version`,
  `neighbour_wire::OLDEST_PEER_PROTOCOL_VERSION`). Every handshake and
  identity announcement checks that window and nothing else.
- The main flows are frozen at the oldest version a build works with:
  the handshake, sync (anti-entropy's ask and answer), a lease claim, the
  identity announcement, and the connection protocols a server is dialled on.
  Their frames are kept in
  `crates/core/neighbour-wire/previous-peer-frames.txt`; tilo-peer-state's
  lease test reads the claim with today's types, writes it back, and fails on
  any byte that differs. The frames of the session transport that carried a
  device's workspaces, subscription and delta flow went with the transport.
- Every message a device takes in, from a peer, a server or its own store, is
  read by one reader, `neighbour_wire::NeighbourMsg::decode`, and the frames
  that carry one end with it or read it through that reader. A document write
  kept before writes carried their reader set ends before it; that reader
  reads it as naming none.
- A version bump keeps the window when the frozen frames still pass: write the
  frames of the version being left into the file first (with its number), then
  move `PEER_PROTOCOL_VERSION`, and set `OLDEST_PEER_PROTOCOL_VERSION` to the
  version left. A change the frozen frames cannot pass breaks the window: it
  sets `OLDEST_PEER_PROTOCOL_VERSION` to the new version and the frames to its
  own, and the servers running the build before it are told to update, as any
  device behind is. A rollout across it then serves each server's workspaces
  from the person's other servers only once those updated too.
- Neither side sends the other what the older one cannot read: what a version
  adds stays within messages the older one ignores or never receives (a new
  frame only answers a request only the newer side sends).
- No reader of anything older is kept, and none of a stored layout: a resource
  migrated by the newer build is refused by the older one, as a migrated
  document always is ("Stored data"), until that server updates in its turn.

## Stored data

Stored formats change with the code. A layout change brings up the layout the
build before it wrote, with one step ([`Format`'s steps](../../crates/platform/tilo-files/src/native/migration.rs)),
and deletes the step that read the one before that: every device that updates
holds files the build before wrote, and a build that refused them would leave
the device without what it kept (a desktop that refused its own workspaces'
entries opened none of them, and kept nothing from then on). No older reader is
kept; a file in a layout this build does not read is refused, saying which
version it holds, and nothing guesses at its bytes.

- **A version number always means the same bytes.** A stored format names its
  version and keeps the bytes that version wrote beside it
  (`golden/<name>/v<version>.hex`); a test reads them with today's types and
  writes them back exactly, so a field added to, removed from or moved in a
  stored type fails a test instead of being noticed when a machine's file stops
  opening. A layout change is a new version with its own bytes.
  `tools/policy/check_format_migrations.py` runs these checks.
- **What is written beside a format is part of its version.** A journal
  appended beside a snapshot, or a frame kept as a file, is a Rust type
  serialized to disk that the golden bytes of the snapshot never see. Each such
  type is listed in `tools/policy/stored_layouts.toml` under the format whose
  version covers it, with a digest of its definition, and a change to it is a
  new version of that format. A snapshot that is brought up takes the journal
  its build appended under that build's rules first, and is written again in
  this build's version before anything is appended to it: a journal is a delta
  of the bytes beside it. A whole journal entry that does not read is refused,
  never skipped. Skipping it once dropped every change a device made since its
  last snapshot, the workspace's governance with them. A break named in
  `tools/policy/wire_enum_breaks.toml` is no waiver for a type listed as
  stored.
- **A resource carries its layout version in its document.** A resource that
  changes its layout adds a `#[resource(version, migrate)]` step
  ([resources.md](resources.md) §6). The step is one change on top of the
  document's history, so it reaches every member with the rest of the record,
  and a device at the older build refuses the migrated document instead of
  reading it as its own. A migration may refuse a document it cannot read, with
  a reason: Notes says of a notebook written before notes were kept as a record
  that it is not migrated, rather than opening it empty.
- **Signed bytes are never rewritten.** An invitation, a delegation and the
  rows a workspace's governance hashes are kept as their signer wrote them,
  since their ids and signatures are over those bytes.
- **A file in an unknown layout says so.** A device's state of a workspace
  (`unsupported durable base version`), an invocation journal, saved channel
  state and a notebook each name the versions they read and refuse every other,
  saying which version the file holds. Saved channel state says the most: the
  bytes are authenticated and this device's own, but no build reaches their
  layout, so they are dropped, the device asks to join the channel again, and
  the keys only it held are gone — said as an error where it happens.
