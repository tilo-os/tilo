# Swapping components

```sh
tilo-vm swap shell                        # your slot, built from this worktree
tilo-vm swap my-slot launcher             # an explicit slot
tilo-vm swap vm fs                        # a VM-wide component into the vm slot
tilo-vm swap app:notes                    # one app, installed into your slot's app store
tilo-vm swap shell --from ci:main-1a2b3c4 # the CI build of a published image
tilo-vm swap shell --from sha:1a2b3c4     # the CI build of a main commit
tilo-vm swap shell --from layer:<hash>    # a layer already in the VM
tilo-vm rollback [--to <generation>]
```

Components and their scope come from `os/components.toml`
(`python3 os/scripts/install-components.py --list`).

## Builders (`--from worktree`, the default)

`--builder vm` (default) works from every host:

1. rsyncs the worktree into the VM (`/var/lib/tilo-vm/src/<worktree-id>/`),
   honouring `.gitignore`; `.git` and `target/` stay behind;
2. runs there `cargo build --locked --profile <profile> -p <packages>
   --features <manifest features, dev_features, pkg/dev-overrides>` with the
   dev image's toolchain and the VM's shared sccache; the output streams live;
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
host does (needs podman here). A 401 or `reqPackageAccess` means a registry
login is missing; the error names where.

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

A VM-wide swap interrupts other agents' sessions, so prefer session
components in your own slot.

## Checks

- A layer built for another CPU architecture, or with a builder image that
  differs from the VM's, is refused. A different base image or toolchain
  is reported as a warning.
- Local builds (`vm`, `host`) are for testing and never promoted; only CI
  builds are.
- Swapping the same layer twice does nothing. A failed apply leaves the slot
  on its old generation; rerun the swap.
