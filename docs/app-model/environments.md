# Environments: where a shell runs and what it can run

A terminal session, an agent's shell and a legacy app all run *somewhere*. Tilo
makes that place explicit. Every session is bound to a **target**:

| Target | What it is | Identity and files | Packages |
|---|---|---|---|
| **Host** | The machine's own login shell (desktop installs; the owner's shell on Tilo OS) | The real user, the real home | The host's own (rpm-ostree, Flatpak, …) |
| **Environment** | A sandbox whose `/usr` is a stack of read-only images, on this device or on a relay | The real uid inside its own namespaces; the environment's home plus directories granted to it | Tool images listed in the environment record |
| **Legacy VM** | One VM per user and guest OS, for software that needs a whole system | A normal user account in the guest; granted directories shared in | Whatever the guest installs (Flatpak, its package manager) |

The terminal never receives a namespace or a VM. It receives a pty from a
broker. The broker decides where the session runs, so the same Terminal works
against this device, a relay, or a VM. This is the boundary `ush` already
draws (`ServeRequest::OpenSession` → pty over `SCM_RIGHTS`); environments
extend what is on the other side of it.

## Cost classes

| Class | Runs as | Per-tenant cost on a relay |
|---|---|---|
| Tilo apps | The launcher sandbox (unchanged) | ~0 |
| Environments (CLI) | Namespaces + seccomp + Landlock over shared read-only images | ~0 idle (socket activated); images are shared by content address |
| Legacy apps | A VM on a host with KVM | Real: metered, quota'd, suspended when idle |

A relay itself only runs the first two. A legacy launch on a relay is a request
to a compute worker that advertises KVM.

## Images

An image is a **tapp package** — signed, BLAKE3-addressed, distributed by the
registry and materialised read-only by `package-store` — of one of two new
kinds:

- `env-runtime`: a minimal userland (shell, coreutils, glibc, ca-certificates,
  dnf for builds). It provides the base of `/usr` and `/etc`.
- `tool`: the files a set of packages adds on top of one specific
  `env-runtime` id. Its manifest names that runtime id; a tool never mounts on
  any other runtime.

Materialised images are plain read-only directory trees
(`pkgs/<package-id>/`). The supervisor needs no mount privilege: bwrap stacks
them as overlay lower directories inside the guest's user namespace.

`env-runtime` images are built in CI from the frozen RPM snapshot and ship in
the system app store (`/usr/share/tilo-apps`) with Tilo OS and the
`tilo-session-apps` package, so the first environment opens offline.

### Resolving images

```rust
/// crates/packaging/package-store
pub fn materialized_tree(store: &Store, id: &PackageId) -> Result<PathBuf, MaterializeError>;
```

`materialized_tree` fetches (from the registry when missing), verifies and
materialises an image and returns its read-only root. It is the only way the
supervisor turns an image ref into a path.

## The environment record

An environment is a synced record (`#[resource(kind = "environment")]`,
declared by the Terminal app), so it follows the user to every device and to a
relay:

```rust
pub struct Environment {
    pub name: String,
    pub runtime: RuntimeRef,         // an env-runtime release
    pub tools: Vec<ToolRef>,         // tools built for that release
    pub grants: Vec<CapOverride>,    // FsPath / NetHost grants, Subject::App("so.tilo.ush")
    pub home: HomePolicy,            // Persistent (default) | Ephemeral
}

pub struct RuntimeRef {
    pub release: String,                     // e.g. "2026.10"; names the same userland on every architecture
    pub images: BTreeMap<Arch, PackageId>,   // the env-runtime image per architecture
}

pub struct ToolRef {
    pub packages: Vec<String>,               // what the user asked for, e.g. ["pandoc"]
    pub images: BTreeMap<Arch, PackageId>,   // the tool image per architecture, for `runtime`
}
```

The record is architecture-neutral: an environment created on an x86_64 laptop
opens on an aarch64 relay. A node uses the images for its own architecture; when
a tool has no image for it yet, the node builds it (same packages, same runtime
release) and records the id, so the next session there is a cache hit.

Actions on it (`#[action]`): `add-tool`, `remove-tool`, `grant-dir`,
`allow-host`, `revoke-grant`, `set-runtime`, `rename`. History and undo come from the record
layer; there is no separate package manifest.

## The guest

`ush` builds the guest from the record:

```text
/usr, /etc   overlay (read-only): lowers = [tool images…, env-runtime]
             (--ro-overlay; no writable upper)
/home/<user> the environment's home (per environment, in the store)
<grants>     one bind per FsPath grant, at the same path as on the host
/tmp /run /proc /dev   fresh
/var         tmpfs, plus the environment's own /var/lib and /var/cache
```

- The guest runs as **the real uid** (a single-uid user namespace mapping), so
  files written to granted directories belong to the user and tools behave as
  they do for a normal user.
- Network: a network namespace per guest, connected by `pasta` (passt,
  unprivileged). The record's `NetHost` grants decide what it reaches, in the
  kernel: the guest's resolver is a DNS stub that answers only granted names
  (`example.com`, or `.example.com` for a domain and everything under it),
  and nftables in the guest's namespace admit only the addresses the stub
  answered, for the record's TTL (60 s to 4 h). An address literal, DNS to
  any other server and every ungranted name fail. `*` is unrestricted; no
  grants is no network. A session whose grants cannot be enforced does not
  start. See [`ush`](ush.md#the-network) and `crates/runtime/egress`.
- Seccomp and Landlock as today (`Profile::Ush`); `guard status` keeps testing
  enforcement.

`dsh` is removed. Its use cases — the real home, containers, podman — are the
Host target.

## Installing tools

`pkg install pandoc` inside an environment:

1. The broker receives `Request::AddTool { packages }`.
2. The **tool builder** resolves the packages against the environment's
   runtime. The cache key is `(runtime id, sorted package NEVRAs)`. A hit
   returns an existing tool image id.
3. On a miss it builds: a build sandbox whose `/usr` is the runtime image with
   a writable upper, running `dnf install` as uid 0 in its own user namespace
   (scriptlets work, because the runtime is a complete system). Its network
   is the same mechanism with an implicit allowlist: the hosts of the
   runtime's own repositories (`baseurl`, `metalink` and `mirrorlist` in its
   `/etc/yum.repos.d`, plus the mirrors a fetched metalink names). The upper is
   sealed into a `tool` tapp, signed by the device key, and stored.
4. The record's `add-tool` action appends the `ToolRef`. Running sessions pick
   it up on `restart`; new sessions have it immediately.

`pkg remove`, `pkg list` and `pkg diff` read and edit the record. Builds run
where the session runs (device or relay compute worker). CI publishes a catalog
of common tool images (git, python3, pandoc, ffmpeg, imagemagick, ripgrep,
jq, …) for each published runtime, so most installs are cache hits.

When the runtime is updated, `set-runtime` rebuilds every tool for the new
runtime before switching. A tool is never mounted on a runtime it was not built
for.

## Host target

The Host target is a pty running the user's login shell, unconfined, served by
the same supervisor (`ServeRequest::OpenSession { target: Host }`). It is only
reachable by the Terminal app, only on a device whose owner is the signed-in
person, and only after the person allowed it once (a `CapOverride` on
`Scope::Device`). Agents and other apps can never open it.

## Legacy VMs

One VM per user per guest OS (`linux`, `windows`). Apps share it the way they
share a desktop.

- **Lifecycle:** the first legacy launch starts (or resumes) the VM; it grows
  memory on demand (virtio-balloon / virtio-mem) and is snapshotted to disk
  after it has had no windows for a few minutes. Resume is from the snapshot.
- **Transport:** a guest agent on AF_VSOCK accepts `Launch { argv, cwd, env,
  stdio }` and relays stdio and the exit status.
- **Windows:** each launched app runs under a Wayland forwarder in the guest
  (`waypipe` over vsock); the host side connects to the compositor as a normal
  Wayland client, so every window is an ordinary `xdg_toplevel` in the current
  strip. X11 apps use Xwayland inside the guest.
- **Files:** granted directories are shared with `virtiofsd` (unprivileged) at
  the same paths.
- **Installing apps:** Flatpak inside the guest; installed legacy apps get a
  launcher shim in every environment of that user and an entry in search.

A launch from a terminal: `code .` resolves to a shim in the environment's
`PATH`; the shim asks the broker (`Request::LegacyLaunch { app, args, cwd }`);
the supervisor asks the legacy VM service to ensure the VM; the guest agent runs
`code .` in the same directory; the window appears in the current strip.

The Windows appliance (`crates/windows/windows-vm`) is the `windows` legacy VM;
its guest bridge moves to the same vsock agent protocol.

## Relays

A relay runs environment sessions for its members:

- The compute session protocol gains an interactive kind: `OpenEnvironment {
  record, cols, rows }` → a pty stream over iroh. The relay builds the guest
  exactly as a device does, with images fetched from the registry.
- Granted directories map to the member's synced files on the relay.
- Sessions are socket-activated and survive the client disconnecting; the
  Terminal re-attaches by session id.
- A legacy launch from a relay environment is routed to a compute worker that
  advertises `kvm`; its windows reach the member as a window stream
  (`surface_stream`), with input routed back.

## Terminal

`terminal.session` gains `target: Target` (`Host | Environment { record,
location } | Legacy { vm }`), where `location` is `ThisDevice` or a relay. A
new tab picks its target (default: the most recently used environment); the
tab shows a target chip; a session's target is part of its synced state, so
reopening it on another device re-attaches to the same place where possible.
