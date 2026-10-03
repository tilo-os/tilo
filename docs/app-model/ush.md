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
    pub grants: Json<Vec<CapOverride>>, // FsPath grants, subject so.tilo.ush
    pub home: HomePolicy,         // Persistent (default) | Ephemeral
}
```

The record is architecture-neutral: a node stacks the images for its own
architecture (`x86_64`, `aarch64`). When a tool has no usable image for it —
none recorded, or one this node cannot materialise — the session builds it
there through the tool builder, for the same packages on that node's image of
the runtime release, and records the id with `add-tool`, so the next session is
a cache hit.

Its actions are `add-tool`, `remove-tool`, `grant-dir`, `revoke-grant`,
`set-runtime` and `rename`. History and undo come from the record layer; there
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

Nothing else from the host is in the namespace. A path you were not granted is
not missing a permission — it is not there. Images are materialised read-only
trees; `bwrap` stacks them as overlay lowers inside the guest's own user
namespace, so the supervisor needs no mount privilege of its own. Overlayfs
needs two lowers to mount without an upper, so a single tree (an environment
with no tools) is bound read-only instead.

The guest runs as **the real uid and gid**, through a user namespace that maps
exactly that one id. A file written to a granted directory belongs to you, and
nothing inside the guest is root. The images know only their own system
accounts, so each session lays a generated `passwd` and `group` naming you —
and a copy of the machine's resolver configuration — over the images' `/etc`.

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
  operation the ruleset forbids.
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
perm trust-dir DIR   perm list           perm revoke ID
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
`current-runtime` file).

## Known limits

- **The guest shares the host's network namespace.** The record's `NetHost`
  grants are not enforced yet: the mechanism is
  [`launcher::netgate`](../../crates/packaging/launcher/src/netgate.rs) — an
  empty namespace and a name-matched SOCKS5 gate — and wiring it to a shell,
  whose tools do not all honour a proxy, is its own change. Until then a guest
  reaches the network the machine reaches.
- **Images are not fetched from the registry yet.** The supervisor
  materialises images through the package store (`materialized_tree`), from
  the system app store and the user's store. Images this device signed (its
  own tool builds) are trusted; one another device of yours built is rebuilt
  here rather than fetched, until the store has a registry fetcher and the
  fleet's device keys as publishers.

## Verifying it yourself

```bash
cargo test -p ush -p tilo-environment   # unit + integration
```

The integration test builds a real guest from fixture images — a runtime whose
`usr` is the machine's `/usr` and a tool that adds one command — and checks
`pkg list`, the tool on `PATH`, a read-only `/usr`, the real uid, and that a
file written to a granted directory is owned by you. It probes first whether
the machine can stack the images at all; inside a build container whose own
`/usr` is an overlay it cannot, and says so. Run the compiled test binary
directly on the host to exercise the guest:

```bash
cargo test -p ush --test it --no-run   # prints the binary, target/debug/deps/it-<hash>
./target/debug/deps/it-<hash> serve_handoff:: --test-threads 1
```
