# Host desktop-session contract

`tilo-session` is the common entry point for a Tilo Desktop installed on an
existing Linux system. RPM, Debian and Arch packages should install the same
wrapper and `tilo.desktop` entry; the package format is not a runtime signal.

The wrapper passes `--host-desktop-session`. This explicit bit is the only
supported way to select host-install behaviour. Do not infer it from
`/etc/os-release`, `XDG_CURRENT_DESKTOP`, `DESKTOP_SESSION`, the display
manager, or `--backend`: those describe the distribution, portal routing, or
render topology, and none reliably says whether the complete Tilo system owns
the machine.

With the flag, the shell:

- keeps the personal identity metadata in the normal XDG config location;
- keeps the certified device-key tree under `$XDG_DATA_HOME/tilo`, falling
  back to `$HOME/.local/share/tilo`;
- on a missing identity, shows the reduced identity-only OOBE as trusted
  compositor chrome after login and withholds all desktop/app affordances;
- retains import, loss acknowledgement, local minting, fingerprint and
  encrypted-backup steps; and
- starts the ordinary desktop only after the ceremony has completed.

Without the flag, paths, copy and first-login behaviour remain the full Tilo
system defaults, including the machine identity tree under `/var/lib/tilo`.
Hosted (`--mode hosted`) development windows remain unable to provision a
first identity because an application-shaped window is not a trusted login
surface. A host package must start native mode from a display-manager session.

Package checks should assert that the installed wrapper contains both
`--host-desktop-session` and `--firstboot`, and that the Wayland desktop entry
executes that wrapper rather than the shell binary directly.

## Crash and performance records

Host sessions retain private diagnostics in
`$XDG_STATE_HOME/tilo/session-logs` (or `~/.local/state/tilo/session-logs`).
`latest.log` is the bounded compositor/application stream and
`frame-trace.csv` is the structured per-loop phase trace. On the next login
both rotate to `previous.log` and `previous-frame-trace.csv`; every file is
mode `0600`. The trace is flushed once per second, so a hard reset loses at
most its final second rather than all timing evidence.

`/usr/libexec/tilo-session-report --previous` converts those two prior-login
files to one JSON report. It is deliberately read-only: production sessions do
not expose the developer input/control socket.

## Switching back to another desktop

The wrapper owns `tilo-graphical-session.target` only while the compositor is
alive. It starts the target after publishing `XDG_CURRENT_DESKTOP=tilo` and
`XDG_SESSION_TYPE=wayland` to both the systemd user manager and D-Bus
activation environment. This gives xdg-desktop-portal the Tilo backend
selection and prevents a classic D-Bus helper from retaining GNOME's desktop
identity.

On exit it stops the Tilo target (which releases the standard graphical
targets), removes Tilo's values from the user manager, and writes empty values
to D-Bus activation. D-Bus has no unset operation; the next GNOME login's
normal activation-environment update supplies its `GNOME` values before its
portal backends are activated. No portal service, Flatpak configuration, or
host application is replaced by this package.

`org.gnome.keyring.SystemPrompter` is supplied by GCR (`gcr-prompter`), not by
`gnome-keyring-daemon` and not by Tilo. Tilo must not start a second keyring or
prompter unit: that would race the host's Secret Service and change existing
application credential behaviour. The handoff fixes its desktop-routing
environment only. A missing, locked, or failing host GCR/keyring remains a
host-session issue and requires a live password-prompt check after login.

## VM test

`dist/session/vm-e2e/run.py` runs the packaged session as a Fedora user does:
the fedora-44 RPMs from Release / desktop installed with dnf on the Fedora 44
Cloud image under KVM, a gnome-keyring Secret Service unlocked on the user's
bus, the identity provisioned there (`--provision-identity`, the same
platform-storage minting the setup screen does), GNOME input sources seeded
in the user's dconf database as a GNOME user has them (`de+nodeadkeys`,
`compose:ralt`; `guest.sh gnome-input`), SDDM autologin into
`tilo.desktop`, then Settings, Notes, Files and Terminal opened through
Super+Space search on the VM's keyboard (QMP). The first login must import the
GNOME input settings into the session's `settings.json` (`de:nodeadkeys`,
Compose on Right Alt, the import's completion marker; check H05), and the lock
screen password and app names are then typed on that German layout, so the
imported keymap is what unlocks the session. This is the only place the
import is exercised: it runs only with `--host-desktop-session`, which the Tilo
OS session never passes. There is no control socket in
this session, so the run sees it only through SSH, the keyboard and
screenshots. Each app must start, and the compositor must stay the same
process for the dwell after it, with no crash log, core dump, unexpected
session exit or panic in the journal or `latest.log`.

It runs in CI only (`.forgejo/workflows/host-session-e2e.yml`, on the KVM
runners): after every Release / desktop package lane with that run's RPMs,
and on dispatch or a pull request changing the test with RPMs built from that
commit first (`dist/ci/run-package-job.sh` with the fedora-44 target of
`dist/ci/package-targets.txt`). `report.md`, screenshots, the guest journal and session logs
are the run's artifact.

## Files workspace recovery before first initialization

A remembered home peer is a recovery lead, not evidence that a local Files
workspace is empty. `home-peers.json` stores certified relay identities and
numeric collaboration room IDs; it does not contain the Files namespace UUID
or its ticket. The authenticated relay content-discovery response supplies
that binding. An offline or unconfigured host must not be interpreted as
"no existing files".

Before starting a Files daemon, inspect the selected existing cache:

```sh
fs workspace-status --cache-dir /path/to/existing/cache
```

This reads bindings only and prints public workspace UUIDs, namespace
presence and whether existing state can be resumed. It does not contact peers,
repair files, initialize storage, or print tickets. A local ticket without a
saved namespace is not sufficient recovery material. Malformed or ambiguous
bindings fail; a saved namespace bound to a different discovered workspace
requires a separate cache rather than loading it under another authority.

A recovery launch uses the ordinary daemon with
`serve --require-existing-workspace --cache-dir /path/to/existing/cache` and
the mount, certified identity root, existing ACL and data-plane arguments for
that installation. The guard runs before identity loading, transport binding
or cache creation. Once admitted, the ordinary daemon validates/recovers the
namespace and authenticates remote peers; the preflight is not a permission
grant or a substitute for those checks. It also cannot prove that a saved
namespace is healthy or that an offline remote can currently be reached.

For an existing person with no local namespace, first establish the Files
binding advertised by their already-enrolled home peer through authenticated
content discovery. The current host package does not yet deliver the primary
Files service, and registration currently expects a running daemon. This
bootstrap gap must be repaired before a fresh host can complete remote
recovery automatically. Do not work around it by creating an empty namespace
and describing it as recovered data. A genuinely new workspace is a separate,
explicit initialization decision; existing host folders remain ordinary local
files until a separately authorized import into that workspace.
