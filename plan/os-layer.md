# OS layer: Fedora bootc

The qOS image is an immutable Fedora bootc deployment. The current base is
Fedora 44, mirrored and pinned by digest in `os/base-snapshot.env`; update the
source and frozen RPM snapshot together when changing Fedora releases.

## Filesystem and package layers

The image keeps the root filesystem immutable while persistent identity,
workspace state, caches, and logs live below `/var`. The collaborative FUSE
filesystem (`fs`) projects metadata at `/var/workspace` and hydrates
content-addressed data into `/var/cache/quick-fs`. Its service is required by
the session and fails when FUSE or the device identity is unavailable.

The image contains the native qOS compositor and system services, the shared
runtime, and the standard applications. Optional application bundles and
developer environments remain separate: Flatpak and distrobox are integration
paths, not replacements for the qOS sandbox or update mechanism.

## Login and session flow

```text
UEFI → systemd-boot → unified kernel image → systemd
  → greetd → cage → qos-greeter
  → authenticated greetd session → qos-session
  → fs.service + qOS graphical-session targets → shell
```

`qos-greeter` is a graphical qOS UI running inside Cage; it is not
`tuigreet`. The session entries in `os/usr/share/wayland-sessions/` invoke
`/usr/libexec/qos-session`, which owns the lifetime of the qOS user targets
and starts `/usr/bin/shell` as the native Wayland compositor. The peer-sync
entry is an explicit opt-in variant.

`fs.service` mounts `/var/workspace` before the compositor. Identity migration
and first-login verification happen in the session targets and greeter flow;
the compositor must not infer identity from an implementation-level Unix
account.

## Build and update pipeline

```text
1. cargo build --release (or the image's builder stage)
2. podman build -f os/Containerfile
3. systemd-repart + bootc install to-filesystem → qcow2
4. qemu/KVM validation, then promotion of the pinned image
```

Use `os/build.sh` for a production image and `os/verify-test.sh` for the test
image, which adds `fs-test.service`. The headless VM toolkit and its display
mode constraints are documented in `os/dev/README.md`; compositor-only lanes
are in `docs/headless-testing.md`.

Deployed systems update atomically with `bootc upgrade` and can return to the
previous bootable generation with `bootc rollback`. Signing, rollback bounds,
and recovery behavior are specified in `docs/security/boot-chain.md`,
`docs/security/rollback.md`, and `docs/recovery.md`.

## Important paths

```text
/usr/bin/shell
/usr/libexec/qos-session
/usr/share/wayland-sessions/shell.desktop
/usr/lib/systemd/system/fs.service
/etc/qos/desktop.toml
/var/workspace
/var/cache/quick-fs
```
