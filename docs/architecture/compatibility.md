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
  waits for a restart of the machine. The shell asks for it, and restarts the
  desktop to finish an update, at most every two days together
  (`shell::system_update`), so the updates of those days apply with one.
- Something that needs a newer system than this one (an app build, a
  collaborator's device) starts the system update at once
  (`tilo-deployment-update.service`): what applies live has applied when it
  returns, the desktop finishes the rest at the next moment the person is
  away, and a restart is asked for at once if one is needed.

### Live system updates

`tilo-live-update.service` (`tilo-extension live:system`,
`crates/system/extension-helper/src/live.rs`) runs after every staging, with
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
   two minutes, the shell starts `tilo-live-update-session.service`
   (`live:session`): it merges the whole update, restarts the changed session
   services and restarts every desktop in place through `tilo-session`
   (SIGUSR1): the login, the lock and the windows stay.
5. A restarted unit that fails, or a desktop that does not come back within
   20 s, puts the previous layer back, restarts the same parts again and
   marks the update failed; it then applies with the next restart of the
   machine, which the shell asks for.

The state the shell reads is `/run/tilo/live-update/state.json`. Layers
last until the next boot, which boots the staged deployment that holds them
all; every boot deletes them, and every apply rebuilds its layer from the
mounted image.

## One version per interface

| Interface | Version | Schema | Checked |
|---|---|---|---|
| App ↔ shell (shell-ui) | `ui::floating_surface::FLOATING_SURFACE_PROTOCOL_VERSION` | `crates/sdk/ui-core/interface.toml` | the app's first frame on its shell-ui connection; the launcher, from the manifest |
| App ↔ runtime (the session services the launcher connects) | `runtime_portal::RUNTIME_INTERFACE_VERSION` | `crates/runtime/runtime-portal/interface.toml` | the launcher, before it connects any service |
| Device ↔ device (handshake, relay frames, peer-sync messages) | `neighbour_wire::PEER_PROTOCOL_VERSION` | `crates/collab/neighbour-wire/interface.toml` | the first field of the certified handshake's `Hello` and `HelloAck` (relay sessions, compute, environments, the HTTPS carrier), and of the `IdentityHello` a device announces on a room's gossip topic, whose index never moves |
| Store package format (`.tapp`, its manifest, and the registry's snapshots, version indexes and channel authorizations) | `registry_types::PACKAGE_FORMAT_VERSION` (`tilo_app_package::TAPP_FORMAT_VERSION` is it) | `crates/packaging/tilo-app-package/interface.toml` | the archive header, when a package is read; a snapshot's `protocol`, when the catalogue is verified |

- There is no negotiation and no code for an older number. A side at another
  number says so and stops.
- The Rust runtime a native app links has no stable ABI, so a build names the
  exact runtime build it links (`package.runtime`), and the store fetches that
  runtime with the app.
- The image declares the versions it serves in
  `os/usr/share/tilo/system-interfaces.toml`, one per interface; every service
  an app can be handed belongs to the runtime interface unless it is one of its
  own. An app's manifest names the version it speaks of each service it uses
  (`[sandbox.services]`, `required@N` or `optional@N`); it runs only where every
  one is served exactly, optional ones included.
- `interface.toml` is written from the code, the way `tilo-app surface` writes
  an app's `surface.toml`: every type the interface carries, read from its
  sources with the serde attributes that shape the wire, and its Cap'n Proto
  schemas. `tools/policy/check_interfaces.py` fails when the file differs from
  the code (`check`, policy suite) and when it changed incompatibly since main
  without a higher version (`against`, t's `changes` check). An added optional
  field of a JSON message, an added Cap'n Proto struct field and a new type are
  compatible; every other change to what one side sends or reads is not.

## A mismatch is a state

The launcher never runs a build that speaks another version than the system
serves. The shell decides first what the person sees
(`shell::app_versions`); a launch from a terminal (`tilo-launcher
run-installed`, the PATH shims, `exec-installed`) does the same, saying so on
stderr: it updates a build that is behind (or recorded at an older package
format) and runs the new one, starts the system update for one that is
ahead, and exits 75 when the update is not there yet:

- **The app is behind** (built for older versions): its place says "Updating
  Mail…" while its update downloads, and it opens there once it fits. After a
  system update the restored windows of such apps do the same in their tabs.
- **No build fits yet** (its developer has not published one): "Waiting for an
  update from the developer". It does not start.
- **The app is ahead** (it needs a newer system): "Opens once Tilo has
  updated", and the system update starts.
- An app whose shell-ui hello names another version than its manifest is
  answered with the shell's version and closed, and the side behind updates.

Between devices (`shell::collaborator_versions`), a device at another peer
version is answered with this one's and admitted to nothing, so nothing it
sends arrives half-applied; each device keeps its own changes, and sync
resumes by itself once both speak the same version:

- **A collaborator is behind**: "Ana needs to update to keep working
  together".
- **This device is behind**: "Updating Tilo to keep working with Ana"; the
  system update starts at once and the restart is asked for as soon as it is
  staged. A browser page behind its host says "This page is out of date.
  Reload it to keep working together."

## User data survives

- **Resources** migrate with `#[resource(version, migrate)]`
  ([resources.md](resources.md) §6). A migration is one change on top of the
  document's history, so the document before it stays readable there: going
  back to the build that wrote it loses nothing, and that build refuses the
  migrated document instead of reading it as its own.
- **Other on-disk formats** are a `tilo_files::native::migration::Format`: a
  version, the oldest version still brought up, and one forward-only step per
  version. Loading an older file copies it to `<file>.v<N>` first, then
  replaces it with the migrated bytes; a newer file is refused untouched. Each
  step is tested against a file the previous version wrote, and only that.
  `tools/policy/check_format_migrations.py` keeps the chain from losing a step,
  so a machine that missed updates still loads. The shell's `state.json` is the
  first such format.
