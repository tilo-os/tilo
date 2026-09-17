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
