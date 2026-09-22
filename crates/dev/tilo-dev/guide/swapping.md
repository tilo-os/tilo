# Swapping components

```sh
tilo-dev swap shell                       # your slot, built from this worktree
tilo-dev swap my-slot launcher            # an explicit slot
tilo-dev swap vm fs                       # a VM-wide component into the vm slot
tilo-dev swap shell --from layer:<hash>   # a layer already in the VM
tilo-dev rollback [--to <generation>]
```

Components and their scope come from `os/components.toml`
(`python3 os/scripts/install-components.py --list`).

`--from worktree` runs, on a Linux host:

1. `cargo build --locked --profile <profile> -p <packages> --features <manifest
   features plus pkg/dev-overrides where the package has it>`
2. `os/scripts/install-components.py --from target/<profile> --dest <layer> <component>`
3. uploads the layer; the VM labels it for SELinux and stores it by content hash.

## What restarts

| Component scope | Goes into | What happens |
|---|---|---|
| `session` (shell, launcher, user services, runtime) | your slot | a normal session restart: log out, restart your user manager, log in |
| app-only | your slot | nothing restarts; live at next app launch |
| `vm` (fs, greeter, system services) | the `vm` slot | `systemd-sysext refresh`, its units restart, and **every slot's session restarts** |
| `boot` (kernel, UKI, bootloader, initramfs, installer) | nothing | refused; use the scenario driver the error names |

A session restart takes seconds; the goal is under 10s for swap plus restart.
A VM-wide swap interrupts other agents' sessions, so prefer session components
in your own slot.

## Checks

- A layer built for another CPU architecture, or with a builder image that
  differs from the VM's, is refused. A different base image or toolchain
  is reported as a warning.
- Host builds are local builds: fine for testing, never promotable.
- Swapping the same layer twice does nothing. A failed apply leaves the slot
  on its old generation; rerun the swap.
