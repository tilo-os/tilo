# SELinux on the qOS installer live medium (E1)

Status: implemented and exercised from the sealed live medium. Tracks roadmap
item **E1** — "SELinux policy for qOS components (drop installer
`enforcing=0`)".

## TL;DR

The installer live medium used to boot with `enforcing=0` on the kernel
cmdline (global SELinux permissive). That is now **removed** — the medium boots
**enforcing**. The privileged install runs in a properly-confined system domain;
only the installer's UI/launcher session domain is relaxed.

- `os/installer-disk.toml` — kernel append is now `qos.installer=1` (was
  `qos.installer=1 enforcing=0`).
- `os/usr/libexec/qos-install` — re-launches its real work as a **transient
  systemd unit** (`systemd-run`) so the privileged ops run in `init_t`, not the
  greeter's `xdm_t`.
- `os/usr/share/qos/selinux/qos-installer.cil` — a scoped, live-medium-only
  SELinux module loaded at boot.
- `os/usr/lib/systemd/system/qos-installer-setup.service` — now also `semodule
  -i`s that module (in addition to dropping the sudoers rule).
- `os/Containerfile.installer` — ships the `.cil` into the image.

## The problem

The `qos-installer` UI runs under `cage` in the greetd session, whose SELinux
domain is **`xdm_t`** (a login/desktop domain). The privileged backend
(`qos-install`) self-escalates with `sudo`, but a plain `sudo` does **not**
change the SELinux domain — the root process is still `xdm_t`. `xdm_t` is
correctly denied the privileged container/install operations a whole-disk
install needs, so `bootc image copy-to-storage` failed with `Permission denied
(os error 13)` under enforcing (confirmed earlier via an enforcing-vs-permissive
A/B). The cheap unblock was global `enforcing=0`.

## The fix: run the privileged work as a transient system unit

`qos-install`, once root, stages stdin and output in a mode-0700 directory under
`/run`, then re-execs its real work via a transient service equivalent to:

```
systemd-run --collect --quiet --wait \
  --unit=qos-install-$$ \
  --property=Type=exec \
  --property=StandardInput=file:/run/qos-install.XXXXXX/stdin \
  --property=StandardOutput=append:/run/qos-install.XXXXXX/output \
  --property=StandardError=append:/run/qos-install.XXXXXX/output \
  --setenv=QOS_INSTALL_TRANSIENT=1 \
  --setenv=QOS_TARGET_IMGREF=... \
  -- /usr/libexec/qos-install <original args>
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
  FD-passing path. PID 1 creates the root-only `/run/qos-installer/bridge`
  below the readable validated-profile directory before greetd, and the wrapper
  matches every bridge file to that system runtime SELinux label before
  systemd opens it; without that explicit match, systemd correctly fails with
  `EXIT_STDIN` (209). Output is tailed while the service runs, so progress still
  streams and neither the startup secret nor recovery key enters journald.
- `--wait` propagates the unit's exit code, so the UI's success/failure
  detection is unchanged.
- `--collect` reaps the unit; `--quiet` keeps systemd-run's own chatter out of
  the log.
- `QOS_INSTALL_TRANSIENT=1` breaks the re-exec recursion (the unit runs the
  same script again, already in the system domain, and falls through to the
  real work).
- We are already root when calling `systemd-run`, so it talks to the system
  manager directly — **no second sudo, and the existing sudoers rule is
  unchanged** (no fragile `sudo systemd-run *` wildcard to audit).

`qos-power` (reboot/poweroff) is unchanged: it goes through logind, already
permitted for the greeter by `os/etc/polkit-1/rules.d/49-qos-greeter-power.rules`.

## The scoped SELinux module

The one remaining gap is that `xdm_t` must be allowed to **ask** systemd to
start the transient unit. `os/usr/share/qos/selinux/qos-installer.cil` grants
that, and is loaded **only on the live medium** (the setup service is gated on
the `qos.installer` cmdline). The installed system never loads it.

Shipped default: `(typepermissive xdm_t)` — the kernel logs-but-allows denials
whose **source** is `xdm_t`, while every other domain (`init_t`,
`container_runtime_t`, the install itself, the kernel) stays enforcing. This is
chosen as the default because the precise allow-rule set for "xdm_t may
StartTransientUnit" cannot be finalised without live AVCs from a VM install run.
It is a large, honest improvement over global `enforcing=0` and carries no
regression risk for the privileged path.

The module also carries the **hardening rules** (commented) that should replace
`typepermissive` once a VM run confirms them. Their CIL syntax + class/perm
names were validated with `secilc`.

## VM confirmation

Build the installer and run an install on a VM (`./os/dev/vm.sh
boot-installer`), then:

1. **Does the install complete under enforcing?** Run a full install to a blank
   `/dev/vdb`. If it succeeds, the mechanism works.
2. **Capture any residual denials** (these would be in enforced domains —
   `init_t`, `container_runtime_t`, etc. — that `typepermissive xdm_t` does NOT
   cover):
   ```
   ausearch -m AVC,USER_AVC -ts boot
   ausearch -m AVC -ts boot | audit2allow -m qos_install_extra
   ```
   The operation most worth watching is `bootc image copy-to-storage` writing
   container storage from `init_t`; if it is denied, add the reported
   `init_t`/`container_*` allows to the `.cil`.
3. **Tighten:** once #2 is clean, replace `(typepermissive xdm_t)` in the
   `.cil` with the commented allow block (plus anything #2 surfaced), rebuild,
   and re-run #1 to confirm the install still completes with `xdm_t` enforced.

### Hotfix if a VM run shows the install broken under enforcing

If an urgent install is needed before the policy is finalised, either re-add
`enforcing=0` to `os/installer-disk.toml` (the old behaviour), or — better —
keep enforcing and widen the scoped module. `(typepermissive xdm_t)` already
covers the launcher; a residual denial would be in an enforced privileged
domain and must be allowed explicitly (do **not** reach for global permissive).

## Scope / safety

- Everything here is **live-medium only** (cmdline-gated). The installed qOS
  system has never carried `enforcing=0` and is unaffected.
- The medium is ephemeral and does whole-disk ops; relaxing only its UI session
  domain while keeping the actual install operations enforced is the right
  trade.
