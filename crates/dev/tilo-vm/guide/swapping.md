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
tilo-vm rollback [--to <generation>]
```

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
ignore. `--profile dev` compiles much faster and runs slower; use it while
iterating on logic, not for performance or timing work. The layer's
provenance records the profile. `--profile` applies only to
`--from worktree` components, not to apps or CI builds.

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

`--builder host` builds with this host's cargo and uploads the layer. It
needs a Linux host of the VM's architecture; macOS and Windows hosts use the
VM builder. Without `--builder`, a VM image that lacks cargo, rsync or
python3 makes a Linux host build locally, with a warning.

## CI builds (`--from sha:` / `--from ci:`)

CI publishes `code.sandbox.example/qos/tilo:main-<sha7>` for main commits and
`:<version>` for releases. `sha:<commit>` means `ci:main-<sha7>`. The
component's manifest `files` are copied out of that image into a layer with
`authority = "ci"`. The VM pulls it when its image has podman; otherwise this
host does (needs podman here). Pulls need a podman registry login for
`code.sandbox.example` where the pull runs: the VM's own
(`tilo-vm vm ssh --raw -- podman login --authfile /etc/ostree/auth.json code.sandbox.example`)
or this host's (`podman login code.sandbox.example`). `FORGEJO_TOKEN` and fgj/tea
logins do not count for pulls. A 401 or `reqPackageAccess` names which login
is missing.

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
| `boot` (kernel, UKI, bootloader, initramfs, installer) | nothing | refused; use the scenario driver the error names |
| not a component (Containerfile, units, `/etc`, packages) | the VM's image | `tilo-vm vm upgrade --from local`: new image, reboot (`tilo-vm guide images`) |

A VM-wide swap interrupts other agents' sessions, so prefer session
components in your own slot.

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
