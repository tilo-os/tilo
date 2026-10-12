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
  bypass the resolver that decides. The grammar is the one every `NetHost`
  grant is read with (`collab_types::caps::NetHostPattern`), so a host
  allowed here means the same to an app; only `*` differs, by backend
  (`docs/architecture/security.md`, "One grant grammar, two backends").
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

The launcher authenticates the caller once with the existing `QUS1` handoff.
Terminal then negotiates persistent control before opening a session:

```json
{"op":"hello","protocol":2}
{"result":"ready","protocol":2}
```

An unsupported supervisor is refused before Terminal sends an open request.
This control connection retains the same authenticated caller for every request;
each Host request still checks the current owner grant and original requester.
A successful version-two session reply includes `"protocol":2` and exactly two
close-on-exec descriptors, in order: the PTY master and a private session lease;
a session that negotiated `editor_protocol` appends the editor capability as a
third, last descriptor.
The control connection remains available for subsequent targets. Requests are
bounded to 4096 bytes and eight active leases. Malformed requests close control;
completed sequential sessions do not exhaust a lifetime request counter.
Legacy clients retain the original one-descriptor, single-session protocol.

One line of JSON out, one line plus the session descriptors back:

```json
{"op":"open-session","target":{"kind":"environment"},"cols":80,"rows":24}
{"op":"open-session","target":{"kind":"environment","record":"work"}}
```

`record` is a record key (`[A-Za-z0-9._-]`, at most 64 bytes); the owning app
and the room are fixed by the supervisor, never taken from the request. The
answer is `{"result":"session","session":…,"environment":…}` with the pty
master attached, or `{"result":"denied","reason":…}`.

### The Host target

`{"kind":"host"}` is the device's own login shell: the account's shell from
the passwd database, on a pty, in the supervisor's session environment, with
no namespace, filter or Landlock ruleset. It is answered only when all of
these hold (`crates/runtime/ush/src/host.rs`):

1. **The caller is Terminal.** `tilo-launcher` writes `QUS1`, a little-endian
   `u32` length and `{"protocol":1,"app_id":…}` on a sandboxed app's
   connection before the app holds it; any other app id is refused. A
   connection without that preamble comes from outside every app sandbox —
   the shell hosting Terminal in-process.
2. **No agent asked.** The request carries `"requested_by":"agent"` whenever
   the session was asked for through Terminal's published actions (by an
   agent, another app, or a collaborator); that is always refused.
3. **The device owner is signed in.** The owner's root key
   (`/var/lib/tilo/user-identity/pubkey.bin`) must be the session's default
   identity (`$XDG_CONFIG_HOME/tilo/identity/registry.json`).
4. **The person allowed it once.** A `CapOverride` granting
   `Scope::Device("host-shell")` to `so.tilo.terminal`, in the store's
   `host-shell.json`. Without it the answer is
   `{"result":"host-not-allowed",…}`; a shell-hosted Terminal tab then raises
   the shell's permission prompt, which records the answer there.

### Environments on a relay

`{"kind":"environment","record":…,"location":{"kind":"relay","relay":<endpoint id>}}`
runs the environment on one of the person's relays: those this device
enrolled as home peers for its user's personal workspace
(`home-peers.json` beside the identity keys; `{"op":"list-relays"}` lists
them). The supervisor still hands back a pty, and runs
`tilo-relay-environment` on its other end to carry it to the session there
(`crates/runtime/ush/src/relay.rs`). The answer's `relay_session` names the
session, which outlives the tab; a later request with
`"relay_session":<hex>` re-attaches to it. Tools the relay builds for the
session are added to the record as `pkg install` here would.

### Environments

`{"op":"list-environments"}`, `{"op":"create-environment","name":…}` and
`{"op":"environment-action","record":…,"action":"rename","args":{"name":…}}`
answer `{"result":"environments","environments":[{"key","name","tools",
"grants"}…]}` — what Terminal's target picker and environments panel show.

The private lease is the session's lifetime in both directions: replacing or
closing the Terminal session shuts down that lease and reaps its exact guest;
guest exit closes the lease without closing persistent control. PTY reader
clones cannot extend this lifetime. A legacy client's request connection remains
its lease. No client reconnects through a global socket to recover a consumed
launcher capability.

## Command runs

Terminal records what a session **ran**, not what its screen showed. A
session is a record of its own (`terminal.session`); opening Terminal starts a
new one (each window the launcher opens has a record of its own, and a new
terminal opened in a window is a new record), and reopening a window returns
to its session. The screen, the scrollback and
the window title stay on the device that runs the process: they are live
output, like the pixels of a legacy window, and another device reaches them by
re-attaching to the session on its relay.

### The shell marks each run

`ush` tells Terminal where each command starts and ends; Terminal never
guesses from the prompt. An `open-session` request names a key the session
marks its runs with, chosen by Terminal for that session:

```json
{"op":"open-session","target":{"kind":"environment"},"runs":"3f6c…"}
```

`runs` is 16 to 64 letters and digits. An interactive session answers
`"runs":true`: when its shell is `bash`, `ush` starts it with an rc file of its
own (`/run/ush/runs.bash` in a guest; for the Host target one that reads
`/etc/profile` first, as the login shell would) that sources the person's
`~/.bashrc` and then writes three marks into the pty stream:

```text
ESC ] 133 ; A ; k=<key> BEL                           the prompt is shown
ESC ] 133 ; C ; k=<key> ; cmd=<b64> ; cwd=<b64> BEL   a command line starts
ESC ] 133 ; D ; <status> ; k=<key> BEL                it ended with <status>
```

`cmd` and `cwd` are base64 of the command line as the shell's history holds
it and of the working directory. The key is a shell variable the rc file
takes out of the environment before anything runs, so no program the session
runs sees it, and output that imitates a mark (a `cat` of a file holding one)
is not taken for one: Terminal acts only on marks carrying its key, and takes
every mark out of what the screen shows. Another shell writes no marks; a
`command` session and an environment on a relay answer `"runs":false`. Their
output is live only.

### A run

Each run is an item of the session record's `runs` collection, written once,
when it ends, by the Terminal that runs the session:

| Field | |
|---|---|
| `command`, `cwd` | as the shell marked them |
| `environment` | where it ran: the session's target |
| `actor`, `for_person` | who ran it: the person typing, or whoever's `run` asked for it, and the person an agent ran it for |
| `started_ms`, `ended_ms`, `exit` | start, end and exit status; the duration is their difference |
| `output` | the raw bytes it printed, escape sequences included, as one content-addressed `Bytes` blob (at most 4 MiB; `truncated` when it printed more) |
| `tool`, `items` | which parser read it, and what it found |

No run writes a CRDT operation per character: the output is one blob, the
rest a handful of fields.

### Parsers

Every run's output is read twice when it ends. The generic parser strips
escape sequences and finds `path:line[:column]` locations, URLs and lines
that say `error` or `warning`. A tool parser, chosen by the command, reads
what that tool means: `cargo` and `rustc` diagnostics (the human format and
`--message-format=json`) as errors and warnings with their code and primary
location, `cargo test` and other libtest output as one item per test with its
outcome, `git status` (long and `--short`/`--porcelain`) as one item per
changed path. Each item is typed (`error`, `warning`, `location`, `url`,
`test-passed`, `test-failed`, `test-ignored`, `change`) with its text and,
where it has them, a path, line, column, code or test name.

### Actions on runs

- `run` runs a command line in the session: a verb of kind **Outside**, which
  queues the command in the record's outbox for the process that runs the
  session now (a request names its pane, so a restarted process never runs
  what was asked of the one before). The Terminal running it types the line
  at the next prompt and settles the entry with the run once it ends
  (`{"run": <id>, "exit": <status>}`; the run's id is the request's), so an
  agent's call returns when the command finished. It shows in the view like
  a person's typing, and the run is attributed to whoever invoked it. A
  session whose shell does not mark its runs, or one in the device's own
  shell, settles the request as failed.
- `rerun` (on a run) runs its command line again; `rerun-test` (on a run, with
  a test name it reported) runs that one test the way the run's tool does
  (`cargo test -p <package> -- --exact <name>`); both are Outside like `run`.
- `open-item` (on a run, with the item's position among its items) shows an
  item: a location or a diagnostic opens its file (relative to the run's
  directory), a URL opens in the browser.

Agents read runs as data: each run is a published item with its fields and
items, its `output` is read with the content it names, and "the second error"
is `open-item` with the second error's position.

Which commands an agent's `run` may start is a grant on the environment;
see [permissions.md](../architecture/permissions.md).

### Search and history

The person's command history spans every session: each run also adds an
entry to the person's own history record (`terminal.history`: command, cwd,
environment, exit, when, who, and the session record and run it was), which
follows them to their devices and never reaches a collaborator. It keeps the
newest 10,000 runs. Every entry is a search entry under its command line, so
Terminal's search finds a command whichever session ran it, and each
session's own runs are search entries of the session whose `open` shows it.

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

The brokered host commands are a fixed set (`app update|list|search|repo
list`, `system status|update`, `tilo update`); the guest names a request, never
a program. The broker runs the host's own binary and streams its stdout and
stderr to the guest as they are produced, then its exit status. Each run is
written to the audit log. The guards:

- Arguments are capped at 4 KiB.
- At most 1 MiB of output is forwarded. Past that the command keeps running,
  the guest is told the rest is not shown, and the exit status still follows.
- Updates may run 30 minutes, queries 5 minutes. A command past its limit is
  stopped, with its helpers, and the guest gets the reason.
- Updates run one at a time; a second one says it is waiting.
- Ctrl-C (any client that hangs up) stops the command and its helpers.

Legacy app launches go the same way but have no time limit: they last as long
as the app is open.

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
## Where images come from

An image the record names is looked for, in order, in the system app store,
the user's store, the shelf in the user's synced files, and the OCI registry
CI publishes env-runtimes and catalog tools to
(`registry.tilo.so/tilo/env-runtime` and `tilo/env-tools`, cached under
`$XDG_CACHE_HOME/tilo/env-images`; a relay names its own repositories in
`$TILO_USH_IMAGE_SOURCES`). Only a tool none of them has is built. Where an
image came from earns it no trust: it opens when the CI key the system store
names, or one of your devices, signed it.

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
  publisher, as it does a registry's.
- **Where the supervisor finds your synced files.** `tilo-ush` runs as the
  `tilo-ush.service` user unit, started with no workspace in its environment
  and in the host's mount namespace, so it reads your synced files where the
  Files app does: the filesystem daemon's mount at `/var/workspace`
  (`$TILO_WORKSPACE_ROOT` when a session names another; a relay names the
  member's shelf in `$TILO_USH_IMAGE_SHELF`). A device without that mount
  neither shares nor fetches through the shelf.

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
