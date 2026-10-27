# The sandbox: what your manifest is granting

Every Tilo package is launched inside a nested sandbox with three
independent enforcement layers. This document maps the manifest's
`[sandbox.*]` blocks onto the concrete mechanisms.

## The layers

```text
                      ┌────────────────────────────────────────┐
                      │        tilo-install run <pkg-id>         │
                      └────────────────────┬───────────────────┘
                                           │ exec
                                           ▼
                      ┌────────────────────────────────────────┐
                      │         tilo-launcher run …             │
                      │  (fork, open service sockets, plan     │
                      │   Landlock ruleset, build bwrap argv)  │
                      └────────────────────┬───────────────────┘
                                           │ exec  bwrap …
                                           ▼
        ╭──────────────── Layer 1: bwrap namespaces ────────────────╮
        │  --unshare-all  (user, pid, uts, ipc, mount, [+ net])      │
        │  --ro-bind payload → /var/tilo-apps/<pkg-id>                │
        │  --ro-bind runtime → /usr/lib/tilo-runtime/<ver>            │
        │  --tmpfs /tmp   --proc /proc   --dev /dev                  │
        │  --setenv TILO_PACKAGE_ID=…  TILO_SVC_*_FD=…                 │
        │  --file <fd>   /run/tilo/services/<name>.sock              │
        │  --seccomp <fd>  (Layer 2 filter installed by bwrap)      │
        │                                                            │
        │  --ro-bind-try × system read profile (fixed, see below)    │
        │  --bind <data home>/tilo-rooms/<room>/<pkg> → /run/tilo/app  │
        │  --setenv HOME=/run/tilo/app  XDG_*=/run/tilo/app/…          │
        │                                                            │
        │       ╭───────── Layer 3: sandbox-exec shim ─────────╮    │
        │       │  install Landlock v3 ruleset:                 │    │
        │       │   • ro rules for the system read profile      │    │
        │       │   • rw rules for /run/tilo/app                 │    │
        │       │   • ro/rw rules for granted capabilities      │    │
        │       │   • network egress restriction if net = off   │    │
        │       │   • port allowlist scoped to [sandbox.net]    │    │
        │       │  execvp entrypoint                            │    │
        │       ╰───────────────────────────────────────────────╯    │
        ╰────────────────────────────────────────────────────────────╯
```

Each layer is defence-in-depth against the layer above. A kernel bug in
namespace isolation still leaves seccomp and Landlock; a broken seccomp
policy still leaves Landlock; and vice versa. The `tilo-hello` demo below
proves both bwrap and Landlock are enforcing in a real run.

## Layer 1: bubblewrap

Driven entirely by `[sandbox]` fields.

### `namespaces`

`--unshare-all` is always applied; the `namespaces` list is currently
informational (the launcher unshares the full set by default). Include
the ones you want unshared — omitting `net` in a future launcher
would leave the network namespace shared.

### FS bind mounts

**Your manifest names no filesystem path.** There is no `[sandbox.fs]`
block; a package that ships one is refused, at install and at launch,
with a message naming the package and the offending entry.

That is not a tightening of an existing list — it is the removal of a
mechanism. The launcher used to bind whatever the manifest named, source
and destination, read-write for every `write` entry, after expanding
`~/`, `$XDG_*` and `<scope=current>` with no `..` handling and no
allowlist. `write = ["/"]` was a valid manifest. Signing did not help:
it proves who wrote the manifest, not that what it asks for is
reasonable.

What the launcher composes instead:

- `payload/` → `/var/tilo-apps/<package-id>/` (read-only). Your entrypoint
  is `/var/tilo-apps/<package-id>/<entrypoint.bin>`.
- Runtime bundle → `/usr/lib/tilo-runtime/<version>/` (read-only), plus a
  `LD_LIBRARY_PATH=/usr/lib/tilo-runtime/<version>/lib` env var, plus
  re-binds of the loader at its canonical path
  (`/lib64/ld-linux-x86-64.so.2` etc.) so the kernel's `INTERP` lookup
  works.
- **The system read profile** → `--ro-bind-try <path> <path>` for each
  entry, plus `--dev-bind-try` for the GPU nodes. Fixed, identical for
  every package, defined in `launcher::profile`: the loader and C
  library, fonts, the GPU loader manifests, `/sys`, the CA bundle,
  timezone data, `/etc/machine-id`. Two rules govern what may be added:
  it carries no user content, and every GUI app needs it. The resolver
  files (`/etc/resolv.conf`, `/etc/hosts`) are added only for a launch
  that shares the host network — an app under a host-scoped grant
  resolves through the egress gate instead.
- **Your directory** → `--bind <data home>/tilo-rooms/<room>/<package>
  /run/tilo/app`, read-write. See below.
- `--tmpfs /tmp`, `--proc /proc`, `--dev /dev` always.
- Anything else is a **capability** the user grants at runtime
  (`collab_types::caps`), bound with `--ro-bind-try` / `--bind-try` when
  and only when the grant is live. A grant never reaches Tilo's private
  configuration (`$XDG_CONFIG_HOME/tilo`: identity secrets, vault keys,
  trust and revocation lists): a grant inside it is refused, and one
  containing it shows it as an empty read-only directory
  (`launcher::capabilities`, "Tilo's private stores are never inside a
  grant").

### Your directory

The launcher gives every app exactly one writable directory. It decides
the location, creates it, and points the environment inside it:

| Variable          | Value                            |
|-------------------|----------------------------------|
| `TILO_APP_DIR`     | `/run/tilo/app`                   |
| `HOME`            | `/run/tilo/app`                   |
| `XDG_DATA_HOME`   | `/run/tilo/app/.local/share`      |
| `XDG_STATE_HOME`  | `/run/tilo/app/.local/state`      |
| `XDG_CONFIG_HOME` | `/run/tilo/app/.config`           |
| `XDG_CACHE_HOME`  | `/run/tilo/app/.cache`            |

So `cli::standalone_state_dir`, `dirs::home_dir()` and a hard-coded
`~/.local/share/<pkg>` all land inside with no change to your app.

On the host it is `<data home>/tilo-rooms/<room>/<package>`, where
`<room>` is the workspace the app was launched into. **Per-room
partitioning falls out of the location**: two rooms are two directories,
so an app opened in one room cannot read what it wrote in another, and
there is no manifest field with which to opt out. A launch with no
workspace (`tilo-launcher run` from a shell, the CLI shim, a test) gets
the `personal` room, which is a room like any other and not a bypass.
`docs/architecture/rooms.md` — "an app opened in a room starts empty" — is this
sentence, at the filesystem layer.

`crates/packaging/launcher/tests/room_confinement.rs` proves it by
reading: an app writes a record in room A and gets `ENOENT` for it in
room B, under `bwrap`.

### Portals

If any `[sandbox.portals]` entry is `"via-host"` AND the D-Bus session bus
address is `unix:path=<path>` (the common case), the launcher does:

```
--ro-bind <bus-socket-path> <bus-socket-path>
--setenv  DBUS_SESSION_BUS_ADDRESS <original-address>
```

Otherwise the bus is NOT in the sandbox, and your `ashpd` calls fail as
they would on a system without a session bus. Fall back to a local flow.

Portal names the launcher understands today:

- `file-chooser` — Open / Save dialogs.
- `screencast` — start a PipeWire capture session.
- `notifications` — desktop notifications.
- `secret` — retrieve the per-application secret from the portal keyring.
- `open-uri` — hand a URL back to the host browser / mailto handler.

### Service sockets

For every `[sandbox.services]` entry the launcher opens a connected Unix
socket and passes the fd via bwrap's `--file <host-fd> <in-sandbox-path>`,
plus a `TILO_SVC_<NAME>_FD=<child-fd>` env var. Your SDK code reads
`TILO_SVC_COLLAB_FD` (or whatever) and takes it from there — see
[`runtime.md`](./runtime.md#portal--service-fds).

If a service is `required` and the launcher can't resolve its
socket, launch fails. If it is `optional`, the launcher warns and does
not pass an fd; your app then sees `NotAvailable::ServiceFdMissing`.

## Layer 2: seccomp

Driven by `[sandbox].seccomp`. The only shipped profile is `tilo-default`,
which blocks a small ring of syscalls (mount ops, kernel-module load, etc.)
that a normal Rust GUI app has no reason to make. The seccomp filter is
compiled by the launcher and installed via bwrap's `--seccomp <fd>` before
the child execs.

## Layer 3: Landlock

Driven by `[sandbox].landlock` and `[sandbox.net]`. The `sandbox-exec`
shim (the launcher itself, invoked recursively) installs the Landlock
ruleset inside the bwrap namespace before `execvp`ing the app.

Rules are derived from the *same* lists the binds came from, so the two
layers cannot disagree about what is reachable — a bind the ruleset does
not name is visible and returns `EACCES` on every open, which reads from
inside the app as a corrupted system rather than as a policy:

- Each system-read-profile entry → RO access rights on that path.
- `/run/tilo/app` → RW access rights.
- Each granted capability → RO or RW per the grant's verb.
- `[sandbox.net] mode = "off"` → all outbound connections denied.
- `[sandbox.net] mode = "allowlist"` → the app is pinned to the egress
  gate's loopback port. Landlock has no address vocabulary at all, so host
  filtering happens at the gate; see the note below.
- `landlock = "strict"` — abort on Landlock unavailable / partially-
  enforced. `landlock = "lax"` — warn but continue. `landlock = "off"` —
  skip the shim.

You can see the landlock enforcement live in `tilo-hello`'s probes:

```
== sandbox probes ==
  read(        /proc/self/status): permission denied — blocked (bwrap or Landlock)
  read(              /etc/passwd): entity not found — blocked (bwrap or Landlock)
  stat(                     /tmp): OK
  stat(            /tilo-launcher): OK
  write(/tmp/tilo-hello-probe): OK
  write(/var/tilo-apps/probe): permission denied — blocked (bwrap or Landlock)
```

No manifest declares filesystem paths, so Landlock denies everything
outside the launcher-provided base: the payload, the runtime, the system
read profile, `/tmp`, and the app's own `/run/tilo/app`.

Note the launcher's log line:

```
tilo-sandbox-exec: landlock: partially enforced (kernel ABI dropped some rights) (12 ro, 2 rw, net_restricted=true)
```

"partially enforced" indicates that the kernel supports fewer Landlock rights
than the profile requests — Landlock supports up to ABI v5 today, and older
kernel ABIs silently drop rights they do not understand.
The launcher records the count of successful rules and continues. On a
kernel with Landlock v3+ the enforcement is total.

### Hostname allowlist

`[sandbox.net] mode = "allowlist"` with `hosts = ["api.example.com"]` leaves
the network namespace **empty** and gives the app one way out: a Unix socket
the launcher holds, speaking SOCKS5 and HTTP `CONNECT`. The allowlist is
matched against the **name** the app asks for, before that name is resolved,
and resolution then happens on the launcher's side per connection. See
`crates/packaging/launcher/src/netgate.rs` for the design.

Three consequences worth knowing before you write networking code:

- **Your app has no resolver.** The namespace is empty, so a client that
  resolves a name itself fails at resolution. Use the proxy environment the
  launcher sets (`HTTPS_PROXY`, `ALL_PROXY`) — `socks5h`, not `socks5`, is
  what sends the hostname rather than an address.
- **An address literal never satisfies a hostname grant.** Asking for
  `93.184.216.34` when you were granted `example.com` is refused, even when
  that is the address the name resolves to. "This address is currently that
  name" is not something the gate can verify, and accepting it would make
  every name-scoped grant bypassable by resolving first. Bake hostnames,
  not IPs.
- **Device-local counts as network.** A granted name that resolves to this
  machine — loopback, link-local, or the machine's own LAN address — is
  dropped unless the grant names that host exactly. An app denied the
  network reaches no local service at all, because the namespace is empty
  and the sandbox's `127.0.0.1` is its own.

A refused destination answers SOCKS5 `HOST_UNREACHABLE` / HTTP `502`, the
same as a host that is unreachable. That is deliberate: handle it
as "offline", not as "ask the user to grant something".

### Denial is an absence, not a refusal

Where a denial can be expressed as a state your app already handles, it is:

| you tried | you get |
|---|---|
| a workspace path you were not granted | `ENOENT`, and it is not in the directory listing |
| writing a path you may only read | `EROFS` |
| connecting to a host you were not granted | unreachable |
| a device you were not granted | the device node is not in your namespace |

So `/var/workspace` is a **filtered view**: an app that enumerates it sees
exactly what it was granted, as an ordinary directory. There is no entry
that appears and then fails, and no errno that distinguishes "hidden" from
"not there" — `crates/system/fs/tests/integration_test.rs` compares the two for every
operation. Nothing here is a reason to prompt the user; if a path is not
there, treat it as not there.

### Read-only participation

An app can be granted read participation in a space: it sees shared
documents and cannot originate a change to one. If that is your launch:

- `TILO_PARTICIPATION=read-only` is set. Render a viewer.
- **No identity broker is handed over**, so `TILO_IDENTITY_BROKER_FD` is unset
  and `runtime-identity::load()` finds nothing. You hold no signing authority, so
  there is no authored change for anything to reject.
- The workspace is bound read-only and served read-only, and the document
  control socket refuses edits with the read-only-filesystem message.

An app that no workspace-scoped capability names gets neither variable and
launches as it always did.

## What gets into your process

This list is closed, and closed in the enforcing sense: the launcher passes
`--clearenv` and then puts back exactly what is below. Nothing of the
compositor's own environment reaches you — not its Wayland socket, not its
identity path, not an `SSH_AUTH_SOCK` or a `DBUS_SESSION_BUS_ADDRESS` you
were not given on purpose. If you need something that is not here, it is a
capability question, not an environment one.

After all three layers, your `main()` sees:

- `argv[0]` = `/var/tilo-apps/<pkg-id>/<entrypoint.bin>`.
- CWD = your payload root inside the sandbox.
- `PATH=/usr/local/bin:/usr/bin:/bin` — the sandbox's, not the user's. A
  session `PATH` names host directories that are not in your namespace, and
  could name a writable one of the user's.
- The user's locale and clock, when their session set them: `LANG`,
  `LANGUAGE`, the `LC_*` set, and `TZ`. Plus `TERM` / `COLORTERM` for a CLI
  entrypoint. These describe the person rather than the session, which is
  why they are the one thing carried across rather than constructed.
- `HOME=/run/tilo/app`, and `XDG_DATA_HOME` / `XDG_STATE_HOME` /
  `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` inside it. `TILO_APP_DIR` names the
  same directory explicitly.
- `LD_LIBRARY_PATH=/usr/lib/tilo-runtime/<version>/lib`.
- `TILO_PACKAGE_ID=<your reverse-DNS id>`.
- `TILO_SVC_<NAME>_FD=<fd>` for every service the launcher opened.
- `DBUS_SESSION_BUS_ADDRESS` (only if a portal is `via-host`).
- `TILO_IDENTITY_BROKER_FD` — one inherited, connected signing capability,
  unless you are a read-only participant. It exposes public metadata and a
  bounded sign operation, never a key or host path.
- `TILO_PARTICIPATION` — `read-only` or `author`, when a workspace-scoped
  capability names you. Absent otherwise.
- `HTTPS_PROXY` / `ALL_PROXY` / `TILO_NET_GATE` — only when your egress is
  filtered to a host allowlist. On macOS the proxy variables name the gate
  and `TILO_NET_GATE` is absent.
- `TILO_WORKSPACE_ID` — the workspace the compositor opened you into, as a
  UUID. Two things read it: `runtime::is_tilo_native()` only asks whether it
  is set, and the UI toolkit uses the value as the collaboration cluster id.
  Apps opened into the same workspace exchange CRDT deltas; apps in
  different workspaces are different rooms and never see each other's
  traffic. It names a room, not a permission — see `--capabilities` for
  what an app is actually allowed to touch.

  The value is the workspace's durable **room id**: a random v4 uuid minted
  once when the workspace is created, stored with it, and unchanged across
  restarts. It is deliberately not derived from the compositor's local
  workspace number, which means something different on every machine — a
  peer invited to the workspace adopts this same uuid, so their apps and
  yours address one cluster and your edits reach each other. Because
  `is_tilo_native()` now
  answers `true` inside a GUI launch, `cli::workspace_root()` resolves to
  `/var/workspace` there instead of the standalone
  `~/.local/share/<pkg-id>/` fallback; an app that writes through it needs
  a capability on the path it writes to.

## macOS: App Sandbox or Seatbelt

On stock macOS there is no bubblewrap, seccomp or Landlock. The macOS
launcher (`crates/packaging/launcher-macos`) still runs every app
sandboxed, and it picks one of two mechanisms from the app's code
signature. The registry check above (a signed index verified against the
Ed25519 root, [`trust.md`](./trust.md)) comes first in both cases. An Apple
signature never replaces it.

| The executable's signature | Launched under |
| --- | --- |
| Valid under strict validation, Apple Developer ID Application (or the configured team), notarized, entitlements include `com.apple.security.app-sandbox = true` | **App Sandbox**: exec'd directly, and the kernel enforces the sandbox its entitlements describe |
| Anything else: unsigned, ad-hoc, invalid or tampered, another signer, not notarized, or no App Sandbox entitlement | **Seatbelt**: exec'd under a generated deny-default profile |

The check fails closed. If the signature cannot be read, or if any
Security.framework call fails, the app goes to Seatbelt. An ad-hoc
signature's entitlements are only the payload's own claim, so they earn
nothing. An unsigned binary on Apple silicon is ad-hoc signed before exec,
because the CPU refuses to run unsigned code, and it still goes to Seatbelt.

**Closing the gap between check and exec.** A file can be swapped between
inspection and exec. So an App Sandbox app is spawned suspended, and the
*running* image is checked again: trusted identity, not ad-hoc,
entitlement present. It is resumed only if every check passes, and
otherwise killed before it runs an instruction. Notarization is checked
on the file only. It says nothing about whether the kernel enforces the
sandbox.

**The Seatbelt profile** is rendered from the same inputs as the Linux
layers: the per-room app directory, the user's `CapabilityPlan` and the
resolved `EgressDecision`. Paths travel as `sandbox-exec -D` parameters
and are never spliced into the profile text. Every path is canonicalised,
because Seatbelt matches real paths (`/tmp` is `/private/tmp`).

| Grant | Rule |
| --- | --- |
| Baseline | Apple's `system.sb` (dyld, `/System`, `/usr/lib`, `/usr/share`, logging, `cfprefsd`), plus `(system-graphics)` for Metal |
| GUI | Fixed Mach services for WindowServer, the window manager, Dock, LaunchServices, pasteboard, input methods, HID, fonts and icons; `/Library/Fonts`; read-only AppKit preference domains |
| Payload | Read, map-executable and exec. Never writable, even under a broader user grant |
| Runtime dylibs | Read and map-executable |
| Container | Read and write, never exec: `~/Library/Application Support/Tilo/tilo-rooms/<room>/<package>`, which is also `HOME`, `XDG_*` and `TMPDIR` |
| Shader cache | Read and write, never exec: the app's own directory below the per-user cache directory, `…/C/so.tilo+<room>+<package>`. The launcher sets `DIRHELPER_USER_DIR_SUFFIX` so `confstr(_CS_DARWIN_USER_CACHE_DIR)` names it, and Metal keeps compiled shaders there across launches. No app reaches another's |
| User grants | `ro_paths` read, `rw_paths` read and write |
| Tilo's private stores | Denied outright after every grant and picked path: `$XDG_CONFIG_HOME/tilo`, `~/.config/tilo`, `$TILO_SECRETS_DIR`. Every directory and symlink on a store's path is denied writes (no rename, removal or repoint), so the store cannot be moved aside and replaced. The plan is confined as on Linux first: a grant inside a store is dropped, a store a grant reaches is created before launch |
| Picked files | Read and write, never exec, of exactly what the person picked in a chooser the runtime showed (below) |
| Camera, microphone | No device and no TCC service; frames and samples arrive over an inherited socket from the runtime (below) |
| Network: off | No sockets beyond `AF_UNIX` creation. No connect or bind, IP or filesystem |
| Network: allowlist | Connect to one loopback port only: the egress gate the launcher runs for this launch, named by `HTTP(S)_PROXY` and `ALL_PROXY` |
| Network: host | IP in and out, the system resolver, `trustd`. Filesystem sockets such as an `ssh-agent` stay closed |
| Service descriptors | Inherited from the runtime at fixed numbers. Every other descriptor is closed by `POSIX_SPAWN_CLOEXEC_DEFAULT` |

**Choosing files.** A panel the app opened itself would run inside its
sandbox: it would show the disk, and the app could then open nothing it
picked. So `tilo_dialogs::system_picker()` in a Seatbelt app asks the
runtime instead, over a socket it inherits (`TILO_FILE_PICKER_FD`). The
runtime shows `NSOpenPanel` or `NSSavePanel` in a helper process of its own,
outside the sandbox, and answers each picked path with a sandbox extension:
the kernel-signed token App Sandbox's own panels hand out. The app consumes
it, and the profile's one extension rule then lets that path, and nothing
else, through for the rest of the process's life. The app says what kind of
chooser it wants and never names a path. A Save destination is created empty
before it is granted, since only an existing file can be. It is granted
with exactly one sibling, `.<name>.tilo-save`
(`tilo_dialogs::portal::save_sibling`), so an app saves atomically by
writing that and renaming it over the destination
(`tilo_dialogs::portal::replace_saved`); nothing else in the folder is
writable, so an app that picks its own temporary name cannot save that way.
A grant does not outlive the process.

**Camera and microphone.** The runtime, which holds the TCC permission,
captures on the app's behalf. The app inherits a control socket
(`TILO_SVC_CAPTURE_FD`); `tilo_capture::VideoCapture::open_camera` and
`AudioCapture::open_microphone` use it by themselves in such an app, so
its code is the same everywhere. Each request sends one end of a fresh
socket pair with it, and the runtime answers on that end, granted or
refused, then streams RGBA frames or `f32` samples until the app drops the
capture or the grant goes (`tilo_capture::broker`). The app names a device
and a format, never a device handle. What the runtime allows:

1. Only a device the manifest declares: `[sandbox.portals] camera =
   "via-host"` or `microphone = "via-host"`. Anything else is refused
   without asking anyone.
2. The person's decision for this app in this workspace, kept in the
   runtime profile (`state/device-grants`, `tilo_capture::grants`). The
   first request asks them in a native alert the runtime shows outside the
   sandbox, in their language (the runtime's Fluent catalogue), with
   "Don't Allow" as the default; the answer, either way, is kept.
3. A revoked grant ends a running capture within a frame and asks again
   next time. Every decision is a resource with a `revoke` action on the
   profile's `console.sock`, the door a person's assistive tool, a script
   or an agent uses: `tilo-control --socket <profile>/console.sock actions
   list`, then `actions invoke runtime revoke@device-grant:<device>:<room>:<app>`.
   Any process may also call `DeviceGrants::revoke`: the file is the truth.

The runtime captures through the crate's AVFoundation backend and links no
GStreamer; `Runtime.entitlements` gives the signed `tilo-runtime` the camera
and microphone, and `Info.plist` says why when macOS asks.

The profile is applied by `/usr/bin/sandbox-exec`, which is SIP-protected.
It runs a preflight first, against a target that cannot exist. If the
kernel rejects the profile, the launch is refused with an error, and
nothing is ever started unsandboxed.

**Caveats and gaps.**

- Seatbelt (`sandbox_init_with_parameters`, `sandbox-exec`, SBPL and
  `system.sb`) is deprecated, undocumented Apple SPI. Apple can change
  it in any release. The profile has a pinned snapshot, and a real
  enforcement test runs on macOS.
- The GPU is the weak spot. Metal needs the IOAccelerator and IOSurface
  user clients, which are a large kernel attack surface that no profile
  can narrow. Mach services the profile allows (WindowServer, fonts,
  pasteboard) are reachable IPC surface as well.
- A camera or microphone is never opened by the app: macOS gates them
  through TCC, which the profile grants no service of. It is brokered
  instead (below).
- An allowlisted network under Seatbelt has no empty namespace behind it,
  so the gate listens on a loopback TCP port instead of a Unix socket, and
  the profile admits that one port. Any local process can reach the port;
  that grants nothing, because an unconfined process already has the
  network and the gate reaches only what the allowlist names. There is no
  `TILO_NET_GATE` on macOS.
- A process whose ad-hoc signature claims the App Sandbox entitlement
  gets Seatbelt, and then `libsystem_secinit` tries to apply App Sandbox
  on top. That aborts the process, which fails closed.

## Debugging

- `tilo-install run <pkg> --no-services` skips service resolution entirely.
  Useful when a `required` service is what's preventing launch.
- `tilo-install run <pkg> --no-landlock` (hidden, debug-only) disables
  Layer 3. Layers 1 and 2 stay enforced.
- `tilo package inspect <pkg>` shows the manifest without launching. Grep for
  `[sandbox.services]` and `[sandbox.portals]` first — they are the two
  blocks that decide whether the app can reach anything outside itself.

Next: [`runtime.md`](./runtime.md) — the SDK your entrypoint links against.
