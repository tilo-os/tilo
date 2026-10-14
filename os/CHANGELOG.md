# os/CHANGELOG

## 2026-10-14 — `--mode hosted` removed

- `shell --mode hosted` (and `--mode=hosted`) now exits with status 2 and
  names its replacement, `--backend winit`. The hosted loop kept its own copy
  of session bootstrap that had drifted from the shipped session's; that
  bootstrap now lives once in `compositor::session_bootstrap`. No unit in the
  image passed the flag.

## 2026-05-22 — native-mode boot + portal/sandbox/peer-sync wiring

Brings the OS image in line with what's landed across the compositor
since the last `os/` bump. Production `Containerfile` semantics are
unchanged for the parts that already worked; everything below is
additive or refactors a path that was incompatible with native mode.

### Wayland session — native mode by default

- `etc/greetd/config.toml`: dropped the `cage --` wrapper around
  `/usr/bin/shell`. As of compositor Phase 7,
  `shell` defaults to `--mode native`, which means the binary
  itself is the Wayland server. Wrapping it in `cage` was correct only
  for the now-legacy `--mode hosted` path; under native mode it
  actively breaks (cage spins up its own Wayland server). greetd now
  just hands tuigreet the sessions dir and lets the user pick.
- `usr/share/wayland-sessions/shell.desktop`: renamed visible
  `Name=` from "Tilo" to "tilo" (matching the portal
  `UseIn=tilo` contract) and dropped the cage wrapping. Added a
  `Comment` so tuigreet shows a useful subtitle.
- `usr/share/wayland-sessions/shell-peer-sync.desktop` (new):
  alternative session entry that invokes
  `shell --enable-peer-sync`. Off by default — user has to
  pick it at the login prompt.

### FUSE ordering + tmpfiles

- `usr/lib/systemd/system/fs.service`:
  - `Before=greetd.service graphical.target` so the FUSE mount is up
    before the compositor session launches.
  - `ConditionPathExists=/dev/fuse` so the unit hard-fails on images
    where the FUSE module isn't loadable, instead of silently booting
    the compositor with an empty mount.
  - `ExecStop=fusermount3 -u -z` (was `umount`) — works without
    `CAP_SYS_ADMIN` and handles a hung client without blocking
    shutdown.
- `usr/lib/tmpfiles.d/tilo.conf` (new): pre-creates
  `/var/workspace` and `/var/cache/quick-fs` with `0755 root:root` at
  boot via `systemd-tmpfiles-setup.service`. The `ExecStartPre=mkdir`
  in `fs.service` becomes a belt-and-suspenders fallback.

### Peer-sync — explicit opt-in

- `usr/lib/systemd/user/shell-peer-sync.service` (new):
  oneshot user unit that creates a marker file at
  `${XDG_RUNTIME_DIR}/tilo/peer-sync.enabled`. **Not enabled by
  default.** Privacy reasons: libp2p's mDNS broadcasts the device and
  gossipsub accepts inbound peers.
- `docs/peer-sync.md` (new): documents the privacy trade-off and the
  two supported opt-in paths (session picker vs `systemctl --user
  enable`). The doc is `COPY`ed into the production image at
  `/usr/share/doc/tilo/peer-sync.md` so it's discoverable
  from the .desktop and systemd `Documentation=` lines.

### Validation image (Containerfile.test)

- Build-time portal config contract: the existing
  `tests/portal_config_validity.sh` is now copied to
  `/usr/libexec/tilo/portal-config-test.sh` and **executed at
  image build time** with `OS_ROOT=/`. Drift between the Rust
  `portal::serve` interface list and the on-disk `.portal` config
  fails the test image build instead of slipping into a VM boot.
- The script gained an `OS_ROOT` env-var override so it works equally
  from the source tree (`bash os/tests/portal_config_validity.sh`)
  and from inside the image (`OS_ROOT=/ bash …`).
- `usr/bin/fs-test.sh`: extended the runtime smoke test (which
  the test image runs at boot via `fs-test.service`):
  - section 1: FUSE mount + content (the original test)
  - section 2: all five binaries present + executable (was: 3)
  - section 3: portal config files present + consistent
  - section 4: `shell` binary is executable / linkable
  Logs land in `journalctl -u fs-test` as before.

### Production Containerfile

- Added `COPY` lines for: `tmpfiles.d`, the new
  `shell-peer-sync.desktop` session entry, the
  `shell-peer-sync.service` user unit, and `docs/peer-sync.md`.
- Did **not** `systemctl --global enable
  shell-peer-sync.service` — the whole point is that it's
  opt-in per user.
- `systemctl --global enable shell-portal.service` was already
  there; left as-is.

### What was already in place — no changes needed

- `xdg-desktop-portal` was already installed and the portal triplet
  (`.portal` + D-Bus `.service` + systemd user unit) is healthy.
- The portal D-Bus name + interface list passes both the Rust-side
  contract (`portal::serve`) and the static `.portal` validator.
- There is no per-user disk grant-store directory to pre-create —
  capability grants persist via the Automerge CRDT inside the
  workspace document (`Workspace.grants: HashMap<AppId,
  Vec<Capability>>`), not as flat files. Whatever the task brief
  meant by "/var/<user>/grants/" doesn't exist in the binary's
  current code path. Documented in the report as a follow-up.

### Known follow-ups (need Rust code, deliberately out of scope here)

1. `shell` does **not** currently recognise `--portal-server-only`
   or `--firstboot`. The flags are referenced by the systemd units
   but the binary silently ignores them (`std::env::args()` parsing
   in `compositor::mod.rs` is permissive). Today, the portal user
   service would launch a full compositor that races with the
   session compositor for `wayland-N`. Needs a CLI handler that
   short-circuits to "register the portal D-Bus name only".
2. `--enable-peer-sync` is consumed from `std::env::args()` only —
   the `peer-sync.enabled` marker file from
   `shell-peer-sync.service` is documented but not yet read
   by the compositor. Until that's wired, persistent opt-in only
   works via the alternative `shell-peer-sync.desktop`
   session entry.
3. Greetd's user is `greetd`; the runtime-DBus permission story for
   D-Bus-activating the portal backend from a freshly-logged-in
   session was not exercised end-to-end (requires the real graphical
   smoke test in `test-vm.sh`).
