# `ush` — the runtime that hosts a shell

`ush` is not a shell language. It is a locked-down application runtime that
*hosts* a shell: it composes a root filesystem out of the host's, read-only,
plus a layer that belongs to you, puts a shell inside it, and brokers every
request that would reach past it. `dsh` is the same program on a developer
profile.

## The problem it solves

`apps/terminal` used to `exec` `$SHELL`. Inside a Tilo app sandbox that fails
with `ENOENT`: [`launcher::profile`](../../crates/packaging/launcher/src/profile.rs)
binds the loader, fonts, `/sys` and the CA bundle, and deliberately binds no
executable directory. Two alternatives fail for different reasons:

- Adding `/usr/bin` to the system read profile hands the host's entire command
  set to every app on the machine, which would defeat the purpose of the
  profile.
- Minting a capability per directory produces a permission dialog for
  `/usr/share/terminfo`, and still leaves `/etc/passwd` out — so the prompt
  prints `I have no name!` and `vim` and `less` do not work.

A **read-only host rootfs provides `/usr/bin`, terminfo and `/etc/passwd` by
construction**, not as a grant. The rest of this design follows from that.

## The root a guest gets

```text
/usr    overlay: lower = [ephemeral layer,] persistent layer, host /usr
                 upper = the session's writable layer
/etc    overlay: same stack
/bin /sbin /lib /lib64 /home …   the base's own root symlinks, mirrored
/var/lib /var/cache /var/log     the layer's own, bound
/var/tmp /tmp /run /proc /dev    fresh
/run/ush/bin                     the runtime and its command shims, read-only
$HOME                            the store's guest home, over the host home
<trusted dirs>                   one bind per `perm trust-dir`
```

Nothing else from the host is in the namespace. A path you were not granted is
not missing a permission — it is not there. The one exception is the machine's
resolver, bound read-only: it describes the network the machine is on and
carries no user content, matching the policy `launcher::profile` applies to
`/etc/resolv.conf`.

## The trust boundary

Three principals, in decreasing authority.

**The supervisor** is Tilo code and runs in the user's session outside every app
sandbox. It builds namespaces, so it needs `unshare`, `mount` and `pivot_root`
— the syscalls `launcher::seccomp` blanket-denies to apps. It is trusted with
them because it is the thing that *creates* the confinement. It owns the layer
store, `policy.json` and the audit log, and none of the three is visible inside
a guest at any path.

**The guest** is everything you run. It is untrusted. It runs as uid 0 *inside
its own user namespace*, which maps to the unprivileged real user outside and
confers no host authority whatsoever — that is what lets `rpm` chown a file into
a layer you own. The seccomp filter still denies `unshare`, `setns`, `mount`,
`umount2`, `pivot_root` and `chroot`, so a guest cannot build a second namespace
and climb out of the first.

**The supervisor's need for those syscalls does not propagate.** `bwrap`
installs the filter after it has finished mounting and immediately before it
execs the guest, so the syscalls exist for exactly the process that performs the
setup and for no descendant of it. Proven in
`profile.rs::the_kernel_enforces_each_profiles_filter` (the `Profile::Ush` case), which installs
the filter in a forked child and calls `unshare(CLONE_NEWUSER)`.

**An app** cannot reach the runtime by executing it: it has neither the syscalls
nor a path to the binary. `apps/terminal` asks the supervisor over the `ush`
service socket and receives a pty master over `SCM_RIGHTS`. What crosses the
boundary is a terminal, not a namespace — and never a `dsh` session, which
`Profile::reachable_from_app_launch` refuses on every request.

## Layers

A layer is a directory owned by you, shaped like a fragment of a root
filesystem, used as the overlay's `upperdir`. Creating one requires more than
`mkdir`,
because unprivileged overlayfs has two limits — both from the same fact, that
host uid 0 is not mapped into the guest's user namespace, so `CAP_DAC_OVERRIDE`
does not apply to a root-owned inode:

- **A root-owned directory cannot be copied up.** So a new layer mirrors the
  base's entire directory tree, owned by you: ~22 000 empty directories on
  Fedora 44, under a second, a few megabytes of metadata. After that a write
  anywhere the base has a directory needs no copy-up.
- **A root-owned file cannot be modified.** The rpm database and dnf's
  transaction history must be rewritten, so a new layer seeds its own copies.

That second point is also what makes a layer a *delta* rather than a second
operating system: the transaction starts from the base image's installed set, so
`pkg install htop` downloads htop and not glibc. Measured: `pkg install sl`
fetched 17 KiB.

**You can add to the base and shadow it, but you cannot alter a file that came
from the image.** This preserves the image-based OS's immutability.

`/var` is not shadowed. The store lives under `$HOME`, which is `/var/home/<user>`
on a bootc system, and overlayfs refuses an `upperdir` nested inside its own
`lowerdir`. The guest gets layer-owned `/var/lib`, `/var/cache` and `/var/log`
instead, which limits access to host machine state.

## Where packages come from

**`dnf`, running inside the guest, installing into `/`.** The guest's `/usr` is
an overlay whose upper is your layer, so "into `/`" *is* "into the layer", and
the host's filesystem is on the read-only side of the mount however the
transaction is spelled.

Alternatives and failure modes:

- **`dnf --installroot=<layer>` on the host.** This places files in the layer,
  but an installroot holding only the delta
  has no `/bin/sh`, no `ldconfig` and no `/usr/lib/rpm`, so every scriptlet
  fails. Measured on Fedora 44: `ldconfig` returns "call to ldconfig failed",
  `man-db`'s file trigger exits 127, and `dnf` reports the transaction as failed
  while leaving the files behind — a half-applied install that reports failure.
  Running the same transaction inside the guest works because the guest *is* the
  base plus the layer.
- **`rpm2cpio` into the layer.** No dependency solving, no scriptlets, no
  database, and no reliable way to implement `pkg remove`.
- **A container image per layer.** Correct and heavy: a second package universe,
  a second update cadence, gigabytes where the delta is kilobytes.

## Permissions are Tilo's

`perm trust-dir ~/src` mints a `collab_types::caps::CapOverride` —
`Subject::App("so.tilo.ush")`, `Scope::FsPath`, `Effect::Grant`, `Verb::Write` —
and every question is `decide_capability`. That is the same record the
compositor stores on a workspace, renders in its permissions panel and can
revoke, and the same deny-biased function `tilo-launcher` and the FUSE mount
call. There is no `ush`-local permission type: a `Restrict` written by the
compositor removes the bind here, and an expired grant does not survive into the
next session.

A new bind needs a new mount namespace, so `perm trust-dir` answers "run
`restart`". That is what the `restart` command is for.

## `scan`, `guard`, `quarantine` — and what they are not

Tilo ships no signature database, and this runtime does not provide one.

- `guard status` reports enforcement it has **tested**, not enforcement it
  configured: the Landlock line comes from attempting an operation the ruleset
  forbids. A status line that said "installed" because the install code ran
  would report success without checking enforcement.
- `scan <path>` reports package ownership, `rpm -V` agreement, executable and
  setuid bits, and whether a write there would reach the host. Every report, on
  every code path, ends with the sentence saying nothing was checked against a
  database — asserted by a test, because a `scan` that could return without it
  would sooner or later be read as a clean bill of health.
- `quarantine` is user-driven: `quarantine add`
  moves a file into the layer with its executable bits cleared, so the original
  path stops resolving. No file is moved there automatically.

## `dsh`

The same program, the same layers, the same store, the same broker — on a
profile that trades isolation for power. It keeps the layer model and the
read-only base; it gives up the syscall filter that makes `ush` a boundary,
because nested rootless `podman` and `distrobox` cannot work without `unshare`,
`mount` and `setns`.

- Namespace syscalls allowed. `ptrace`, `bpf`, `keyctl`, module loading and
  `AF_PACKET` sockets stay denied — `dsh` does not disable seccomp entirely.
- `/dev/fuse` bound.
- The capability bounding set kept. `bwrap` empties it, and a guest that can
  `unshare(CLONE_NEWUSER)` and then not write the new namespace's `uid_map` gets
  nowhere. These capabilities exist only inside the guest's own user namespace.
- The full subordinate id range mapped. The runtime builds the user namespace
  itself, has `newuidmap`/`newgidmap` write the maps from outside — both carry
  file capabilities, so no privilege is involved — and hands `bwrap` the
  namespace with `--userns`.

**`dsh` is a development profile, not a sandbox.** It prints that on
every start, and it is reachable only by typing it: an app that was handed the
service socket at launch cannot ask for one.

## Commands

```
ush [-c COMMAND] [-v] [--ephemeral]   a sandboxed shell
dsh [-c COMMAND] [-v]                 a developer shell (NOT a sandbox)
tilo-ush serve [--socket PATH]         hand guests to apps that ask

pkg install [--one-time] PKG…   pkg remove PKG…   pkg list
pkg diff                        pkg inspect PKG   pkg freeze   pkg burn
perm trust-dir DIR              perm list         perm revoke ID
scan PATH                       guard status
quarantine list | add PATH | restore PATH
restart
```

`pkg install --one-time` records the package and removes it when the session
ends; `pkg freeze` keeps it. `pkg burn` destroys the layer, deferred to the end
of the session because an overlay `upperdir` cannot be removed from under a live
mount.

## Storage

```
~/.local/share/ush/
  layers/persistent/       the layer a normal session writes to
  layers/ephemeral/<id>/   one per --ephemeral session
  sessions/<id>/           overlay workdirs, the broker socket
  audit/                   the broker's append-only log, one file per day
  policy.json              saved permission decisions
  home/                    the guest's home — not yours
```

`$TILO_USH_HOME` moves it. The store must be on ordinary disk: overlayfs will not
take tmpfs as an upper directory, and the runtime says so by name rather than
letting the mount fail with `Invalid argument`.

## Known limits

- **The guest shares the host's network namespace.** Package installation needs
  egress, and the right mechanism to broker it is
  [`launcher::netgate`](../../crates/packaging/launcher/src/netgate.rs) — an
  empty namespace and a name-matched SOCKS5 gate — which is not wired to this
  runtime yet. Until it is, a guest reaches the network the machine reaches.
- **Rootless `podman` inside `dsh` needs more plumbing.** The namespace
  primitives are there and verified — nested user namespaces work and the full
  subuid range is mapped — but podman takes its *rootful* path when `euid == 0`,
  which the guest's uid-0-in-a-userns design makes true. Running it needs either
  a guest at the real uid (which would break `pkg`, since rpm must chown) or the
  rootful storage paths provisioned in the layer.
- **A layer cannot upgrade a base package**, only add to the base and shadow it.
  See "Layers" above; `pkg` reports this explicitly.

## Verifying it yourself

```bash
cargo test -p ush          # unit + integration
```

The integration test performs the overlay mount the runtime performs before
deciding whether it can run, and reports the failure when it cannot — inside this
repository's build container it cannot, because that container's own `/usr` is a
podman overlay. Run the compiled test binary directly on the host to exercise
the guest:

```bash
cargo test -p ush --test it --no-run   # prints the binary, target/debug/deps/it-<hash>
./target/debug/deps/it-<hash> serve_handoff:: --test-threads 1
```
