# SELinux on the Tilo installer live medium (E1)

Status: implemented and exercised from the sealed live medium. Tracks roadmap
item **E1** — "SELinux policy for Tilo components (drop installer
`enforcing=0`)".

## TL;DR

The installer live medium used to boot with `enforcing=0` on the kernel
cmdline (global SELinux permissive). That is now **removed** — the medium boots
**enforcing**. The privileged install runs in a properly-confined system domain;
the installer UI stays enforced as well.

- `os/installer-disk.toml` — kernel append is now `tilo.installer=1` (was
  `tilo.installer=1 enforcing=0`).
- `os/usr/libexec/tilo-install` — re-launches its work as a **transient
  systemd unit** (`systemd-run`) so the privileged ops run in `init_t`, not the
  greeter's `xdm_t`.
- `os/usr/share/tilo/selinux/tilo-installer.cil` — a scoped, live-medium-only
  SELinux module loaded at boot.
- `os/usr/lib/systemd/system/tilo-installer-setup.service` — now also `semodule
  -i`s that module (in addition to dropping the sudoers rule).
- `os/Containerfile.installer` — ships the `.cil` into the image.

## The problem

The `tilo-installer` UI runs under `cage` in the greetd session, whose SELinux
domain is **`xdm_t`** (a login/desktop domain). The privileged backend
(`tilo-install`) self-escalates with `sudo`, but a plain `sudo` does **not**
change the SELinux domain — the root process is still `xdm_t`. `xdm_t` is
correctly denied the privileged container/install operations a whole-disk
install needs, so `bootc image copy-to-storage` failed with `Permission denied
(os error 13)` under enforcing (confirmed earlier via an enforcing-vs-permissive
A/B). The cheap unblock was global `enforcing=0`.

## The fix: run the privileged work as a transient system unit

`tilo-install`, once root, asks a short transient service to create and label a
mode-0700 directory and mode-0600 stdin/output files under `/run`. It then
re-execs its work via a second transient service equivalent to:

```
systemd-run --collect --quiet --wait \
  --unit=tilo-install-$$ \
  --property=Type=exec \
  --property=StandardInput=file:/run/tilo-installer/bridge/run.<random>/stdin \
  --property=StandardOutput=append:/run/tilo-installer/bridge/run.<random>/output \
  --property=StandardError=append:/run/tilo-installer/bridge/run.<random>/output \
  --setenv=TILO_INSTALL_TRANSIENT=1 \
  --setenv=TILO_TARGET_IMGREF=... \
  -- /usr/libexec/tilo-install <original args>
```

systemd (PID 1, `init_t`) spawns the unit, so the dangerous operations —
`bootc image copy-to-storage`, `podman run --privileged ... bootc install
to-disk`, the partition/mount/seed of the freshly written disk — execute in a
**system-service domain** the base targeted policy already permits to drive
podman/bootc (the ordinary "systemd runs a container service" path), and stay
**fully enforced**. `xdm_t` no longer touches container storage or the disk.

Why this shape:
- The first VM run proved that `systemd-run --pipe` is not reliable here:
  Fedora/systemd reset the transient-unit request while passing the greeter's
  anonymous pipe file descriptors. Ordinary root-only files avoid that D-Bus
  FD-passing path. PID 1 creates the root-only `/run/tilo-installer/bridge`
  below the readable validated-profile directory before greetd. The transient
  preparation service creates each run's files and restores their dedicated
  `tilo_installer_bridge_t` label. The wrapper checks ownership, modes and
  labels before sending the secret. The greeter domain has no create or
  relabel permission on the bridge. Output is tailed while the install runs;
  neither the startup secret nor recovery key enters journald.
- `--wait` propagates the unit's exit code, so the UI's success/failure
  detection is unchanged.
- `--collect` reaps the unit; `--quiet` keeps systemd-run's own chatter out of
  the log.
- `TILO_INSTALL_TRANSIENT=1` breaks the re-exec recursion (the unit runs the
  same script again, already in the system domain, and falls through to the
  work).
- We are already root when calling `systemd-run`, so it talks to the system
  manager directly — **no second sudo, and the existing sudoers rule is
  unchanged** (no fragile `sudo systemd-run *` wildcard to audit).

`tilo-power` (reboot/poweroff) is unchanged: it goes through logind, already
permitted for the greeter by `os/etc/polkit-1/rules.d/49-tilo-greeter-power.rules`.

## The scoped SELinux module

The enforcing VM reported a `USER_AVC` for `xdm_t` requesting `system:start`
on `init_t`. The live-only policy grants that operation, plus greeter
read/write on the dedicated bridge type and service-domain creation/cleanup.
It grants no access to generic `/run` files and does not make any domain
permissive. The setup service is gated on `tilo.install`; the installed system
never loads the module.

The setup unit must permit SELinux helper exec transitions for `semodule` and
`restorecon`. It therefore sets `NoNewPrivileges=no`, retains its other
sandbox restrictions, and fails before greetd if the policy cannot load while
SELinux is enabled.

## VM confirmation

For an automated enforcing proof on a disposable `--dev-login` medium, see
`os/dev/README.md` (`--selinux-audit-ssh` on `test-installer-cycle.sh`). That
path fails closed on AVC/USER_AVC and lost audit records; it does not replace
the release medium's SSH-disabled graphical cycle.

Build the installer and run an install on a VM (`./os/dev/vm.sh
boot-installer`), then:

1. **Does the install complete under enforcing?** Run a full install to a blank
   `/dev/vdb`. If it succeeds, the mechanism works.
2. **Capture any residual denials** in every domain:
   ```
   ausearch -m AVC,USER_AVC -ts boot
   ausearch -m AVC -ts boot | audit2allow -m tilo_install_extra
   ```
   The operation most worth watching is `bootc image copy-to-storage` writing
   container storage from `init_t`; if it is denied, add the reported
   `init_t`/`container_*` allows to the `.cil`.
3. Add only permissions justified by observed denials, then rebuild and re-run
   the full install.

### Hotfix if a VM run shows the install broken under enforcing

If an urgent install is needed before the policy is finalised, either re-add
`enforcing=0` to `os/installer-disk.toml` (the old behaviour), or — better —
keep enforcing and add the exact observed permission to the scoped module.

## Scope / safety

- Everything here is **live-medium only** (cmdline-gated). The installed Tilo
  system has never carried `enforcing=0` and is unaffected.
- Both the UI and privileged install run under enforced policy.
