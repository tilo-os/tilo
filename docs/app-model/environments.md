# Environments: where a shell runs and what it can run

A terminal session, an agent's shell and a Linux app all run *somewhere*. Tilo
makes that place explicit. Every session is bound to a **target**:

| Target | What it is | Identity and files | Packages |
|---|---|---|---|
| **Host** | The machine's own login shell (desktop installs; the owner's shell on Tilo OS) | The real user, the real home | The host's own (rpm-ostree, Flatpak, …) |
| **Environment** | A sandbox whose `/usr` is a stack of read-only images, on this device or one that takes work | The real uid inside its own namespaces; its workspace's own files as `~` plus directories granted to it | Tool images listed in the environment record |
| **Linux VM** | One VM per user and guest OS, for software that needs a whole system | A normal user account in the guest; granted directories shared in | Whatever the guest installs (Flatpak, its package manager) |

The terminal never receives a namespace or a VM. It receives a pty from a
broker. The broker decides where the session runs, so the same Terminal works
against this device or a VM. This is the boundary `ush` already
draws (`ServeRequest::OpenSession` → pty over `SCM_RIGHTS`); environments
extend what is on the other side of it.

## An environment belongs to a workspace

A Terminal is opened in a workspace, and every environment it lists, makes
and opens is that workspace's. Several agents can work on one device
at once, each in a workspace of its own, each with its own files, shell and
environment, none seeing another's:

- **Records.** An environment's record is kept in its workspace, where
  Terminal opened there keeps its own records, and names the workspace. Listing environments in a workspace lists only that
  workspace's.
- **Files.** Its `~` is the workspace's own files: for a workspace `W`, its
  tree of the device's files, `/var/workspace/.workspaces/W`. Only that tree
  is bound into the guest, and the environment is refused any grant of
  another workspace's tree or of the files folder around them.
- **Variables.** Its programs start with `TILO_WORKSPACE` (the workspace's
  id) and `TILO_ENVIRONMENT` (the environment's stable id,
  `<workspace>/<record>`).
- **Who says which workspace.** The launcher, for a sandboxed Terminal: it
  writes the workspace the app was launched in on the `ush` connection
  before the app holds it, and the app cannot name another. The shell, for
  a Terminal it hosts: on each request.

Environments made before environments belonged to a workspace are the
**primary workspace**'s (`personal`), unmoved: they open as before, with the
device's files folder as `~`, for a Terminal in no workspace. See
[`ush`](ush.md#workspaces).

## Cost classes

| Class | Runs as | Per-tenant cost where it is served |
|---|---|---|
| Tilo apps | The launcher sandbox (unchanged) | ~0 |
| Environments (CLI) | Namespaces + seccomp + Landlock over shared read-only images | ~0 idle (socket activated); images are shared by content address |
| Linux apps | A VM on a host with KVM | Real: metered, quota'd, suspended when idle |
| Hosted workspaces' Linux programs | The workspace environment's own Firecracker VM | Real: billed per second while it runs, limited per plan, snapshotted when idle |

A member that takes work runs the first two. A Linux VM launch is a request to
a member that advertises KVM.

## The guest helper

The supervisor mounts its own helper and its exact native loader dependency
closure as individual read-only files under `/run/ush/bin` and `/run/ush/lib`.
The host glibc loader resolves that closure before startup; missing libraries
produce an error before a guest starts. No host library directory or tool is
mounted, and the environment's `/usr` remains the signed image stack.

The first process enters this private loader and helper directly to install
Landlock, then executes the environment's shell. Command shims preserve their
own names for broker dispatch and use a library path scoped to that helper
exec. The shell and its tools receive no global `LD_LIBRARY_PATH` override.

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

Built from the same snapshot as the OS, most of an env-runtime's files are
byte-identical to the host's own `/usr`. The OS image therefore ships its tree
sparse: the files the host already has are left out and named, each with the
host path that holds the same bytes, in the image's `host-sources` beside its
signed metadata (`os/scripts/seed-env-runtime.py`, #4072). A sparse system
tree is never handed out as it is: `materialized_tree` rebuilds it in the
user's store from the tree and those host files, every byte checked against
the image's signed hash, as for any system image (`local_materialized_tree`).

### Resolving images

```rust
/// crates/core/package-store
pub fn local_materialized_tree(store: &Store, id: &PackageId) -> Result<PathBuf, MaterializeError>;
```

`local_materialized_tree` resolves and admits the signed image, then returns
its read-only root in the per-user package store. System images are rebuilt
there from verified bytes through the content store, so bootc and system
extensions beneath `/usr` do not consume the guest overlay's stacking depth.
Cached bytes, links, executable modes and publisher trust are checked again
before use. The import is one durability barrier, not one per file: the
content store syncs once before the tree is materialised. A session that has
to import first says so on its terminal before the prompt appears. The guest mounts this local tree read-only; its confinement stays
the same. Other image readers can use `materialized_tree` without relocating
the system tree.

### Developer image probe

`cargo run --locked -p ush --example resolve-signed-images -- RUNTIME_PACKAGE TOOL_PACKAGE`
uses the same `StoreImages` provider as the supervisor. It checks the tool's exact
runtime binding, admits both signed packages and prints their verified read-only
roots as JSON. It does not mount an environment or execute a tool. Run it in the
intended owner's profile: publisher and fleet trust are checked again, including
on cached images. Host paths in the returned JSON are local observations, not
collaborative resource identifiers.

`tilo app runtime image add --publisher KEY` trusts that key for that admission only;
it does not change subsequent image-reader trust. A device normally uses its
enrolled publisher. A developer probe may use the existing
`TILO_USH_TRUSTED_PUBLISHER` setting scoped to its own private profile. This does
not persist a publisher policy or bypass signature, revocation or runtime checks.

## The environment record

An environment is a synced record (`#[resource(kind = "environment")]`,
declared by the Terminal app), kept in its workspace, so it follows
the user to every device that holds the workspace:

```rust
pub struct Environment {
    pub name: String,
    pub runtime: RuntimeRef,         // an env-runtime release
    pub tools: Vec<ToolRef>,         // tools built for that release
    pub grants: Vec<CapOverride>,    // FsPath / NetHost grants, Subject::App("so.tilo.ush")
    pub home: HomePolicy,            // Persistent (default) | Ephemeral
    pub home_dir: String,            // what `~` is: the workspace's own files
    pub workspace: String,           // the workspace it belongs to; empty: the primary one
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
opens on an aarch64 device. A member uses the images for its own architecture; when
a tool has no image for it yet, the node builds it (same packages, same runtime
release) and records the id, so the next session there is a cache hit. An image
another of the user's devices built for the same architecture is fetched from
the shelf in their synced files and trusted while that device is certified by
their identity, rather than built again (see `ush.md`).

Actions on it (`#[action]`): `add-tool`, `remove-tool`, `grant-dir`,
`allow-host`, `revoke-grant`, `set-runtime`, `rename`. History and undo come from the record
layer; there is no separate package manifest.

## The guest

`ush` builds the guest from the record:

```text
/usr, /etc   overlay (read-only): lowers = [tool images…, env-runtime]
             (--ro-overlay; no writable upper)
~            the workspace's own files, through their grant (ush.md, "Home")
<grants>     one bind per FsPath grant, at the same path as on the host
/tmp /run /proc /dev   fresh
/var         tmpfs, plus the environment's own /var/lib and /var/cache
/run/tilo/agent.sock   the environment's own MCP server (below)
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
  start. See [`ush`](ush.md#the-network) and `crates/core/egress`.
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
   sealed into a `tool` tapp, signed by the device key, stored, and shelved
   in the user's synced files for their other devices.
4. The record's `add-tool` action appends the `ToolRef`. Running sessions pick
   it up on `restart`; new sessions have it immediately.

`pkg remove`, `pkg list` and `pkg diff` read and edit the record. Builds run
where the session runs (this device or the member that took the work). CI
publishes a catalog
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
`Scope::Device`). Agents and other apps can never open it. A device with no
`env-runtime` image has no environment to start: a Terminal asks for one, is
refused, and opens the Host target instead, with the same allow.

## Agents in an environment

Several agents work on one machine at once, each
in an environment of its own, each in its own workspace, and each a distinct
agent the person can tell apart, attribute and stop.

**Two agents in two workspaces.** In the workspace *Studio*, open Terminal in
the environment *dev* and run an agent there; switch to *Garden*, open Terminal
in the environment *site* and run it again. Each one's MCP server is
`tilo-runtime --connect` (`tilo-runtime-mcp --connect`), with nothing else to
configure:

```json
{"mcpServers": {"tilo": {"command": "tilo-runtime", "args": ["--connect"]}}}
```

For a client that names itself Reviewer, the first asks to work in *Studio* as
`reviewer@dev#1`, the second in *Garden* as `reviewer@site#1`. The person allows each on the sheet; the bar
shows both, each in its workspace, and Stop on one ends that one alone.

**The environment's socket.** The session's MCP server
(`$XDG_RUNTIME_DIR/tilo-session/agent.sock`) serves only programs of the login
outside every sandbox, so nothing in a guest reaches it. When `ush` opens an
environment it asks the session, on that socket, for a socket of the
environment's own, by its `TILO_ENVIRONMENT` id
(`runtime_portal::environment_socket`; the request passes the
socket's kernel peer checks, so it comes from a program of the login outside
every sandbox). The session binds a fresh socket beside its own
(`tilo-session/environments/`), answers its path and the workspace it serves,
and `ush` binds it into the guest alone at `/run/tilo/agent.sock`, naming it in
`TILO_AGENT_SOCK`, which `tilo-runtime --connect` uses in place of the
session's socket. The connection the request came on is the socket's lease: when
the environment ends (or its `ush` does) the socket is removed and the agents
it let in are stopped.

- **Workspace.** A program on an environment's socket works in the
  environment's workspace (above: the one its Terminal was opened in, as its
  record names it, read by `ush::workspace::environment_workspace`), never the
  one in front, and asking for another is refused. An environment of the
  primary workspace, which the shell knows by no id, works in the workspace in
  front of the person when it opened. The session admits the workspace only
  if the person is one of its people.
- **Who may use it.** Being reachable only inside the environment is what
  authenticates a program, and the kernel says so too: only a process of the
  account that descends from the process holding the lease (the environment's
  own `ush`) is served there. A program of the login outside it, or one in
  another environment, is refused before anyone is asked. The session's own
  socket keeps its checks: another account, or a process in other namespaces,
  is refused there.
- **The person decides.** Each program is asked about on the sheet, naming
  the environment; "Always allow" is remembered per environment (its
  `<workspace>/<record>` id), executable and client name, so allowing Reviewer once in *dev* covers its later
  sessions there, and only there. A program in an environment works as an
  agent; it never acts as the person.
- **An agent per connection.** Each connection the person admits is an agent
  of its own: the client's name, where it works from (the environment's record
  key, or `host`) and an instance number, as in `reviewer@dev#2`. Its
  changes are attributed to that id, the shell lists it as `Reviewer in dev
  #2`, and the person's Stop ends it alone. Instance numbers count from one in
  each session of the desktop.
- **The Host case.** A program in the Host target, or any program of the login
  outside a sandbox, uses the session's own socket and works in the workspace in
  front. `tilo-runtime --connect --workspace ID` asks for another by id; it is
  admitted there only when the person is one of that workspace's people (and
  not blocked in it), and the person still decides on the sheet.

## Linux VMs

One VM per user per guest OS (`linux`, `windows`). Apps share it the way they
share a desktop.

- **Lifecycle:** the first Linux VM launch starts (or resumes) the VM; it grows
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
- **Installing apps:** Flatpak inside the guest; installed Linux apps get a
  launcher shim in every environment of that user and an entry in search.

A launch from a terminal: `code .` resolves to a shim in the environment's
`PATH`; the shim asks the broker (`Request::LinuxVmLaunch { app, args, cwd }`);
the supervisor asks the Linux VM service to ensure the VM; the guest agent runs
`code .` in the same directory; the window appears in the current strip.

The Windows appliance (`crates/compat/windows-vm`) is that service's `windows` guest;
its guest bridge moves to the same vsock agent protocol.

## Hosted workspace VMs

On a host that serves other people's workspaces, a workspace's shell, its own
commands and Tilo's actions run as everywhere else, and need no VM. A Linux
program the shell hands on (tier 3: `cargo`, `npm`, `python`) runs in a VM of
that workspace environment's own, never in a guest sharing the host's kernel
(`crates/vm/linux-vm/src/hosted`).

- **One VM per workspace environment,** with a size (`small` 2 vCPU / 4 GB,
  `default` 4 / 8, `large` 8 / 16, `xlarge` 16 / 32) and a persistent disk of
  50, 50, 110 or 250 GB.
- **Lifecycle:** the first program boots it; once no program has run for the
  idle limit (30 s by default) it is snapshotted to disk and its process
  ends; the next program restores it. It is the Linux VM's lifecycle and
  agent, on Firecracker: a restored VM maps its memory file and reads pages
  as the guest touches them, and a suspend writes only the pages written
  since the last one. The crate docs have the measurements behind the choice.
- **More than one host:** a VM stays on the host that first woke it, where
  its disk is. A workspace's Linux work is a turn its devices take
  (`work::takes_turn`): the host keeping the workspace's VM takes it, else one
  with room for one more VM, room being the memory its awake VMs really take —
  what the kernel measures each Firecracker process at, not the size the VM
  may grow into. While no device takes it, the work waits and says so
  ("Waiting for a device that runs Linux."); a wake on a full host waits too
  ("The machine is full until one of its VMs goes idle."). Neither is ever
  refused for room.
- **Eviction:** a host the cloud takes back at short notice (an Azure Spot
  VM, about 30 seconds) is told by a file appearing (`--evict-notice`, written
  by whatever watches the cloud's notice). It then wakes no more VMs, saying
  the VM is moving to another machine, and suspends every awake one at once onto
  its state directory, which lives on storage that outlives the host. The
  next host to open that directory restores each VM on its next command. A VM
  lost before its suspend has no valid snapshot and boots afresh from its
  disk, so old memory never meets a newer disk.
- **Moving a VM:** a machine told to drain — the same entry the eviction notice
  uses, asked for by an action, so a rolling update calls it — takes no more
  wakes and suspends every awake VM onto its state directory, then moves each
  VM, its disk, its snapshot and its meter, to the machine it names. The
  bytes go over the connection the two machines already keep, a block at a time: a
  transfer cut short carries on from its last whole block, a disk's holes arrive
  as holes, and the machine taking a file uses it only once every block and the
  file's digest are what was offered. The VM arrives asleep and restores on its
  next command; one whose suspend failed moves its disk alone and boots afresh.
  The machine it left keeps the fact that it left, so a wake placed there says the
  VM moved rather than making an empty machine in its place.
- **A lost machine:** each VM's disk and latest snapshot are copied in the
  background to a second machine, a block at a time, so only the blocks that
  changed since the copy before travel. A VM that is running is paused for its
  copy and goes on, so its copy is a disk and no snapshot; one that was asleep
  sends its snapshot too. A restore never pairs an old snapshot with a newer
  disk — taking a newer disk drops the memory that came before it — and boots
  the disk afresh. Losing a machine loses the work since the last copy.
- **Files:** the environment's granted directories, at the same paths, served
  over vsock (the agent mounts them as FUSE). Each is served by its own host
  process held to that directory by Landlock, in user, mount, network and IPC
  namespaces of its own where the jails and the service's sockets do not
  exist.
- **A program's `~`:** a program the shell runs in the VM starts with `HOME`
  the environment's shared folder — the same path the shell's `~` names — so
  what the program writes there is the workspace's files,
  versioned and bounded by its grant. What belongs to the machine rather than
  to the workspace stays on the VM's own disk, in the tenant's folders there:
  `XDG_CACHE_HOME=/home/tilo/.cache`, `XDG_DATA_HOME=/home/tilo/.local/share`,
  `NPM_CONFIG_CACHE=/home/tilo/.npm`, `CARGO_HOME=/home/tilo/.cargo` and
  `RUSTUP_HOME=/home/tilo/.rustup` (the toolchain the image ships), and
  `CARGO_TARGET_DIR=/var/tmp/tilo/target`. The shell sets these over the ones
  a run carries, so no run moves its `~` or its caches into the workspace's
  files, and a program the machine ships is found
  by its path there rather than under the run's `~`. The guest agent takes a
  launch's `HOME` only when it names a directory the guest has, and the
  tenant's own home otherwise.
- **Jail:** each VM runs as a uid of its own (a host has slots; slot `n` is
  uid `base + 2n`), in Firecracker's jailer: a chroot holding only its
  devices, its state, its sockets and the guest kernel; a cgroup bounding
  its memory, CPU and processes; a mount and network namespace of its own.
  The service is unprivileged; one small helper, `tilo-hosted-jail`, starts,
  stops and releases a slot's VM. It is root-owned, runnable only by the
  service's group, holds `cap_setuid,cap_setgid` to become root, refuses any
  caller but the service, reads only a root-owned configuration
  (`/etc/tilo/hosted-jail.json`), and takes the VM's directories as
  descriptors the service opened. Its sockets directory is on the run
  directory's tmpfs, where the directory's set-group-id bit gives every socket
  Firecracker makes there the service's group. Releasing a place hands every
  file back to the service and removes the jail, so the next VM in the place
  inherits nothing.
- **Network:** the public internet, nothing of the host's. In the VM's
  network namespace (owned, through a user namespace, by the slot's network
  uid) Firecracker's tap and `pasta`'s are bridged, and a bridge filter drops
  frames to private, shared, link-local (the metadata endpoint
  169.254.169.254), loopback, multicast and reserved networks, Azure's
  168.63.129.16 and any network the host adds; IPv6 is off. `pasta` makes
  every connection from a socket of the host's and forwards nothing inwards,
  so the host routes and translates nothing, and in Kubernetes the pod's
  network policy applies to VM traffic too. Every VM has the same address
  (10.0.2.15, gateway .2, DNS .3), so a snapshot restores anywhere. A VM
  is allowed one destination besides the internet: the server its Browser
  session joins the workspace through
  (`--server-endpoint`/`TILO_VM_SERVER_ENDPOINT`, a public address or the
  stack's cluster address), and a server on a local host's loopback is
  reached at 10.0.2.4, which `pasta` maps there. Firecracker's rate
  limiters bound each VM's bandwidth.
- **Metering:** each run of the VM's process and each held snapshot is a
  usage record (VM, workspace, account, size, start, end, seconds) in the
  account's log, billed per second with the sub-second rest carried to
  the next run. A stopped or suspended VM costs nothing.
- **Plans:** an account's plan bounds its running VMs, its VMs kept and the
  sizes it may use. A refused program says why in plain words, and what to
  do, and the refusal is typed
  (`tilo_action_services::server_account::Refusal`: a reason and the next
  steps, each with the provider's page).
- **Selling VMs:** a host that sells them says so in its plans file
  (`sells`: its name, the plans it offers, the pages where a card is added,
  limits raised and billing managed). An account the file does not name has
  no card on file: nothing asks for one until its first Linux program, which
  the guard refuses with `no_payment_method`. A person's command then waits
  with Terminal's prompt and runs once the provider adds the account to the
  file; an agent's is refused at once and tells the person what to do
  ([`ush.md`](ush.md), "The shell marks each run"). Billing itself (checkout,
  prices, credit, rate limits, fraud checks) is the provider's: it only
  writes the plans file and answers its pages.
- **The account, live:** the session serves the person's account with its
  server (`tilo.server`, `server.account/account`) as a live read: the plans
  offered, the account's access, usage per workspace, limits, credit,
  invoices and where billing is managed, sent again whenever the plans file
  changes (`crates/core/tilo-server-account`). A host that sells nothing
  answers no plans and no access.
- **ush** runs the program through the VM's agent with the shell's streams
  relayed, when the host names its hosted-VM service (`TILO_HOSTED_VM_SOCKET`,
  `TILO_HOSTED_ACCOUNT`, `TILO_HOSTED_VM_SIZE`, and `TILO_HOSTED_PLANS`, the
  plans file a held command follows). Run records are the shell's,
  unchanged. A program whose input and output are the session's terminal
  runs on a terminal in the VM: the keys are relayed as typed, the size
  follows, and the output is recorded like any other.
- **The agent socket:** a program in the VM finds Tilo's MCP server where an
  environment's programs find it on a device, `/run/tilo/agent.sock`, named in
  `TILO_AGENT_SOCK` (above, "Agents in an environment"). The guest agent serves it to the
  programs a host started for a workspace shell's session
  (`LaunchServingAgents`) and to their descendants alone, by the connecting
  process the kernel names; anything else is closed unread. Each connection
  travels to the host on the program's own connection to the agent, and ush
  hands it to the workspace's server with the workspace, environment and
  session it came from (`ush::hosted_vm::MachineAgents`), which decides what
  it is. It ends with the program
  ([`agent_socket.rs`](../../crates/vm/linux-vm-agent/src/agent_socket.rs)).
  The server asks the person whose Terminal `run` the workspace admitted for
  that session, on their approving devices, whether the program may work in
  the workspace as `<client>@<environment>#n` (numbered from one in each
  workspace since the server started). "Always allow" is kept for that person
  per environment, executable and client name. Allowed, it acts as that agent
  for them through the workspace's actions, held to their access and an
  agent's bound, and History keeps what it does as that agent's; a session
  nobody is known to have started is refused before anyone is asked
  ([`machine_agents.rs`](../../crates/core/tilo-server/src/machine_agents.rs)).
  This holds where the server runs its workspaces' shells itself; where their
  machines are on VM hosts, the hosts do not carry these connections back yet.
- **Secrets:** a workspace hands items of its vault to the programs its VM
  starts through one Terminal document, its machine secrets
  (`tilo_environment::machine_secrets`): each names a vault item by its
  reference and the variable it arrives in (`GITHUB_TOKEN`), and
  members change it with `add-secret` and `remove-secret` as the workspace's
  policy lets them. When a command, a terminal session or another program
  starts in the VM, the server reads the document once and releases each item
  for whoever started it, as the workspace's policy lets them read its vault;
  an item the vault does not hold stops the start with "That item is not kept
  here". The values go only into the environment of the programs started in
  the VM, over any variable of the same name, never into the shell's own
  variables, the command line, what Terminal keeps of it, an answer or a log
  line.
- **The guest image** is one signed OCI image holding two files: an
  uncompressed x86-64 kernel built from a pinned kernel.org longterm release
  with everything a VM needs built in (virtio-mmio, blk, net, vsock, FUSE,
  ext4, cgroups, namespaces, overlayfs) and no modules, and `rootfs.ext4`, a
  raw ext4 of Fedora 44 (the release every Tilo binary is built on; `dnf
  install` adds packages) with the tenant account (`tilo`, uid 1000,
  passwordless sudo), git, a compiler, python and uv, node and rust on `PATH`
  for every shell, the headless session with Browser and Chromium, and the
  same guest agent the Linux VM runs. It has no systemd: PID 1 is
  `hosted-init`, which mounts what a guest needs, grows the filesystem to the
  disk it was given, points DNS at the host's forwarder and hands over to the
  agent. Each VM's disk is a reflinked copy of `rootfs.ext4`. Hosts do not
  build it: CI publishes it, signs it with the release key and pins the digest
  in `os/hosted-guest-snapshot.env`, and `tilo-linux-vm hosted fetch-guest`
  fetches only that digest under `/etc/containers/policy.json`'s
  `sigstoreSigned` scope for `registry.tilo.so/tilo/hosted-guest`.
- **Browser:** a hosted workspace's Browser runs in the same VM: the first
  browser action wakes it (no card needed) and starts a headless session in
  it, as the unprivileged `tilo-browser` user, which joins the workspace as a
  device. Linux actions in that VM still need the plan and the card. New VMs
  start as copy-on-write clones of a warm template of their size. The exact
  contract — argv, the `TILO_HOSTED_BROWSER` mark, one line of material on
  its stdin, `ready`, `up` and `used` on its stdout — is documented in
  [`crates/vm/linux-vm/src/hosted/browser.rs`](../../crates/vm/linux-vm/src/hosted/browser.rs),
  and the whole of it in [`hosted-browser.md`](../hosted-browser.md).
- **Linux apps with windows:** opening one (`app.so.tilo.linux#open`, naming
  its desktop file) starts a second headless session in the same VM, as the
  tenant, with Xwayland ([`hosted-browser.md`](../hosted-browser.md#linux-apps)).
  It joins the workspace as a device like Browser's and shares every window
  its apps open with the workspace's members, over the live session. Opening one
  is a Linux action: it needs the plan and the card.

## Members that take work

A member with the take-work appetite serves its workspace's environments the
way a device serves its own:

- The action service carries a request to open a session (`OpenEnvironment {
  record, cols, rows }`) and the accepting member answers with a pty stream
  over iroh. It builds the guest exactly as a device does, with images fetched
  from the registry.
- Granted directories map to the workspace's synced files on that member.
- Sessions are socket-activated and survive the client disconnecting; the
  Terminal re-attaches by session id.
- A Linux VM launch goes to a member that advertises `kvm`; its windows reach
  the requester as a window stream (`surface_stream`), with input routed back.

A member holds a workspace as any device does, so its action service admits the
server's runs. A member that runs the workspace's VMs also answers its browser
environments' opens there (`tilo_action_services::browser`), which the host's
hosted-VM service starts beside the Linux VM; the server asks for them the way
it asks for a shell command, as a Linux turn the member takes
(`crates/core/tilo-runtime/src/device/environments.rs`). The first time the server
asks about a workspace the member does not hold yet, the member enters it: it asks
the server's own `workspace.enter`, which answers an invitation naming the member's
person, and redeems it as a device joins any workspace. The server decides
which member may enter which workspace, never the request: a device of another
person asking the member gets nothing from it, and a workspace the server does
not govern is refused there and remembered for a while, so nothing that names
workspaces makes the member ask again and again.

## Terminal

`terminal.session` gains `target: Target` (`Host | Environment { record } |
LinuxVm { vm }`). A new tab picks its target (default: the most recently used
environment); the tab shows a target chip; a session's target is part of its
synced state, so reopening it on another device re-attaches to the same place
where possible.

A Terminal's environments are those of the workspace its window is in. A
Terminal the shell hosts names that workspace (its window's) on every request
to the supervisor (`"workspace":…`); a Terminal the launcher started is held
to the workspace it was launched in. Opening Terminal in two workspaces gives
two environments, each with its own `~`, its own picker list and its own
default environment.
