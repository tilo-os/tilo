# Swapping components

```sh
tilo-vm swap shell                        # your slot, built from this worktree
tilo-vm swap shell --profile dev          # faster compile, slower binary
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

## CI builds (`--from sha:` / `--from ci:`)

CI publishes `registry.tilo.so/tilo/tilo:main-<sha7>` for main commits and
`:<version>` for releases. `sha:<commit>` means `ci:main-<sha7>`. The
component's manifest `files` are copied out of that image into a layer with
`authority = "ci"`. The VM pulls it when its image has podman; otherwise this
host does (needs podman here). Tilo images on `registry.tilo.so` are public,
so neither pull needs a login.

## Apps (`app:<dir>`)

`swap app:notes` builds `apps/notes` with `tilo-app package` (self-signed
`.tapp`) and installs it as your slot user with
`tilo-install install --allow-self-signed --force`. The user store shadows
the image's copy, so it is live the next time the app launches; nothing
restarts. App installs are not generations: `rollback` does not undo them.

## What restarts

| Component scope | Goes into | What happens |
|---|---|---|
| `session` (shell, launcher, user services, runtime) | your slot | a normal session restart: log out, restart your user manager, log in |
| app (`app:<dir>`) | your slot's app store | nothing restarts; live at next app launch |
| `vm` (fs, greeter, system services) | the `vm` slot | `systemd-sysext refresh`, its units restart, and **every slot's session restarts** |
| `boot`, swappable (the kernel: `dev_swap = "reboot"`) | the `vm` slot, plus a boot entry | modules merge with `systemd-sysext refresh`, the kernel becomes the next boot entry, **the VM reboots**; confirmed, it becomes the default (`tilo-vm guide kernel`) |
| `boot`, scenario-only (UKI, bootloader, initramfs, installer, rollback) | nothing | refused; use the scenario driver the error names (the installer: `tilo-vm scenario install`, `tilo-vm guide scenarios`) |
| not a component (Containerfile, units, `/etc`, packages) | the VM's image | `tilo-vm vm upgrade --from local`: new image, reboot (`tilo-vm guide images`) |

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
  image, toolchain or `Cargo.lock`. `apps+runtime` is the exception: thin
  apps link the image's `libruntime_dylib.so`, so another `Cargo.lock` is
  refused there. Then move the VM to an image of your commit
  (`tilo-vm vm upgrade`) or rebase onto the image's commit; never regenerate
  `Cargo.lock` to match.
- Local builds (`vm`, `host`) are for testing and never promoted; only CI
  builds are.
- Swapping the same layer twice does nothing. A failed apply leaves the slot
  on its old generation; rerun the swap.
