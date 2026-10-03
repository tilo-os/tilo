# `ush` — the runtime that hosts a shell

`ush` is not a shell language. It is a locked-down application runtime that
*hosts* a shell: it builds a guest from an [environment](environments.md)
record — a read-only stack of images for `/usr` and `/etc`, the environment's
own home, the directories it was granted — puts a shell inside it, and brokers
every request that would reach past it.

## The problem it solves

`apps/terminal` cannot `exec` `$SHELL`. Inside a Tilo app sandbox that fails
with `ENOENT`: [`launcher::profile`](../../crates/packaging/launcher/src/profile.rs)
binds the loader, fonts, `/sys` and the CA bundle, and deliberately binds no
executable directory. Adding `/usr/bin` to the system read profile would hand
the host's entire command set to every app on the machine.

A guest whose root is built from images gets `/usr/bin`, terminfo and
`/etc/passwd` **by construction**, not as a grant, and what it can run is
exactly what its environment record lists — on every device it opens on.

## The environment record

An environment is a synced record declared by the Terminal app
(`#[resource(kind = "environment")]`, in
[`tilo-environment`](../../crates/runtime/environment/src/lib.rs)):

```rust
pub struct Environment {
    pub name: String,
    pub runtime: RuntimeRef,      // { release, images: arch -> tapp:<hex> }
    pub tools: Vec<ToolRef>,      // { packages, images: arch -> tapp:<hex> }
    pub grants: Json<Vec<CapOverride>>, // FsPath and NetHost grants, subject so.tilo.ush
    pub home: HomePolicy,         // Persistent (default) | Ephemeral
}
```

The record is architecture-neutral: a node stacks the images for its own
architecture (`x86_64`, `aarch64`). When a tool has no usable image for it —
none recorded, or one this node cannot materialise — the session builds it
there through the tool builder, for the same packages on that node's image of
the runtime release, and records the id with `add-tool`, so the next session is
a cache hit.

Its actions are `add-tool`, `remove-tool`, `grant-dir`, `allow-host`,
`revoke-grant`, `set-runtime` and `rename`. History and undo come from the record layer; there
is no other package manifest and no other permission file.

The supervisor keeps the records where Terminal keeps its own —
`<data home>/tilo-rooms/personal/so.tilo.terminal/.local/share`, room
`personal` — joins the record's shared writers, applies actions through the
record layer (attributed to `so.tilo.ush`), and appends. A session that names
no record gets the **default environment**, created on first use with the
system store's current `env-runtime`.

## The root a guest gets

```text
/usr, /etc     read-only overlay: lowers = [tool images…, env-runtime]
               (/etc also carries the session's passwd, group and resolver
               on top); no writable upper anywhere
/bin /sbin /lib /lib64   links into /usr
/home/<user>   the environment's home (per environment, in the ush store)
<grants>       one bind per FsPath grant, at the same path as on the host
/tmp /run /proc /dev     fresh
/var           tmpfs, plus the environment's own /var/lib and /var/cache
/run/ush/bin   the runtime and its command shims, read-only
```

Nothing else from the host is in the namespace — including its network: see
[The network](#the-network). A path you were not granted is
not missing a permission — it is not there. Images are materialised read-only
trees; `bwrap` stacks them as overlay lowers inside the guest's own user
namespace, so the supervisor needs no mount privilege of its own. Overlayfs
needs two lowers to mount without an upper, so a single tree (an environment
with no tools) is bound read-only instead.

The guest runs as **the real uid and gid**, through a user namespace that maps
exactly that one id. A file written to a granted directory belongs to you, and
nothing inside the guest is root. The images know only their own system
accounts, so each session lays a generated `passwd` and `group` naming you —
and a `resolv.conf` naming the session's DNS stub — over the images' `/etc`.

## The network

Every guest has a network namespace of its own. What it can reach is the
record's `NetHost` grants, enforced in the kernel by
[`tilo-egress`](../../crates/runtime/egress/src/lib.rs) — not by a proxy,
which command-line tools ignore:

```text
guest netns (owned by a user namespace the supervisor owns)
  resolv.conf → 127.0.0.1:53 ──► DNS stub (supervisor) ── granted? ──► the machine's resolver
                                   │ no: NXDOMAIN, nothing leaves the machine
                                   └─ every A/AAAA answered ──► nftables set, with the record's
                                                                TTL (at least 60 s, at most 4 h)
  nftables, output: lo and established flows accept; DNS anywhere but the stub
                    reject; TCP/UDP to an address in the set accept; else reject
  tap ◄──► pasta (passt, unprivileged, in the host's namespace) ◄──► the network
```

- **By name.** `perm allow-host github.com` allows that host;
  `perm allow-host '*.example.com'` the domain and every name under it
  (recorded as `.example.com`, the capability vocabulary's spelling);
  `perm allow-host '*'` everything. An address cannot be granted: it would
  bypass the resolver that decides.
- **No address literal gets through.** An address is reachable only after the
  stub answered it for a granted name, and only until the answer ages out.
  Connections already open survive that; new ones need a new answer.
- **No grants, no network.** The stub refuses every name and the set stays
  empty. `*` installs no filter: the guest reaches everything, still from
  its own namespace.
- **In force at once.** The stub and the filter belong to the running
  session, so `perm allow-host` — and `perm revoke` of a host — apply without
  `restart`. Narrowing empties the sets, so nothing answered under the old
  grants stays reachable.
- **Not the machine's own services.** `pasta` maps none of the host's
  loopback into the guest. Answers that point a name under a granted *domain*
  at a private, link-local or loopback address are not admitted (an exact
  host grant may name a LAN host); link-local — cloud metadata at
  `169.254.169.254` — never is.
- **Fails closed.** The supervisor programs the filter over netlink, through
  a socket opened inside the namespace while it still held the namespace's
  `CAP_NET_ADMIN`; no `nft` binary is involved. If the kernel refuses
  nf_tables and the record grants hosts, the session does not start and
  says why. `pasta` is attached only once the filter is in place. Without
  grants, a guest whose filter or `pasta` is unavailable starts offline.
- **Nothing in the guest can change it.** `bwrap`'s user namespace nests
  inside the one that owns the network namespace, so the guest has no
  capability over its network — and seccomp denies it `unshare` and `setns`.

The filter matches addresses, so a granted host shares its address with
whatever else is served from it (a CDN): names are what you grant, addresses
are what the kernel can enforce.

## The trust boundary

Three principals, in decreasing authority.

**The supervisor** is Tilo code and runs in the user's session outside every app
sandbox. It builds namespaces, so it needs `unshare`, `mount` and `pivot_root`
— the syscalls `launcher::seccomp` blanket-denies to apps. It owns the store,
the environment records and the audit log, and none of them is visible inside a
guest at any path. When a grant covers one of them (granting your whole home,
say) the supervisor covers it with an empty tmpfs after binding the grant.

**The guest** is everything you run. It is untrusted. The seccomp filter denies
`unshare`, `setns`, `mount`, `umount2`, `pivot_root` and `chroot`, so a guest
cannot build a second namespace and climb out of the first; `ptrace`, `bpf`,
keyrings, module loading and `AF_PACKET` sockets are denied too. Landlock
restricts writes to scratch, `/var`, the home and the writable grants.

**The supervisor's need for those syscalls does not propagate.** `bwrap`
installs the filter after it has finished mounting and immediately before it
execs the guest. Proven in `seccomp.rs::the_kernel_enforces_the_filter`, which
installs the filter in a forked child and calls `unshare(CLONE_NEWUSER)`.

**An app** cannot reach the runtime by executing it: it has neither the syscalls
nor a path to the binary. `apps/terminal` asks the supervisor over the `ush`
service socket for a target and receives a pty master over `SCM_RIGHTS`. What
crosses the boundary is a terminal, not a namespace.

## The service protocol

One line of JSON out, one line plus a descriptor back:

```json
{"op":"open-session","target":{"kind":"environment"},"cols":80,"rows":24}
{"op":"open-session","target":{"kind":"environment","record":"work"}}
```

`record` is a record key (`[A-Za-z0-9._-]`, at most 64 bytes); the owning app
and the room are fixed by the supervisor, never taken from the request. The
answer is `{"result":"session","session":…,"environment":…}` with the pty
master attached, or `{"result":"denied","reason":…}`. `{"kind":"host"}` — your
own login shell — is part of the vocabulary and is refused here with
`not available`: this supervisor builds environments only.

The request connection is the session's lifetime in both directions: Terminal
closing it ends the guest, and the guest exiting closes it.

## Installing tools

`pkg install pandoc` inside a guest asks the broker (`Request::AddTool`). The
broker builds a `tool` image for the environment's runtime through the
[`ToolBuilder`](../../crates/runtime/environment/src/lib.rs) seam — the build
runs `dnf` as uid 0 in its own build sandbox, never in your guest — and appends
it to the record with `add-tool`. Running sessions pick it up on `restart`; new
sessions have it immediately. A package another tool already provides is not
built again. The builder's output streams to the shell that asked.

`pkg remove` takes the tools providing the named packages out (`remove-tool`).
A tool image is one build, so packages installed together are removed
together; `pkg remove` says which. `pkg list` shows the runtime and the tools.

On a device without a tool builder, `pkg install` answers that building is not
available; the tools an environment already has keep working.

## Permissions are Tilo's

`perm allow-host github.com` applies `allow-host`, a `Scope::NetHost` grant
(see [The network](#the-network)); `perm list` shows hosts as `net …`.

`perm trust-dir ~/src` applies `grant-dir`, which mints a
`collab_types::caps::CapOverride` — `Subject::App("so.tilo.ush")`,
`Scope::FsPath`, `Effect::Grant`, `Verb::Write` — into the record, and every
question is `decide_capability`. That is the same record the compositor stores,
renders in its permissions panel and can revoke. A `Restrict` written elsewhere
narrows the bind here, and an expired grant does not survive into the next
session. Paths the guest builds itself (`/usr`, `/etc`, `/proc`, `/dev`,
`/run/ush`, and `/tmp`, `/run`, `/var`, `/var/lib`, `/var/cache`, `/home`
themselves) cannot be granted.

A new bind needs a new mount namespace, so `perm trust-dir` answers "run
`restart`". That is what the `restart` command is for.

## `scan`, `guard`, `quarantine` — and what they are not

Tilo ships no signature database, and this runtime does not provide one.

- `guard status` reports the environment, its runtime and tools, and
  enforcement it has **tested**: the Landlock line comes from attempting an
  operation the ruleset forbids, and the network lines from connecting to an
  address nothing granted (`1.1.1.1:443`, which must be refused) and to the
  first granted host.
- `scan <path>` reports which image a path comes from (the topmost image that
  has it — a file from an image was verified by its content address when it
  was materialised and cannot be changed from the guest), executable and setuid
  bits, and whether a write there would reach the host. Every report ends with
  the sentence saying nothing was checked against a database.
- `quarantine` is user-driven: `quarantine add` moves a file into
  `/var/lib/ush/quarantine` with its executable bits cleared, so the original
  path stops resolving. No file is moved there automatically.

## Commands

```
ush [-c COMMAND] [-v] [--environment RECORD]   a shell in an environment
tilo-ush serve [--socket PATH]                  hand guests to apps that ask

pkg install PKG…     pkg remove PKG…     pkg list
perm trust-dir DIR   perm allow-host HOST
perm list            perm revoke ID
scan PATH            guard status
quarantine list | add PATH | restore PATH
restart
tilo …  app …  store …  system …      brokered to the host session
```

## Storage

```
~/.local/share/ush/
  environments/<record>/home/   an environment's home
  environments/<record>/var/    its /var/lib and /var/cache
  sessions/<id>/                generated /etc entries, the broker socket,
                                an ephemeral home
  audit/                        the broker's append-only log, one file per day
```

`$TILO_USH_HOME` moves the store (and, with it, the records);
`$TILO_USH_RECORDS` moves the records alone. The supervisor hands both to the
sessions it starts. Images live in the package store; tests point
`$TILO_USH_IMAGES` at fixture trees instead (`<hex id>/` directories and a
`current-runtime` file). The network tests fake the outside world on
loopback: `$TILO_USH_RESOLVER` names the resolvers the stub forwards to,
`$TILO_USH_EGRESS_MAP_HOST_LOOPBACK` an address at which the guest reaches
the host's loopback, and `$TILO_USH_EGRESS_REFUSE_NETFILTER=1` makes the
session behave as if the kernel refused the filter.

## Known limits

- **The network is filtered by address.** See the end of
  [The network](#the-network). Unix sockets are not network traffic: a
  granted directory holding one reaches whatever listens on it.
- **Images are not fetched from the registry yet.** The supervisor
  materialises images through the package store (`materialized_tree`), from
  the system app store, the user's store and the shelf in the user's synced
  files; CI catalog images that ship with neither are not fetched.

## Images your other devices built

A tool built on one of your devices opens on the others — your relay
included — without being built again:

- **Trust.** The supervisor's store trusts, besides the publishers the
  installation ships and this device's own key, every device key your
  identity has certified (`ImageTrust::with_fleet`). The fleet is read again
  at every use (`ush::fleet`: this device's certificate, the identity host's
  fleet record, the certificates your home peers were enrolled with), so a
  device you remove stops counting at once. Its images are refused from then
  on, the ones already materialised here included and whenever they claim to
  have been signed, because a lost key can back-date a signature; your other
  devices build their own. A relay is one of your devices exactly when it
  holds a certificate from your identity; a relay without one is anyone, and
  its images open only when the CI catalog key signed them. Another person's
  devices are never trusted this way: only your identity speaks for your
  fleet.
- **Transport.** A device shares each tool it builds by putting the whole
  package on the shelf at `.tilo/images/<id>.tapp` in your synced files, and
  every device looks there for an image it lacks before building one. The
  synced files already reach all your devices and are fully replicated on your
  home peer, which serves them by content hash when a device reads one, so no
  new protocol or peer service is involved. Nothing on the shelf is trusted:
  the store checks each package is the image asked for and admits it by its
  publisher, as it does a registry's. A session with no synced files mounted
  neither shares nor fetches, and builds what it lacks.

## Verifying it yourself

```bash
cargo test -p ush -p tilo-environment -p tilo-egress   # unit + integration
```

The integration test builds a real guest from fixture images — a runtime whose
`usr` is the machine's `/usr` and a tool that adds one command — and checks
`pkg list`, the tool on `PATH`, a read-only `/usr`, the real uid, and that a
file written to a granted directory is owned by you. It probes first whether
the machine can stack the images at all; inside a build container whose own
`/usr` is an overlay it cannot, and says so. The network test gives the record
one host and checks, from a shell in the guest, that the host is reachable by
name, that an address literal and an ungranted name are not, that
`perm allow-host` applies at once and that `guard status` reports what it
tested; a second checks that a session with hosts it cannot filter does not
start. They need `pasta` and `/dev/net/tun`, and say so where those are
missing. Run the compiled test binary
directly on the host to exercise the guest:

```bash
cargo test -p ush --test it --no-run   # prints the binary, target/debug/deps/it-<hash>
./target/debug/deps/it-<hash> serve_handoff:: --test-threads 1
```
