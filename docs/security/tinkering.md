# Tinkering without root

*2026-07-30*

An OS without `sudo` must still provide owner-controlled system inspection and
modification. This document describes how to separate that access from ambient
root privileges.

The constraint it is built to satisfy:

> Unlocking elevated access should either be unnecessary, or preserve user data
> when it is required.

And the specific job:

> the part that provides this access without handing `sudo` to every program
> when it needs only a subset of elevated permissions.

---

## Part 1 — Findings

### 1.1 Sinty OS `ush` and `dsh`

Sinty OS (`sinty.dev`, desktop "Singularity Desktop", by Mirko Brombin) announced
its first public alpha on 2026-07-26. `ush` is implemented and its source is public:
[`github.com/singularityos-lab/ush`](https://github.com/singularityos-lab/ush),
Go, GPL-3.0-or-later, one release (`v0.1.0`, 2026-07-25, empty release notes).

**A warning about source quality before anything else.** The published prose on
`ush` is approximately 850 words total: one docs page (`/docs/ush/`, ~600 words),
one docs page (`/docs/dsh/`, ~250 words), and eight short paragraphs in the alpha
announcement. `/docs/under-the-hood` contains nothing on any of it. There is **no
third-party coverage of any kind** — no HN thread, no Reddit thread, no Lobsters,
no LWN, no Phoronix, no DistroWatch entry, no CVE, no independent review. The
repository has one star. Everything below that is more specific than those 850
words was read out of the Go source, which is substantial
(~450 KB, with unit tests and an adversarial harness) but has been reviewed by
nobody outside the project.

The design ideas are documented, but the implementation remains unvalidated.
Where this document relies on source rather than documentation, it says so.

#### Documented

From `/docs/ush/`:

> `ush` is how Sinty OS runs Linux applications. It is a locked-down runtime that
> lets you install and run desktop software without `sudo`, `pkexec`, or any
> other privilege elevator. Anything an app tries to do that crosses out of its
> sandbox goes through a broker that asks you first.

> * The host filesystem is mounted read-only. Writes land in an overlay layer
>   that belongs to you, not to the system.
> * The app runs in its own user, mount, process, network, IPC, and hostname
>   namespaces […]
> * Your home directory is write-isolated by default. An app sees your files only
>   where you have said it may.

The published CLI, verbatim:

```
ush                         # interactive shell in the guest
ush -c "command"            # run one command
pkg install htop            # into the persistent layer
pkg install --one-time htop # into a layer that lasts only this session
pkg remove htop             # remove from the layer
pkg burn htop               # destroy the package's layer files
pkg diff                    # what your layers changed versus the base
pkg inspect htop            # files, scripts, services, risk
pkg freeze                  # promote the session layer to the persistent one
pkg list
perm trust-dir <dir>        # grant a directory read-write ("Share with Linux")
guard status                # Landlock and daemon posture
```

Storage layout, documented:

```
~/.local/share/ush/
  layers/persistent/   your permanent layer
  layers/ephemeral/    per-session layers
  sessions/            active session metadata
  audit/               the broker's audit log
  policy.json          the permission decisions you have saved
```

From the announcement, on privilege:

> "Applications and interactive shells should not receive a general-purpose
> host-root context." … "The guest requests a specific operation, the broker
> evaluates it against policy and only that capability is exposed."

From `/docs/dsh/`:

> `dsh` is a convenience for development, not a sandbox. It deliberately relaxes
> the isolation that makes `ush` safe […] nested rootless `podman` and
> `distrobox`, `/dev/fuse`, and a subordinate id range are all available.

#### Established from source, not documented

- **The base is not overlaid; a Debian tools tree is grafted on.** `internal/fs/overlay.go` binds host `/usr`, `/bin`, `/sbin`, `/lib` **read-only** and overlays only `/etc` and `/var`. Packages install via dpkg `--instdir=<layer>/pkgroot`, `--force-not-root`, `--force-script-chrootless`, `--force-depends`, `--no-triggers`, and a list of `--path-exclude`s that deliberately drop Debian's own `libc.so.6` and `ld-linux*.so`. The shell then adds the pkgroot's subpaths to `PATH` and `LD_LIBRARY_PATH`.

  This is the most consequential thing in the whole investigation. It means Debian binaries in a `ush` layer run **against the host's glibc**, not Debian's, with dpkg triggers and dependency checks disabled. It explains the `internal/compat` module, which classifies packages `FULL / PARTIAL / BROKEN / UNKNOWN` against nine failure reasons — and which ships with **no populated profile data**, so every package starts `UNKNOWN`. There is no published compatibility matrix.

- **`ush` blocks the real tools.** `internal/shell/shell.go` intercepts `apt`, `apt-get`, `apt-cache`, `apt-mark`, `dpkg`, `dpkg-reconfigure` and refuses them, routing users to `pkg`.

- **Seccomp is a denylist**, default `SECCOMP_RET_ALLOW`, denying `ptrace`, `kexec_*`, `bpf`, `setns`, `perf_event_open`, and the new mount API. `mount`/`umount2` are deliberately **allowed** because the layer mounts its own overlays. `dsh` keeps only `ptrace` and `kexec_*` blocked.

- **The source admits a weakness the docs do not.** Verbatim: *"seccomp-notify CONTINUE has an inherent TOCTOU race. The exec whitelist here is best-effort / audit-grade, not a hard security boundary."* `/docs/ush/` presents the exec supervisor as one of five co-equal "independent defenses" with no caveat.

- **Landlock failure is non-fatal.** If the kernel lacks it, `ush` prints a warning and continues on namespaces + seccomp only. The docs state flatly that "Landlock enforces the read-only filesystem policy in the kernel."

- **The read-only claim is narrower than documented.** The Landlock RW set is `/tmp /run /var /etc /dev` plus `$HOME`, `$XDG_RUNTIME_DIR` and the storage dir. What is actually read-only is `/usr /bin /sbin /lib /lib64`.

- **"Even root inside is not host root" is not a Sinty quote.** The phrase does not appear in any source. The nearest is a code comment: *"every id unprivileged, the user namespace unchanged: uid 0 in the guest is still a non-privileged real uid, so the host stays non-rootable."* What guest-uid-0 is actually *for*, per the code, is mundane and practical: unmodified `apt`/`dpkg` superuser checks pass, and rootless copy-up chowns work.

- **`dsh` is off by default**, gated by a three-tier policy (`/usr/lib/ush/dsh-policy` in the signed rootfs > `/etc/ush/dsh-policy` > a user opt-in marker). The shipped image sets `optin`. None of this is documented; `/docs/dsh/` implies you just type `dsh`.

- **`scan`, `guard` and `quarantine` need `singd`, which is not open source** and whose repository does not exist publicly. The docs document those commands as though they work.

- **I could not establish when an ephemeral layer is deleted.** `RemoveEphemeral()` exists in `internal/fs/layer.go`; a grep found no caller. The docs' phrase "a layer that lasts only this session" is not backed by a mechanism I can point at. Also: no layer listing, no garbage collection, no size accounting — `pkg burn` is per-package only.

#### Inference, labelled as such

- The `--path-exclude` of libc and `ld-linux` is, I infer, a forced move rather than a preference: with host `/usr` bound read-only and only a pkgroot prefix for Debian content, a Debian libc would be loaded by the *host's* dynamic loader against host-built binaries. Excluding it avoids that at the cost of every Debian package now depending on host glibc being new enough. I have no source stating this rationale.
- The Buildroot recipe marks `ush` "Dev-image only" while `/docs/ush/` says `ush` "is how Sinty OS runs Linux applications". I cannot resolve which is current. My inference is that GUI apps go through Flatpak (which the Store page documents) and `ush` is the terminal/Debian path, but nothing states it.

#### What Sinty concludes about root, and why we diverge

`/docs/os/rooting/` describes bootloader unlock as designed and **not implemented**, and the intended flow ends in a **full data wipe**. The announcement says it is *"closer to unlocking an Android bootloader than to typing a password."*

That is precisely the thing this workstream is required not to do. Sinty and Tilo agree about the layer model and disagree about the escape hatch, and the disagreement is the whole point of the design below.

### 1.2 Prior-art comparison

| | Root to invoke | Ambient root inside | Real boundary | Discard | Layer is a first-class object |
|---|---|---|---|---|---|
| **distrobox** | no (rootless) | **yes** — the entrypoint writes `<user> ALL = (root) NOPASSWD:ALL` into `/etc/sudoers.d/` | userns + your own DAC → **effectively none** | `distrobox ephemeral`, rootfs only | no |
| **toolbx** | no | yes (image must ship working `sudo`) | same → effectively none | none | no |
| **nix shell / develop** | **no** | **no** | none, by design | **exit = discard** | yes (store paths) |
| **Flatpak** | no | no | bwrap + seccomp + D-Bus filter; degrades to nothing as permissions widen | weak | yes (runtime / SDK / extension) |
| **sysext / confext** | **yes** | n/a — it *is* the host | none | `/run/extensions/`, `--mutable=ephemeral` | **yes, the best of the lot** |
| **bootc usr-overlay** | **yes** | **yes, over the whole machine** | read-only `/usr` + composefs/fs-verity | reboot — **`/usr` only** | **no** |
| **Qubes disposable** | no | yes, and harmless | **Xen** | `save_on_stop=false` | yes (dvm template) |
| **ush** (claimed) | no | yes, in a userns | namespaces + Landlock + seccomp + broker | `--one-time` layer (mechanism unverified) | partly — persistent vs ephemeral, no listing |

Per-item detail worth carrying into the design:

**distrobox.** `--privileged --security-opt label=disable --security-opt apparmor=unconfined --ipc host --network host --pid host --volume /:/run/host/ --volume $HOME:$HOME`. It deliberately disables capabilities, SELinux and AppArmor, leaving only the user namespace. Upstream is explicit: *"Isolation and sandboxing are not the main aims of the project, on the contrary it aims to tightly integrate the container with the host."* `distrobox-host-exec` is a documented escape to the host via `org.freedesktop.Flatpak.Development.HostCommand`. Upstream has refused to make `$HOME` unsharing an option, calling it *"mandatory for Distrobox's core functionality."*

The thing to internalise: **write access to `$HOME` is equivalent to code execution as the user at next login.** `~/.bashrc`, `~/.config/autostart/`, `~/.local/bin`, `~/.local/share/systemd/user/`. There is no gap between "can write my dotfiles" and "is me". A distrobox is best modelled as the same security principal as the desktop session with a different `/usr`, and `distrobox ephemeral` does not change that, because the discarded part is the rootfs and the shared part is the home directory.

**toolbx.** Narrower, more conservative, same privilege model. `toolbox(1)` explicitly states that it provides no security beyond the usual host command-line environment. Tilo should use the same scope for this profile.

**nix.** Its user-level package operations require no authorisation, prompt or reboot, and exiting the shell discards the environment. Composition is isolated because packages do not share a prefix. Prebuilt binaries that require `/lib64/ld-linux-x86-64.so.2` fail without compatibility tooling such as `buildFHSEnv` or `nix-ld`, which introduces impurity. The store is world-readable and unsuitable for secrets; writes go through a root daemon whose `trusted-users` are root-equivalent.

**Flatpak.** `--devel` is a model of a *graduated* affordance: it swaps runtime for SDK and relaxes the seccomp filter by exactly three entries. From `common/flatpak-run.c`, the non-devel-only blocklist is `perf_event_open`, `personality` (non-default), and `ptrace`, with the reason in a comment — perf has been a CVE farm, and ptrace inside a shared sandbox lets a compromised process manipulate its siblings. A single named flag, ten auditable lines. The portal pattern is the best reusable idea in the survey: **the user's choice in the dialog *is* the grant**, and the File Chooser returns a document-portal path rather than widening the sandbox. The failure is that `--filesystem=host`, `--filesystem=home`, and `--talk-name=org.freedesktop.Flatpak` (a full silent escape via `flatpak-spawn --host`) all collapse the model with no visible cliff.

**systemd-sysext / confext.** The right object model with the wrong authorisation model. An extension is named, versioned, self-describing, and declares host compatibility via `extension-release.d` (`ID=`, `SYSEXT_LEVEL=`/`VERSION_ID=`, `ARCHITECTURE=`). `merge` / `unmerge` / `refresh` are reversible verbs leaving no residue. `--mutable=ephemeral` (systemd 256+) puts the upper dir on tmpfs. But: `/usr` and `/opt` only (confext for `/etc`), purely additive by convention, **no dependency resolution whatsoever**, version pinning brittle by design, requires root for every operation, and — the chicken-and-egg that kills it on an atomic desktop — you need a build environment to build one.

**bootc / ostree.** `bootc usr-overlay` is close to the goal: one command enables `dnf install strace`, and reboot restores the base. It requires root and unlocks `/usr` for every process on the machine for the rest of the boot. The overlay is an anonymous upperdir in `/var/tmp`, so it cannot be named, listed, exported or diffed. **`/etc` and `/var` writes escape the discard**: `dnf install` writes `/var/lib/rpm` and `/etc` too, so the RPM database and configuration persist while the binaries disappear. `ostree admin unlock --hotfix` also clones the deployment as a rollback target before persisting the overlay.

**Qubes disposables.** The gold standard for the *affordance*. `save_on_stop=false` at the storage layer plus `auto_cleanup` at the object layer: discard is a property of storage, not a cleanup script anyone can forget. The UI ideas are the valuable part — "Edit in disposable qube" on a file's right-click menu, coloured window borders so the trust level is ambiently visible, edited files written back to the originating qube so people do not route around the sandbox to save their work. **Named disposables** decouple identity from state: a stable, configured, addressable thing that is nonetheless amnesiac. And the privilege insight: you are root inside a disposable and it buys nothing, because the boundary is Xen. **Root inside is only dangerous when the boundary is weak.** Costs are brutal: 10–14 GB RAM for a modest setup, no GPU acceleration for guests as of 2025, seconds of startup latency, and preloaded disposables in 4.3 make the RAM problem worse rather than better.

**run0** (systemd 256+, a symlink to `systemd-run`) is the closest modern sudo alternative and worth three specific steals. It is **not setuid** — it asks PID 1 to create a transient unit, so privileged code starts from a clean privileged context rather than disentangling itself from a hostile one. Authorisation is polkit (`org.freedesktop.systemd1.manage-units`), so the prompt is isolated from the terminal. And it **tints the terminal background red when running as root** — persistent ambient indication of elevated state, which no sudo timeout ever provided and almost nobody copies. Its limits: not a drop-in (fine-grained sudoers rules have no equivalent), background jobs die with the transient unit, and it hard-couples privilege to systemd.

**polkit's granularity problem, stated precisely.** The *authorisation* granularity is good — per-action IDs, `auth_admin` vs `auth_self`, `_keep` variants, JS rules with access to the subject. The *action design* is where it fails, and upstream admits it: *"It is important to strike the right balance between too fine-grained and too coarse-grained action definitions."* Concretely, `org.freedesktop.systemd1.manage-units` is one action for "manage any unit" and granting it is root-equivalent; `org.freedesktop.policykit.exec` is one action for "run any program as root". There is no user-facing UI for browsing or editing grants — no Flatseal for polkit, no Android permission screen.

**ChromeOS is the structural lesson.** It ships both affordances and keeps them cleanly separated. Developer mode = "I want to modify the OS": global, persistent, powerwash on toggle, scary boot screen. Crostini = "I want a Linux environment": cheap, adds no boot-time verification steps, leaves Verified Boot intact. **Wanting a package manager does not put you on the path that weakens the OS.** distrobox and toolbx get this right by accident. bootc gets it wrong, because `usr-overlay` is the *convenient* answer and it is the one that weakens `/usr`.

### 1.3 The one hard technical constraint

Measured on this machine (kernel 7.1.3, Landlock in the active LSM list, bubblewrap 0.11):

Unprivileged overlayfs in a user namespace works, and the upper directory is
readable from outside with whiteouts visible as 0:0 character devices. But
**an unprivileged user namespace cannot write into directories owned by host
root**, even as uid 0 inside. With bubblewrap's default single-uid map, host
root appears as `nobody` (65534):

```
$ bwrap --unshare-user --uid 0 --overlay-src /usr --overlay $up $work /usr … sh -c 'ls -ld /usr /usr/bin'
drwxr-xr-x. 1 root      root       /usr        # created by the overlay, writable
drwxr-xr-x. 2 nfsnobody nfsnobody  /usr/bin    # from the lower dir: unwritable
```

A namespace grants authority over mapped ids only, and host uid 0 cannot be in
an unprivileged map. Idmapped mounts do not rescue this: mapping host uid 0
requires uid 0 to be mapped in your namespace, which is the thing you do not
have.

**Therefore a writable layer must stack on a base tree the user owns, not on the
host's `/usr`.** This is not a preference; it is the shape of the kernel. It is
also, I suspect, the real reason Sinty put Debian *inside* rather than overlaying
the host — though as noted, Sinty's answer (graft a dpkg prefix onto host `/usr`)
takes on a glibc-mismatch problem that a full base rootfs does not have.

Second constraint, from the same measurements: a real package manager needs more
than one uid. With a single-uid map, `apt` fails outright — `E: setegid 65534
failed`, `seteuid 42 failed`, `Method http died unexpectedly`. Mapping the user's
whole `/etc/subuid` range fixes it. The consequence is that layer files belong to
subordinate ids the plain host user has no authority over, so **anything that
reads or deletes layer contents must re-enter the same mapping**.

### 1.4 What Tilo already has

Read from the tree; build on these rather than duplicating them.

- **`crates/packaging/launcher/`** is the enforcement point and the pattern to follow. `sandbox.rs::build_bwrap_argv` builds a `Vec<String>` and execs `bwrap` — all mount work is delegated, which is why the whole thing is already rootless. `seccomp.rs::compile_baseline()` compiles a denylist with `seccompiler` (no libseccomp, no seccomp-notify anywhere in the tree) and installs it via `bwrap --seccomp <fd>` on fd 16. `landlock_shim.rs` re-execs the launcher inside the namespace as `__sandbox-exec` so Landlock rules can use *sandbox-side* paths; `landlock::apply_plan_or_log` (parent-side) is effectively dead.

  **The baseline filter denies `mount`, `unshare`, `setns`, `pivot_root`, `chroot`.** A layer session cannot use it unchanged. `[sandbox].seccomp` is already a *named* profile string, so the extension point is a second profile beside `compile_baseline()`, not a relaxation of the default.

- **`netgate.rs` is the design precedent to imitate.** Per-host egress enforced by a SOCKS5/HTTP-CONNECT broker over a bind-mounted socket, with the property `docs/architecture/security.md:150` calls out: it *"needs no privilege and no kernel feature bwrap does not already require."* An earlier nftables draft was explicitly retracted because it needed `CAP_NET_ADMIN` the launcher does not have. That rejected-alternative note is the shape of argument this codebase expects.

- **The typed capability vocabulary already exists** in `crates/collab/collab-types/src/caps.rs`: `Verb`, `Principal`, `Subject`, `Scope` (including `FsPath`, `Device`, `NetHost`), `Expiry`, `Effect`, `CapOverride`, and a fail-closed `decide()`. `crates/session/shell/src/shell/state.rs` aliases `Expiry` as `GrantScope` with `GRANT_ONCE` / `GRANT_SESSION` / `GRANT_ALWAYS`, where session grants are **never written to disk**. Grants live on the workspace CRDT (`Workspace::capabilities: Vec<CapOverride>`), are evaluated at `crates/session/shell/src/portal/mod.rs::permits`, and reach the launcher as a `0600` JSON file under `$XDG_RUNTIME_DIR/tilo/capabilities/` that is deliberately *not* bound into the sandbox. **Do not add a second vocabulary.**

- **The prompt UI exists**: `crates/sdk/ui-core/src/widget/permission_prompt.rs`, with Allow / Allow this session / Always allow / Deny, and "Always" disabled for non-owners.

- **`docs/app-model/sandbox.md`** documents the three layers (bwrap → seccomp → Landlock shim) and notes that `[sandbox].namespaces` is parsed and ignored. Its hostname-allowlist section describes a DNS proxy that does not exist — `docs/architecture/security.md:140-154` is authoritative.

- **`docs/app-model/trust.md`**: Ed25519 publisher identity; signed root over `manifest_hash || payload_root_hash || publisher_identity` whose BLAKE3 *is* the `PackageId`; attestations signed over an 82-byte preimage deliberately excluding the `PackageId` to avoid a fixed point; revocation checked **at install and at every launch**, with no "trusted once" cache.

- **distrobox and podman are installed in the image and completely unconfigured.** `os/Containerfile:255-268` installs `distrobox podman fuse3 bubblewrap`. There is **no `/etc/subuid` or `/etc/subgid` entry**, no `/etc/containers/{containers,storage}.conf`, no `user_allow_other` in `/etc/fuse.conf`, no podman socket. `apps/containers/` is a podman/distrobox manager built on `ui` but is not packaged and its binary is not in the image.

- **`os/config.toml` puts `quickuser` in `wheel`.** Tilo today has ordinary password `sudo`. There is one polkit rule, for the greeter's power actions. No `run0` usage. `os/dev/switch-to-stock-kernel.sh` is the tree's only `bootc usr-overlay` use, and it needs root.

- SELinux targeted and expected Enforcing, with `tilo_fs_t` enforcing too (it shipped `permissive` until its allow-set was captured from a VM run). Kargs include `lockdown=integrity`. Unprivileged user namespaces intentionally allowed (bwrap needs them). `grep -rni tinker` returned nothing — greenfield.

---

## Part 2 — The design

### 2.1 Diagnosis: usability requirements

Lockdown creates three usability problems for power users, each with a matching
design response.

1. **The mechanism is hidden.** A denial is silent and indistinguishable from a
   bug. There is no shell, so there is no way to find out *why*. → **Every
   refusal names what was attempted, what refused it, and the command that would
   allow it.**
2. **The escape hatch is punitive.** Bootloader unlock wipes the device, so the
   OS's answer to "I want to understand my computer" is "then lose your data".
   Sinty's own rooting page inherits this. → **Elevation never wipes. It is
   bounded and reversible, not a factory reset.**
3. **The default posture is powerlessness**, with power as a rare exception. →
   A full root shell with a working package manager is available immediately,
   with no prompt, because it is scoped to a layer rather than to the machine.

The design requirement is: **wanting a package manager must not put you on the
path that weakens the OS.**

### 2.2 Two mechanisms, deliberately separate

**`tinker` — work layers.** Where you are root, where the package manager works,
where you break things. Needs no privilege, prompts for nothing, and cannot
affect the next boot. This covers most package and configuration work.

**`tilo-hold` — capability leases.** For the genuinely host-level operations a
layer cannot contain: change the firewall, mount a disk, restart a unit, load a
signed module. Sub-root granular, brokered, leased, revocable, visible.

They remain separate. Conflating them gives a process more authority than the
requested operation requires, as with `sudo` and `bootc usr-overlay`.

### 2.3 `tinker`: interactive behavior

The session's hostname is the layer name and `$TILO_LAYER` carries it too, so an
ordinary prompt already says where you are. (A tint or an explicit `[layer …]`
prefix, in the spirit of `run0`'s red background, is not built — see Part 3.)

```console
$ tinker
root@scratch1:~# apt-get install -y cowsay
root@scratch1:~# echo "tuned = yes" > /etc/my-tweak.conf
root@scratch1:~# rm /etc/motd
root@scratch1:~# exit

Layer 'scratch1' is ephemeral and still on disk.
  it added 9 package(s): cowsay libgdbm-compat4t64 libgdbm6t64 libperl5.40 …
  and 6 change(s) no package accounts for — tinker diff scratch1

  keep it:     tinker keep scratch1 --as <a-better-name>
  throw away:  tinker discard scratch1
```

The commands:

| Command | Effect |
|---|---|
| `tinker` | ephemeral layer, interactive root shell, network on |
| `tinker shell <name>` | open or create a named layer |
| `tinker shell --on <layer>` | stack on a kept layer (repeatable, base-most first) |
| `tinker shell --keep` | skip the decision at the end |
| `tinker shell --offline` | unshare the network namespace |
| `tinker shell --share <path>[:rw]` | expose one host path, read-only unless `:rw` |
| `tinker shell --explain` | print the exact `bwrap` vector and run nothing |
| `tinker shell -- <cmd>` | one command instead of a shell |
| `tinker list` | every layer, base, kind, note, stack |
| `tinker diff <name>` | what the layer changed; `--all` for everything |
| `tinker keep <name> [--as NEW] [--note …]` | promote ephemeral → kept |
| `tinker discard <name>` | delete it; refuses a kept layer without `--yes` |
| `tinker revert <name> <path>` | undo one path; the base version reappears |
| `tinker base add <name> --from-image <ref>` | unpack an OCI image as a base |
| `tinker explain` | the boundary, in full, including what it does not protect |

Store layout, per user, on writable storage, outside every path the image owns:

```
$XDG_DATA_HOME/tilo/tinker/
  bases/<name>/                 unpacked base rootfs, owned by the user
  layers/<name>/upper/          the layer's writes — and its diff
  layers/<name>/work/           overlayfs scratch
  layers/<name>/home/           the layer's own home, mounted at /root
  layers/<name>/layer.toml      name, base, kind, created, stacked_on, note
```

### 2.4 The four design decisions

**(a) Ephemeral is "persistent with a pending delete", not tmpfs.**

Sinty has `pkg install --one-time` and `pkg freeze`, so retroactive promotion is
not a new idea. But the *default* matters. If ephemeral means an invisible tmpfs
overlay — which is what `bwrap --tmp-overlay` gives you, and what
`sysext --mutable=ephemeral` gives you — the work is gone before the offer can
be made, and the user must guess at the start of a session whether the next
twenty minutes will be worth keeping. That is a guess made at the moment of
least information.

So an ephemeral layer here is an ordinary layer on disk marked for deletion at
session end. It costs disk during the session and permits changes to be discarded
or promoted later.
Promotion is a **rename**, not a copy: `tinker keep scratch1 --as rust-dev` is
instant regardless of layer size.

And the empty case is handled silently: a layer that changed nothing is deleted
on exit without a word, so opening a shell to look at something costs nothing and
generates no noise.

**(b) The upper directory *is* the diff.**

No change journal, snapshot, or write-time bookkeeping is needed, so the diff
cannot drift from the filesystem. overlayfs records the required state:

- a 0:0 character device is a whiteout → the path was **deleted**, and the base
  copy is intact underneath;
- a directory with the `overlay.opaque` xattr *where the base also has that
  directory* → **replaced**, base entries hidden but present;
- any other entry that also exists in the base → **modified** (overlayfs copies
  up on first write);
- anything else → **added**.

This makes `tinker discard` a return to the base rather than an approximation.

**(c) A diff must be legible or it will not be read twice.**

The naive version of (b) reports 1880 changed paths for nine small packages, and
a user who sees that number once learns to ignore the diff forever. But almost
all of it is *explained*: dpkg writes a complete file manifest per package to
`/var/lib/dpkg/info/<pkg>.list`, and because a layer contains only files it wrote
itself, every receipt found there is by construction a package this layer added —
no comparison against the base needed.

So files are attributed to the package that claims them, and what remains is what
no package accounts for: the user's changes. Same worked example:

```console
$ tinker diff scratch1
packages (apt): cowsay libgdbm-compat4t64 libgdbm6t64 libperl5.40 libtext-charwidth-perl netbase perl perl-base perl-modules-5.40
  + /etc/hosts
  ~ /etc/ld.so.cache
  - /etc/motd
  + /etc/my-tweak.conf
  + /etc/networks
  + /var/cache/ldconfig/aux-cache
  … and 1874 more that those packages account for (--all to show)

1880 changed path(s), 72894155 bytes in the layer.
Base 'debian' is untouched; `tinker discard scratch1` returns to it exactly.
```

The six lines show the edited configuration, deleted file, ldconfig caches, and
two files written by netbase's maintainer script. They expose changes made by
package scripts as well as by the user.

Deletions are **never** attributed away, even when a package owns the path.
Removing a package's file is the user's doing and the most surprising thing a
layer can be hiding.

**(d) Diff, and therefore selective undo.**

Because the diff is the filesystem, `tinker revert <layer> <path>` is just
deleting the corresponding upper entry. Remove a whiteout and the file
un-deletes. Remove a copied-up file and it reverts. Nothing else in the layer
moves. This supports selective undo —
"my environment broke, revert the two files that broke it" without rebuilding it.

`diff` supports four operations: inspect installer changes, review before keeping
a layer, turn changes into a recipe, and revert a specific path.

### 2.5 `tilo-hold`: the sub-root answer

`sudo`'s failure is that needing one capability grants all of them, permanently,
to a whole process tree. The naive fix — prompt per operation — fails differently:
nobody will use a system that asks about every `write()`.

The resolution is that **the unit of grant is (session × capability × argument
constraint × duration), and the trigger is the first attempt, not a
pre-declaration.** One prompt per capability per session, raised lazily by the
thing that needs it. That is the ergonomic core; everything else is bookkeeping.

**Capabilities are broker-implemented verbs, not Linux capabilities.** Linux
capabilities are simultaneously too coarse (`CAP_SYS_ADMIN` is root) and too fine
(nobody thinks in `CAP_NET_BIND_SERVICE`). The vocabulary is a closed set of
operations the broker performs *on the caller's behalf* — the caller never
receives the capability:

```
net.firewall.rule        add or remove a firewall rule
disk.mount.removable     mount a removable device at a managed path
service.restart:<unit>   restart one named unit, from the image's unit set
device.usb.raw:<id>      raw access to one enumerated device
time.set                 set the clock
module.load:<name>       load one module, from the signed set in the image
usr.overlay              a transient writable /usr, in a private mount namespace
os.deploy                switch to an image reference
```

**The invariant that makes this sub-root, and the one to defend in review: no
capability in the set grants code execution as root.** `service.restart` takes a
unit name from the image's own set, not a command line. `module.load` takes a name
from the signed set, not a path. `os.deploy` takes an image reference, not a
script. This is precisely where polkit lost: `org.freedesktop.policykit.exec` and
`org.freedesktop.systemd1.manage-units` are each one action meaning "run anything
as root", so `pkexec bash` became the normal workaround and the granularity was
decorative.

**Grants are leases.** A lease is `(capability, argument constraint, holder,
expiry)`. Default expiry is the session that asked. The existing
`Expiry::{AfterUse, EndOfSession, Never}` is exactly this vocabulary, the existing
prompt widget already offers exactly these four buttons, and `EndOfSession` is
already never written to disk. `tilo-hold list` shows live leases; `tilo-hold drop`
revokes one; the same list appears in the permissions panel.

**Denial is legible.** For the user's own shell, every refusal prints what was
attempted, which capability would permit it, and the command:

```console
$ nft add rule inet filter input tcp dport 8080 accept
nft: operation not permitted

  this needs the capability   net.firewall.rule
  ask for it with             tilo-hold net.firewall.rule --why "open 8080 for testing"
  see what you hold           tilo-hold list
```

This is in direct, deliberate tension with the `security-denial-as-absence` work,
and the distinction is the resolution: **absence for untrusted code, explanation
for the human.** An app should not learn what exists by probing. A user in their
own shell must be able to find out why something failed, or they will conclude
the OS is hiding the machine — which is the entire feeling this design exists to
avoid.

**Owner operations requiring arbitrary root.** Some operations need arbitrary root: a kernel debug
session, an out-of-tree driver. `usr.overlay` is that door. It is `bootc
usr-overlay --read-only` remounted read-write **in a private mount namespace**,
so — unlike plain `usr-overlay`, which unlocks `/usr` for everything on the
machine for the rest of the boot — only the holding session sees a writable
`/usr`. It is transient by default, loud (`tilo-hold list` shows it; the terminal
is tinted, per `run0`), recorded, and **it does not wipe anything**. The denial
message names this path.

`usr.overlay` must also document a bootc limitation: **`/etc` and
`/var` writes escape the discard.** `dnf install` under it writes `/var/lib/rpm`
and `/etc` permanently while the binaries evaporate at reboot. The lease
announces this behavior when granted.

### 2.6 Where the boundary is, concretely

**Namespace root and host authority.** All of it follows from the kernel
rule measured in §1.3 — an unprivileged namespace grants authority over mapped
ids only, and host root is not mapped:

- No write to the host's `/usr`, `/etc`, boot chain, other users' data, or the
  layer store of another user. The host filesystem is not mounted at all.
- No kernel modules, no host firewall, no host devices beyond what was `--share`d,
  no clock, no host process visibility.
- **No effect on the next boot.** The base image is untouched, so `bootc upgrade`
  and rollback still work. Tinkering cannot brick the machine, and repair is
  `tinker discard`, not recovery media.
- Limits the authority of an *unprivileged* attacker who gets code execution inside.

**What it does not protect against.** The tool and documentation must state these
limits:

- **The kernel.** An unprivileged user namespace is a large interface with a long
  CVE history, and handing users one with `CAP_SYS_ADMIN`-in-userns *increases*
  the machine's kernel attack surface. The layer seccomp profile must allow
  `mount`, `unshare` and `pivot_root` — the very calls the baseline denies — so
  it is a weaker filter than an app gets, by construction. `tinker` is therefore
  intended for the machine's owner rather than as a sandbox for untrusted code.
- **Your own data.** Code in a layer runs as you. Anything `--share`d is exposed,
  and `--share $HOME:rw` is equivalent to handing over your session.
  `$HOME` is not shared by default precisely because distrobox and toolbx show
  where that ends.
- **A lease you approved.** The broker executes it.
- **What you installed.** Packages come from their repository's signing key. That
  trust is Debian's, not this machine's. A persistent layer is a persistent
  foothold that runs on every entry — which is the security argument for
  ephemeral-by-default, independent of the ergonomic one.
- **Resource exhaustion.** A layer can fill the disk. No cgroup limits yet.
- **Multi-tenancy.** Same-user confinement only. Not a boundary between users.
- **A `dsh`-style profile, if one is added.** Nested rootless podman means nested
  userns plus `/dev/fuse` plus subuid — a much larger surface. Sinty says out
  loud that `dsh` is not a security boundary; any Tilo equivalent must say the
  same, in the tool's own output, not only in docs.

### 2.7 Why this design is usable

The design has six usability requirements:

1. **The common case needs no permission at all.** This is nix's key property:
   not the store, but that there is nothing to authorise. `tinker` prompts for
   nothing, ever. A power user's first ten minutes involve zero dialogs and a
   root prompt.
2. **Standard tools work.** `apt` remains `apt`. Sinty blocks `apt`, `apt-get` and
   `dpkg` and routes users through `pkg`. `tinker` wraps the environment, not the
   package manager.
3. **Nothing is hidden.** `--explain` prints the literal `bwrap` vector that
   runs. `tinker explain` states the boundary *and* its holes. Denials name the
  capability and the command, so users can inspect the boundary and its
  exceptions.
4. **Elevation exists, is reachable, and never wipes.** Sinty's rooting path
   ends in a full data wipe, while `tilo-hold` is bounded, revocable, visible,
   and requires a prompt.
5. **Users can change their mind.** Retroactive `keep`, per-path `revert`, and a
   discard that returns exactly to the base. Nothing about a session is a
   commitment made in advance.
6. **The blast radius is legible.** "This cannot affect your next boot" is a
   claim a user can check — and `tinker diff` plus an untouched base is the
   check.

Limitation: a layer is not the host, and someone whose goal is *to modify this machine's `/usr`*
is being offered something adjacent to what they asked for. §1.3 says that cannot
be fixed unprivileged. The mitigation is that `usr.overlay` exists, is named in
the denial, and does not wipe — the interface is explicit. Whether that is
sufficient depends on user needs.

### 2.8 What to reuse, and what has to change

Reuse:

- `launcher::sandbox`'s pattern — build argv, exec `bwrap`, delegate all mount
  work. It is why this is rootless.
- `landlock_shim`'s `__sandbox-exec` re-exec, to apply Landlock with
  sandbox-side paths. The host-side/sandbox-side path distinction is the
  number-one correctness trap in that code.
- `netgate` verbatim for a layer's egress: the same per-host SOCKS5 broker, so a
  layer's network is filtered by the same mechanism an app's is, with the same
  no-privilege property.
- `collab-types::caps` as the lease vocabulary. `Expiry` already *is* lease
  duration. Add `Scope::Elevated(ElevatedCap)` rather than a second enum.
- `crates/sdk/ui-core/src/widget/permission_prompt.rs` for the lease prompt.
- The `0600` capability-file handoff under `$XDG_RUNTIME_DIR/tilo/capabilities/`,
  which already deliberately does not let the sandbox see its own permissions.

Must change:

- **A second seccomp profile.** `compile_baseline()` denies `mount`, `unshare`,
  `setns`, `pivot_root`, `chroot`. Add `compile_tinker()` beside it; do not relax
  the default. `[sandbox].seccomp` is already a named profile string.
- **`/etc/subuid` and `/etc/subgid` must be provisioned by the image.** Nothing
  in `os/` writes them today, and without them `apt` fails in a layer and
  rootless podman cannot work either. `os/usr/libexec/tilo-firstboot-user` is the
  place.
- **`sudo` should go.** `os/config.toml` puts `quickuser` in `wheel`, so Tilo has
  ambient password root today. Removing it depends on `tilo-hold` covering the
  operations people need; remove it only after that replacement is available.
- **`docs/app-model/sandbox.md`**'s hostname-allowlist section still describes a
  DNS proxy that does not exist. Fix it while nearby.
- **Base images need to ship in the OS image**, or first `tinker` needs a
  network pull. Sinty pre-bakes an apt/dpkg tools tree at
  `/usr/share/ush/tools` for the same reason.

---

## Part 3 — Prototype status

`crates/system/tinker/`, 42 tests, all green.

```bash
cargo build -p tinker && ./crates/system/tinker/demo.sh
```

`demo.sh` drives the entire loop from a cold start and fails if the base
moved, so the load-bearing claim — that tinkering cannot affect the next boot —
is checkable rather than asserted.

**Working, proven end to end against a Debian trixie base with no privilege
at any point:**

- `tinker base add debian --from-image docker.io/library/debian:stable-slim` —
  pull and unpack via rootless podman.
- `tinker` — ephemeral layer, uid 0 inside, `apt-get update && apt-get install`
  succeeds, network and DNS work.
- `tinker diff` — package attribution and change classification; the worked
  example reduces 1880 raw entries to 6 meaningful lines.
- `tinker keep scratch1 --as cowbox --note …` — retroactive promotion by rename.
- `tinker revert cowbox /etc/my-tweak.conf` — one path undone, the rest of the
  layer intact, including a deletion made in the same session.
- `tinker shell buildbox --on cowbox` — stacking; the upper layer sees the lower.
- `tinker discard` — refuses to pull a layer out from under a stack; after
  discarding everything the base is byte-identical by structural fingerprint.
- `--explain`, `tinker explain`, `tinker list`.
- Transparent re-entry into the subuid mapping for every command that touches
  layer contents.

**On paper only:**

- `tilo-hold` entirely. No broker, no lease store, no `ElevatedCap` enum, no
  prompt wiring. This is the larger and more contentious half of the design.
- Reuse of `launcher`'s seccomp and Landlock. The prototype's sessions are
  namespace-confined but carry **no seccomp filter and no Landlock ruleset** —
  they are strictly weaker than the design describes. `compile_tinker()` does not
  exist.
- `netgate` integration; a layer's network is currently all-or-nothing.
- `tinker adopt` — emitting a reproducible recipe (or a sysext, or a `.tapp`)
  from a layer. This is the loop-closer that nix has and containers do not, and
  it is unbuilt.
- Compositor and UI integration: no terminal tint, no permissions-panel lease
  list, no Qubes-style "open this in a layer" affordance.
- Image work: subuid provisioning, shipped base images, `sudo` removal.
- Disk accounting and garbage collection.

**Unresolved question.** A layer is not the host, and §1.3
says no amount of cleverness makes it the host without privilege. So the design
routes host-level cases through `usr.overlay` — and `usr.overlay`
is the one capability in the set that *does* grant arbitrary root code execution,
because a writable `/usr` in your own mount namespace is a writable `/usr`. It
therefore violates the invariant that §2.5 leans on for everything else.

An open question is whether a private-mount-namespace writable `/usr` provides
sufficiently narrower authority than `sudo`. It is scoped to one session rather
than the machine, leased with an expiry, listed, and revocable. However, a
session that can write `/usr` can drop a binary that the next boot's *host* will
run, so a transient write does not preserve integrity. One option is to make
`usr.overlay` a separate, louder, attested operation closer to
`ostree admin unlock --hotfix`, which clones a pristine rollback target first.
This remains unsettled.
