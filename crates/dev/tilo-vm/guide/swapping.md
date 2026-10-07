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

The one positional argument is the component; the slot is `--slot` and
defaults to the one named after your branch. Components and their scope
come from `os/components.toml`
(`python3 os/scripts/install-components.py --list`).

A canonical `vmd`-only change merges the management binary for the next RPC
without restarting running desktops. This applies to its first addition,
replacement and removal. Unexpected layer files, executable/link metadata,
declared services or other changed components retain normal refresh behavior.
Explicit slot restart commands still refresh sessions. Installing this behavior
through an older daemon uses that older daemon's activation policy once; the
receipt reports any desktop refresh it performed.

## Matched userspace cohort

```sh
tilo-vm swap userspace --slot device --builder remote
```

This worktree operation uses one frozen source inventory to build and import
launcher, installer, apps plus their exact SDK, AppHost, NativeRuntimeHost,
control, app CLI, ush and Shell. It requires a daemon advertising
`userspace.apply`; an older daemon is refused before any build or activation.
VM-wide services, the greeter, image and kernel are separate operations.

Every import must finish before activation. Changed source, target architecture,
component identity, generation or owner login refuses the cohort. A byte-identical
reused layer may retain its original import provenance; the receipt separately
records the actual candidate source inventory and each exact imported hash.

The maintained installer selects authenticated cohort packages in the candidate
readonly view, preserving user holds and pins. Old selected SDKs are retained as
runtime-only signed closures, with immutable trust scoped to their exact version,
architecture, root and publisher. Old app catalogues and global publisher pins
are never retained as overlapping layers. An unverifiable closure refuses before
the owner desktop changes. An intervening selection needing an unadmitted closure
also refuses the final refresh, leaving the staged generation pending.

The owner's greetd login is preserved and its desktop refreshes once after
selection, so freshly restored apps use the selected SDK. Ordinary dev slots
restart once through their existing session path. A partial selection failure is
reported with its completed selections, retained closures and recovery refresh;
it is never reported as a successful install. There is no reboot.

The aggregate receipt separates each component's header/build wait (`build-N`),
verified archive reception and local capacity scan (`receive-N`), device transfer,
import and stage total. `cohort_archive-N` records the verified raw archive bytes
and unpack storage estimate; a refused archive records elapsed reception but no
successful byte claim. Reception includes framing/hash checks and local I/O, so
it is not a pure network measurement. `cohort-stream-finish` and
`cohort-builder-exit` account for the final framing and producer exit wait.

All nine archives are received before capacity admission and device transfer.
Stage totals contain transfer, import and intervening source checks; apply totals
contain daemon validation, closure preparation, selection and refresh timings.
Do not sum nested entries. The cohort uses one remote builder admission and one
metadata snapshot on each side. Each build retains its canonical profile,
features, installer and independently verified archive. The admission count does
not promise a cold-build or 60-second update time.

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
and passes its stdout through as text lines:

1. `<builder> run -p tilo-vm -- layer-pack --component <name> ...` runs
   tilo-vm's packer there: the same `cargo build` as above (its output
   streams here), then `os/scripts/install-components.py`, then the layer
   tar on stdout as framed base64 lines with its sha256;
2. tilo-vm decodes those lines straight into the upload and checks the
   sha256 and size before it imports. A remote `build` that brings its
   binaries back would put every one of them into `target/` instead;
3. the layer records `builder = "remote"`, and the target and rustc read
   from the binary on the builder.

Which builder serves which architecture is set once in `vms.toml`, as the
drop-in's command line up to the cargo subcommand:

```toml
[remote_builders]
aarch64 = ["remote-cargo", "--on-builder"]   # whatever runs `run` on an aarch64 machine
```

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

## What restarts

| Component scope | Goes into | What happens |
|---|---|---|
| `session` (shell, launcher, user services, runtime) | your slot | a normal session restart: log out, restart your user manager, log in. A laptop's `device` slot (the owner's login) instead merges VM-wide and restarts the desktop and the component's user services in place; the owner stays logged in (`tilo-vm guide devices`) |
| app (`app:<dir>`) | your slot's app store | nothing restarts; live at next app launch |
| `vm` (fs, greeter, system services) | the `vm` slot | `systemd-sysext refresh`, its active units and the live owner's active user services restart, and **every slot's session restarts** |
| `boot`, swappable (the kernel: `dev_swap = "reboot"`) | the `vm` slot, plus a boot entry | modules merge with `systemd-sysext refresh`, the kernel becomes the next boot entry, **the VM reboots**; confirmed, it becomes the default (`tilo-vm guide kernel`) |
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

### Worktree apps and their matching runtime

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

### Userspace capacity preflight

The userspace command first receives and verifies all nine framed archives in private
host temporary files under this worktree’s `target/tilo-vm/`, using the same single remote builder admission. Before the first
VM upload it observes available storage and reports the whole-cohort estimate: all
unpacked payloads (4 KiB allocation rounding plus 16 KiB metadata allowance per entry
and 1 MiB per layer), the largest simultaneous upload, and the unchanged configured
free-space reserve. Uploads are removed after each import. Existing layers are counted
conservatively even if the importer later reuses them. The receipt retains this budget.
Missing capacity observations, malformed archives and insufficient capacity refuse
without transferring or selecting any cohort layer. Host temporary files are released
on failure or completion; ensure the host has space for the bounded archives too.

This is a preflight observation, not an exclusive disk reservation: other writers can
consume space afterward. Existing per-member free-space checks remain. A refusal
reports the byte shortfall and the ordinary `tilo-vm gc --dry-run` / `tilo-vm gc` route;
it never lowers the reserve or deletes user files.
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
