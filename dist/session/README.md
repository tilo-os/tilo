# Host desktop-session contract

`qos-session` is the common entry point for a qOS desktop installed on an
existing Linux system. RPM, Debian and Arch packages should install the same
wrapper and `qos.desktop` entry; the package format is not a runtime signal.

The wrapper passes `--host-desktop-session`. This explicit bit is the only
supported way to select host-install behaviour. Do not infer it from
`/etc/os-release`, `XDG_CURRENT_DESKTOP`, `DESKTOP_SESSION`, the display
manager, or `--backend`: those describe the distribution, portal routing, or
render topology, and none reliably says whether the complete qOS system owns
the machine.

With the flag, the shell:

- keeps the personal identity metadata in the normal XDG config location;
- keeps the certified device-key tree under `$XDG_DATA_HOME/qos`, falling
  back to `$HOME/.local/share/qos`;
- on a missing identity, shows the reduced identity-only OOBE as trusted
  compositor chrome after login and withholds all desktop/app affordances;
- retains import, loss acknowledgement, local minting, fingerprint and
  encrypted-backup steps; and
- starts the ordinary desktop only after the ceremony has completed.

Without the flag, paths, copy and first-login behaviour remain the full qOS
system defaults, including the machine identity tree under `/var/lib/qos`.
Hosted (`--mode hosted`) development windows remain unable to provision a
first identity because an application-shaped window is not a trusted login
surface. A host package must start native mode from a display-manager session.

Package checks should assert that the installed wrapper contains both
`--host-desktop-session` and `--firstboot`, and that the Wayland desktop entry
executes that wrapper rather than the shell binary directly.
