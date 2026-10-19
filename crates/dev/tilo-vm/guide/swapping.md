# Swapping components

```sh
tilo-vm swap shell                        # your slot, built from this worktree
tilo-vm --target review swap shell --profile dev          # faster compile, slower binary
tilo-vm swap --slot my-slot launcher      # an explicit slot
tilo-vm swap --slot vm fs                 # a VM-wide component into the vm slot
tilo-vm swap app:notes                    # one app, installed into your slot's app store
tilo-vm swap shell --from ci:main-1a2b3c4 # the CI build of a published image
tilo-vm swap shell --from sha:1a2b3c4     # the CI build of a main commit
tilo-vm swap shell --from layer:<hash>    # a layer already in the VM
tilo-vm swap kernel                       # the kernel: build, install VM-wide, reboot (guide kernel)
tilo-vm rollback                          # the generation before the current one
tilo-vm rollback --forward                # the next newer one again
tilo-vm rollback --to <generation>        # any generation, older or newer
tilo-vm rollback --slot vm                # the vm slot, the kernel included
```

Rolling back never deletes a generation, so `rollback --forward` (or
`--to <n>`) goes forward again until `gc` prunes it; `tilo-vm slot show`
lists them, generation 0 (the bare image) included.

`tilo-vm --json slot show` includes `provenance`, keyed by selected component,
read from each imported layer's metadata. Compare `source_commit`,
`dirty_tree_hash`, `target`, `toolchain` and `cargo_lock_hash` with the selected
`apps+runtime` cohort before qualifying a replacement. Profiles can differ.
Older daemons/imports may omit provenance; that is unknown, not proof of a match.
The import record is diagnostic metadata; package authenticity still comes from
the existing signed installer and the OS publisher pins.

For a captured fresh-slot replacement journey, the executable evidence contract
is `scripts/vm-acceptance/component-replacement.py` (instructions in that suite's
README). It checks successful activation, failed-candidate recovery, isolated
fixture state and teardown, and never performs deployment itself.

The one positional argument is the component; the slot is `--slot` and
defaults to the one named after your branch. Components and their scope
come from `os/components.toml`
(`python3 os/scripts/install-components.py --list`).

## A daemon at another protocol

Every operation requires the daemon's protocol to be the client's. A device
whose daemon speaks another one is updated with its image (`tilo-vm vm upgrade`).

## Build profile

A build from your worktree uses the component's manifest profile (`release`
for almost every component; `apps+runtime` uses `thin-app`) with the
manifest's features plus each package's `dev-overrides` feature. So the
binary runs at realistic speed and still honours the harness variables
(`TILO_TEST_CONTROL` and friends) that release builds without the feature
ignore. A `release` layer links as a dev image links its own binaries: without
LTO and with sixteen codegen units, so a warm rebuild recompiles the changed
crates and links them without re-optimising the whole program (published
images use thin LTO).
`--profile dev` compiles much faster and runs slower; use it while
iterating on logic, not for performance or timing work. The layer's
provenance records the profile. `--profile` applies only to
`--from worktree` components, not to apps or CI builds.

Inside the VM, native GNU/Linux builds with `--profile dev` or
`--profile test` use the LLVM linker bundled with Rust. The C compiler
frontend and the target's existing linker flags, including hardening, are
preserved. This avoids GNU ld's memory pressure when linking large debug
binaries. The first build after switching linkers can rebuild cached outputs;
later builds reuse the same driver and each worktree's own build directory.
An explicit `CARGO_TARGET_<TRIPLE>_LINKER` environment variable on the daemon
overrides this choice. Other profiles and host builds keep their existing
linker selection.

## Builders (`--from worktree`, the default)

`--builder vm` (default) works from every host:

1. rsyncs the worktree into the VM (`/var/lib/tilo-vm/src/<worktree-id>/`),
   honouring `.gitignore`; `.git` and `target/` stay behind;
2. runs there `cargo build --locked --profile <profile> -p <packages>
   --features <manifest features, dev_features, pkg/dev-overrides>` (see
   *Build profile* above) with the dev image's toolchain and the VM's shared
   sccache; the output streams live;
3. lays the layer out with `os/scripts/install-components.py` from the synced
   tree and imports it in the VM. Nothing travels back to the host.

Each worktree has its own build dir in the VM; `gc` evicts old ones.

Builds run as whoever owns the build root (`/var/lib/tilo-vm/src`): root in a
VM, the laptop's owner on a laptop whose build root is a directory of theirs
mounted there. Only laying the layer out and installing it keep the daemon's
privilege, so a swap leaves no root-owned files in the build dir or the
compiler cache, and the owner's own `cargo` and sccache keep working beside
it. Before the first such build the daemon gives back root-owned files earlier
daemons left and stops a root sccache server; `tilo-vm doctor` lists any
worktree `target/` or compiler cache that still holds root-owned files, with
the `chown` that fixes it.

`--builder host` builds with this host's cargo (or `TILO_VM_CARGO`, any cargo
drop-in, such as one that builds on another machine) and uploads the layer. It
needs a Linux host of the VM's architecture; macOS and Windows hosts use the
VM builder. Without `--builder`, a VM image that lacks cargo, rsync or
python3 makes a Linux host build locally, with a warning before the build
starts.

`--builder remote` builds on a remote builder of the machine's architecture
and streams the layer through this host into tilo-vmd's upload, so no binary
is ever written to this host's disk. The builder is any cargo drop-in whose
`run` runs the program on a machine of that architecture, in this worktree,
and preserves its framed base64 stdout:

1. `<builder> run -p tilo-vm -- layer-pack --component <name> ...` runs
   tilo-vm's packer there: the same `cargo build` as above (its output
   streams here), then `os/scripts/install-components.py`, then the layer
   tar on stdout as base64 lines with its sha256;
2. tilo-vm decodes those lines straight into the upload and checks the
   sha256 and size before it imports. A remote `build` that brings its
   binaries back would put every one of them into `target/` instead;
3. the layer records `builder = "remote"`, and the target and rustc read
   from the binary on the builder.

A repeated remote build offers at most eight content hashes that the target
has verified for that component and architecture. The builder still assembles
and checks the requested source; when its canonical content matches an offer,
it sends only a checked cache result and no archive. The target verifies the
stored bytes again before reuse. The receipt records `upload = "skipped"`
and `layer_reused`. Changed content takes the ordinary streaming/import path.
For `swap userspace`, an already-active identical generation returns unchanged
without restarting the session only after rechecking every layer's bytes and
provenance, the signed cohort, current app selections, selected runtime trust
and session readiness. Missing or different selections take the normal install
and refresh path. This avoids resetting live tasks on an exact repeat; it does
not eliminate remote assembly time. For a small edit, swap just `shell` or the
app rather than rebuilding the whole userspace cohort.

Older daemons without `layer.verify` also use that path. Cache loss during the
build causes one retry without reuse; a damaged existing content-addressed
layer is refused rather than activated or silently overwritten. The first
transfer is still required.

Which builder serves which architecture is set once in `vms.toml`, as the
drop-in's command line up to the cargo subcommand:

```toml
[remote_builders]
x86_64 = ["remote-cargo", "--on-builder"] # any cargo drop-in that runs remotely
aarch64 = ["remote-cargo", "--on-builder"] # configure it for an aarch64 builder
```

Remote assembly and remote VM execution are separate capabilities. Hosting a
KVM guest on the builder additionally requires an accessible `/dev/kvm`, nested
virtualization when the builder is itself a guest, and the supported VM adapter.
Without that hardware, keep the KVM guest local and stream targeted component
layers from the remote builder; a build cache cannot supply virtualization.

The repository ships one backed by t (`docs/t.md`): `dist/dev/t-builder.sh`
hands the `layer-pack` command to t.toml's `tilo-vm-pack-<arch>` check, which
runs it on a t node of that architecture against this worktree (uncommitted
changes included) on the server's warm builds, and writes the framed layer it
downloads to stdout. Nothing compiles on this host:

```toml
[remote_builders]
aarch64 = ["dist/dev/t-builder.sh", "aarch64"]   # relative to the worktree
x86_64 = ["dist/dev/t-builder.sh", "x86_64"]
```

The framed layer waits in `~/.cache/tilo-vm/` while it streams to the
device and is deleted afterwards; nothing lands in `target/`. One t-backed build per worktree at a time: the request sits in
the worktree's `.tilo-vm-pack/` while it runs.

For a device whose architecture is listed there, `--builder remote` is the
default. For one that is not, `--builder remote` fails and names the line to
add.

## CI builds (`--from sha:` / `--from ci:`)

`sha:<commit>` means `ci:main-<sha7>`. The component's manifest `files` are
copied out of CI's dev image of that build,
`registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci` (`pr-<n>-<sha7>-ci` for pull
requests), the image `vm upgrade --from sha:<commit>` moves a VM onto: the CI OS
image's binaries with the dev-overrides rebuilds (the shell) on top, so
`control` and `shot` work. Each tag is one index over the architectures that
built, and the copy is for the VM's (or device's) own architecture, into a
layer with `authority = "ci"`. The VM pulls it when its image has podman;
otherwise this host does (needs podman here). Tilo images on
`registry.tilo.so` are public, so neither pull needs a login. Whether a
commit built your architecture:
`skopeo inspect --raw docker://registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci`
lists the platforms; the aarch64 lane builds the newest main commit only, so
an older one may lack arm64.

## Apps (`app:<dir>`)

`swap app:notes` builds `apps/notes` with `tilo-app package` (self-signed
`.tapp`) and installs it as your slot user with
`tilo-install install --allow-self-signed --force`. The user store shadows
the image's copy, so it is live the next time the app launches; nothing
restarts. App installs are not generations: `rollback` does not undo them.

On a physical device the app builds on this host when it is a Linux machine
of the device's architecture (`--builder host`, and the default when no remote
builder is configured for it), else on the remote builder; the device itself
never compiles an app. Either way the device's installer checks the package's
architecture, publisher and runtime ABI.

The app links the image's own `libruntime_dylib.so`, so the worktree's
`Cargo.lock` must be the image's. When it is not (your branch is newer than
the image), `swap app:<dir>` refuses before building and names
`tilo-vm swap apps+runtime`, which builds the apps with their matching runtime
and, being a generation, can be rolled back.

## What restarts

| Component scope | Goes into | What happens |
|---|---|---|
| `session` (shell, launcher, user services, runtime) | your slot | a normal session restart: log out, restart your user manager, log in. A laptop's `device` slot (the owner's login) instead merges VM-wide and restarts the desktop and the component's user services in place; the owner stays logged in (`tilo-vm guide devices`) |
| app (`app:<dir>`) | your slot's app store | nothing restarts; live at next app launch |
| `vm` (fs, greeter, system services) | the `vm` slot | signed and merged by the system extension activation (`tilo-vm guide trust`), its active units and the live owner's active user services restart, and **every slot's session restarts** |
| `boot`, swappable (the kernel: `dev_swap = "reboot"`) | the `vm` slot, plus a boot entry | modules merge as a signed layer, the kernel becomes the next boot entry, **the VM reboots**; confirmed, it becomes the default (`tilo-vm guide kernel`) |
| `boot`, scenario-only (UKI, bootloader, initramfs, installer, rollback) | nothing | refused; use the scenario driver the error names (the installer: `tilo-vm scenario install`, `tilo-vm guide scenarios`) |
| not a component (Containerfile, units, `/etc`, packages) | the VM's image | `tilo-vm vm upgrade --from local`: new image, reboot (`tilo-vm guide images`) |

Component services use `try-restart`: an installed inactive optional service
stays inactive. Missing, masked or unloadable declared units and manager/restart
errors still fail the swap. Explicit slot/session restart continues to start its
required session and user manager; it is a separate lifecycle operation.


A VM-wide swap interrupts other agents' sessions, and a kernel swap reboots
the VM under them, so prefer session components in your own slot.

A kernel builds on this host, never in the VM: `--builder`, `--profile` and
`--from ci:` do not apply to it, while `--jobs` and `--direct` apply only to
it. `tilo-vm guide kernel` has the whole loop.

## Verify your swap

The session sees your slot's layers stacked over the VM's `/usr`; nothing
else does, so check from inside the slot, never with `vm ssh`:

```sh
tilo-vm slot show                              # generations, and each component's files with sha256 and layer
tilo-vm slot show --json | jq '.files[] | select(.path == "/usr/bin/shell")'
tilo-vm slot exec -- sha256sum /usr/bin/shell  # the file the session runs
tilo-vm slot exec -- shell --version
tilo-vm slot exec --slot review-a -- sh -c 'ls -l /usr/lib/tilo | head'
```

`slot exec` runs the command in the mount namespace of the slot's running
session, as its user `tds-<slot>`, so it sees exactly the files the session
does. Output streams back; the command stops after `--timeout` seconds
(default 60, exit 124) or 1 MiB of output (exit 125), and its own exit
status is passed on. The slot's session must be running
(`tilo-vm slot restart` if not). Compare the sha256 with your build
(`sha256sum target/release/shell` for a host build); `tilo-vm logs` shows
what the restarted session printed.

## Checks

- Each layer's provenance is checked against the VM image's
  `/usr/share/tilo-vm/base.json`. Refused: another target triple (or CPU
  architecture) or another builder image; the error names the field and
  both values. A dev image names no builder image (it is its own builder),
  so there the target and toolchain stand for it. Warned: a different base
  image, toolchain or `Cargo.lock`. An individual thin app must match its
  installed toolkit and another `Cargo.lock` is refused. A complete
  `apps+runtime` layer may carry a different graph only after the daemon verifies
  every application's signed payload, exact native runtime ABI, architecture,
  bundled interpreter and closed library graph. Target and builder-image
  mismatches remain refusals; never regenerate `Cargo.lock` to match.
- Local builds (`vm`, `host`) are for testing and never promoted; only CI
  builds are.
- Swapping the same layer twice does nothing. A failed apply leaves the slot
  on its old generation; rerun the swap.

## Explicit targets and failed starts

Use `--target <registered-name>` on any command to select a development VM or
already-enrolled development machine for just that invocation. It overrides
`TILO_VM`, `.tilo-vm` and the registry default without changing any of them.
An unknown target or a conflicting positional VM name fails before connecting.
For example: `tilo-vm --target review rollback --slot my-slot`.

A runtime swap becomes current only after its restarted sessions pass the
existing readiness checks. A failed activation restores the previous generation
and checks it once. The error includes the original failure and whether recovery
succeeded. If recovery also fails, inspect the journal and retry the named prior
generation; the pending marker remains for diagnosis. Neither recovery path
resets account identity or application data. Kernel changes retain their separate
boot-confirmation lifecycle.

### Matched userspace replacement

```sh
tilo-vm swap --slot my-slot userspace --builder remote
```

`userspace` builds the matched session components and `apps+runtime` from one
verified worktree inventory, using each component's canonical profile and the
registered remote builder for the target architecture. It stages every member
before asking the daemon to activate them together. Host/VM builders, custom
profiles, image sources and the VM-wide slot are not supported for this command.

The daemon checks the admitted session identity and expected generation before
selection, verifies the signed cohort in its candidate view, and preserves held
apps and the session's existing application-store location. Publisher rotation
keeps trust only for exact retained runtime identities, not every runtime signed
by an old key. One readiness-checked session refresh commits the generation;
failure restores prior app selections and the prior view through normal recovery.
Account identity and application data are not reset. A receipt distinguishes
staging, activation and recovery; missing verification fails rather than skipping.

The complete toolkit carries `lib/system-app-cohort.json` in its signed payload.
It names the exact system application IDs declared by the verified source and
binds them to that source inventory's digest. Admission requires equality with
the candidate registry: a missing source member, extra member, duplicate ID or
changed inventory is refused before activation. An older image can therefore
retire obsolete system packages by replacing its read-only system registry;
user-owned packages, held selections and application data are not deleted. Their
verified old runtimes remain subject to the normal retention checks. Legacy
layers without signed membership cannot authorize retirement, and cannot serve
as a source-bound `userspace` member. Switching back restores the previous
system registry through the existing generation lifecycle.

Older daemons without `userspace.apply` must be updated through the normal daemon
component swap first. Individual component swaps and `app:<dir>` installs remain
available for narrower changes; they are not an atomic userspace replacement.

### Worktree apps and their matching runtime

`tilo-vm swap --slot device apps+runtime` is the loop for unpushed app or
toolkit changes on a real board laptop, whose sealed image only CI produces.
With no remote builder configured it builds on this host when the host is a
Linux machine of the device's architecture (`--builder host` asks for that
explicitly); otherwise it uses the `[remote_builders]` entry (`--builder remote`).
`--builder vm` is refused. The same `layer-pack` assembly runs either way, so
the layer is identical and its provenance records `builder = host` or `remote`.
Trust is never widened: the cohort is signed with an ephemeral key that is not
exported, and tilo-vmd accepts it only after verifying the key pins carried in
the layer, every signed package, the runtime ABI and the loader closure. Nothing
is added to `/etc/tilo/runtime-bundle-trusted-publishers.txt`, and
`launcher`/`app-host` keep ignoring `TILO_RUNTIME_BUNDLE_DIR` unless built with
`dev-overrides`. `tilo-vm rollback` returns to the previous apps and runtime
unless the user packages it installed require the newer SDK (it says so).

`tilo-vm swap apps+runtime --builder remote` builds every packaged native app and
its toolkit in one Cargo graph on the configured builder for the slot's actual
architecture. It verifies the complete source inventory before and after the
build, signs and verifies the payload-derived runtime ABI, checks loader closure,
and packs apps through the normal installer. Ephemeral signing keys stay in
private build staging and are not exported.

The same invocation updates the existing `launcher` and `install` session
components from that source before activating the cohort. These updates restart
the selected session; a failure stops the remaining steps and is recorded in the
normal receipt. Installed component pins must be root-owned and immutable;
operator `/etc` trust remains effective. No OS image or kernel reboot is needed.
Older dev images need the matching daemon once, through the normal VM component
update (`tilo-vm swap vmd --slot vm --builder remote`); a refused cohort leaves the
existing apps/runtime active. Its receipt records any already applied session
prerequisites.

`swap app:terminal --builder remote` retains the ordinary app installer and its
runtime checks. A standalone package whose authored runtime is absent is refused;
use the paired component above for an unpublished worktree toolkit rather than
relabeling it as the VM's existing ABI.

Without a remote builder (CI, or a host whose architecture has none) the same
cohort comes from CI: dispatch vm-acceptance with `ref=<branch>
swap=apps+runtime prebuilt_cohort=true`. CI builds the branch's launcher,
install, shell and apps+runtime as signed layers, and `tilo-vm swap-artifact`
installs them in that order through the daemon's normal signed-cohort checks,
from a checkout at exactly the producing commit
(`scripts/vm-acceptance/README.md`, `docs/layer-artifacts.md`). `--builder host`
stays refused for apps+runtime: it cannot assemble the paired, signed cohort.

### Shell startup companions

A source shell swap installs both `/usr/bin/shell` and the matching
`/usr/libexec/tilo-session` from that source tree. The session wrapper is a
mandatory input: an incomplete build fails before a layer can be imported.
Published image swaps extract both paths from the same selected image.

For a development slot, the normal swap restart opens a fresh PAM session and
runs the matching wrapper, preserving its account and application data. The
wrapper initializes that new login's recovery channel; an in-place compositor
restart keeps the existing login's lock state. Never repair a locked session by
writing a recovery record. An already-running owner login must use its normal
logout/login flow when its wrapper needs updating.

To repair a shell-only swap on an older development image, rebuild the paired
component from the matching source in the existing worktree:

```sh
TILO_VM_CARGO="$CARGO" tilo-vm swap shell --slot my-slot --builder host --profile dev
```

Select the registered VM with `TILO_VM` (or `--target` on a current client).
Set `CARGO` to your Cargo drop-in when using a remote builder. The host path
packages and uploads its received binary
and this worktree's mandatory startup companion. The slot restarts through its
normal PAM unit; no OS image rebuild, account reset or lock override is needed.

### Updating the guest daemon

`tilo-vm swap vmd --slot vm --builder host --profile dev` imports the
replacement through the existing authenticated route, then opens its RPC
from that immutable imported layer before applying the normal generation
transition. An already-running RPC process would keep executing its old
mount-composition code even after its binary on disk was replaced. The
handoff preserves its state root and configured RPC flags, checks the root
and architecture in the new handshake, and records `daemon-handoff` and
`applying_daemon_layer` in the receipt. A failed handoff applies no generation.

This needs the existing SSH route; a peer-only connection reports a clear
error before activation. No registered daemon command or access policy is
changed. The update still restarts affected sessions through their normal
units: checkpoint shared work and agree on a maintenance window first.
Stand-in views include the candidate VM layers and their private identity
environment before restarting; readiness still precedes committing the new
generation, and failure uses the existing recovery path.

### Repeating a host-built component swap

After assembly, `--builder host` computes the same content identity as the
layer importer and asks the target for its existing layers. If it already
holds the identical component, the command skips packing and uploading it.
The receipt records `digest`, `layer_lookup`, `upload = "skipped"` and
`layer_reused`. Existing layer provenance is retained, as on a duplicate import.
Changes to file bytes, executable bits or link targets require an upload;
timestamps alone do not. This is whole-layer reuse, not a binary delta, so a
wrapper-only change still uploads its newly assembled layer. Imported content
must match the local digest before it can proceed to swap. A cache hit uses
the existing RPC protocol and works with older compatible daemons.

### Layer transfer

Component archives use the existing authenticated daemon connection. New clients
on byte-transparent SSH/subprocess transports try the optional protocol-8 `layer.upload.binary` method. A ready response starts
bounded raw chunks (at most 1 MiB each, 8 GiB total); a footer supplies the exact
byte count and SHA-256. The final response must match the original request id
and byte count. No per-chunk JSON, base64 or round-trip acknowledgement is needed.

`layer-pack --binary` can produce binary framing for a byte-transparent caller.
Automatic remote component builds retain base64 framing for compatibility
with cargo drop-ins that present stdout as text. The SSH upload leg alone
uses binary framing; binary builder output requires a byte-transparent
transport.
Source inventory, target, canonical content identity and signed-cohort checks are
unchanged. Default `layer-pack` output remains the existing base64 format; its
reader accepts either encoding. An older daemon that rejects the optional method
before receiving archive bytes gets the existing serial base64 upload. Authority,
version, I/O or integrity failures never trigger that fallback.
The existing device-admin peer bridge retains serial JSON uploads and its
per-request accounting; the client defaults to legacy framing unless a known
byte-transparent transport explicitly enables binary uploads.

A source failure sends a cancellation frame and drains the final response, so the
same connection can continue. A partial transport failure poisons the connection;
truncation or loss of framing closes the daemon stream. Interrupted, cancelled or
corrupt binary uploads are removed before they can be imported. Import still
validates and hashes the staged tree before atomically publishing it; applying or
rolling back a generation uses the same paths.

The isolated benchmark `cargo run --release -p tilo-vmd --example transport-bench -- PATH [--archive]` takes a representative
executable path (packed once), or an existing archive with `--archive`, and measures
legacy, production mixed and optional fully binary framing and
actual daemon upload/import over private Unix sockets. It requires its artifact;
missing input fails. It reports artifact identity, wire bytes, transport time and
import time and checks equal imported content hashes. These measurements describe
codec and daemon transport costs, not live SSH, VM or device performance.

Host-build receipts record `assemble`, `digest`, `layer_lookup`, `pack`,
`transfer` and `import` separately. Use those phases to distinguish packaging,
transport and validation costs. CI layers are cached by image digest on the
host. Source builds hash the assembled tree and check the device for matching
component content before packaging or sending bytes. A cache hit skips both
packaging and upload; changed content uses the full uncompressed archive.

## Opt-in phase CPU records

`tilo-vm swap shell --frame-cpu-trace --slot <slot>` enables the shell's
phase CPU companion on an owned headful or headless stand-in slot. The flag is
not supported for device logins, VM-wide components or applications. It is a
typed generation setting, rather than an arbitrary environment override:
swapping the same layer with a different setting creates a new generation;
rollback restores that generation's setting. A subsequent shell swap without
the flag disables it. Protocol 9 requires a matching CLI and daemon, so an old
daemon cannot silently ignore the request. Update tooling through its normal
lifecycle before using the option; this does not require rebooting the VM.

Each compositor start, including a wrapper-requested desktop restart, allocates
a new owner-private `frame-cpu.*` directory beside the normal session logs.
`frame-cpu-path` (mode 0600) names the current CSV; the CSV is created exclusively
by shell at mode 0600. Read the pointer through `tilo-vm slot exec`, then collect
that file with the stock `frame-trace.csv`. Records are retained for evidence;
remove unwanted records from the slot's own state directory after collection.
The stock trace schema does not change, and disabled sessions allocate no CPU
record. This records CPU phases, not GPU-exclusive time or hardware performance.

`tilo-vm --json slot show` includes `installed_builds`, keyed by component, with
the selected immutable layer's source commit, compiler/toolchain, target, build
profile, Cargo.lock digest and builder provenance when recorded. Unavailable selected layers are explicit `null`; unrecorded provenance fields
remain unavailable. `identity_scope` is
`installed_layer_not_running_process`: this receipt describes installed build
artifacts and never certifies the running process's executable bytes. An OS
permission denial when reading a process image remains an observation gap.
