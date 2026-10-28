# Develop an installed Linux login session

Use a real login session for locking/unlocking, seat input, suspend and physical
outputs. Prefer a [nested desktop](run-desktop.md) or [dev VM](dev-vms.md) for
changes that do not need your host's login path. macOS runs Tilo as an
application, not as a login desktop or OS.

Installing the host payload changes system packages, PAM unlock support and the
login-session menu. Keep a working alternate desktop and recovery access. The
optional developer entry additionally allows a checked local compositor build to
become your login session; install it only on a development machine.

## Install or update on rpm-ostree

For Silverblue-family hosts, the maintained helper builds the runtime and app
RPMs from the checkout and installs them in one transaction:

```sh
dist/rpm/host-update.sh --help
dist/rpm/host-update.sh
```

Run it as your ordinary user; it invokes sudo for system changes. Compilation
uses Cargo on PATH, so configure the build executor before running it. The
default requires a clean tracked tree. `--allow-dirty` explicitly admits tracked
worktree edits.

Reboot into the new deployment, then choose **Tilo** in the greeter. For live
application from another desktop, `--apply-live` avoids the reboot but still
requires logging out and selecting Tilo. The helper refuses that mode while
`tilo-shell` is running.

The helper installs the version-matched `tilo-session` and `tilo-session-apps`
packages. `--devel` explicitly adds the developer subpackage; `--no-devel`
removes it from the selected package set.

For other RPM hosts, `dist/rpm/build-rpm.sh` produces the runtime, apps and
optional developer RPMs under `dist/rpm/build/RPMS/<architecture>/`. Install the
matching runtime **and apps** with the host package manager.
`dist/rpm/build-rpm.sh --help` describes the build and output without compiling
or packaging.

## Log into your compositor build

After explicitly installing `tilo-session-devel`, build the marked debug target
and point the developer entry at it:

```sh
cargo build --locked -p shell --bin shell --features dev-session
install -d -m 700 "$HOME/.config/tilo"
printf '%s\n' "$PWD/target/debug/shell" > "$HOME/.config/tilo/dev-session"
chmod 600 "$HOME/.config/tilo/dev-session"
```

Run this from the checkout root. Log out and choose **Tilo (my build)**. After
each rebuild, log out and back in; rebuilding does not replace the running
process.

The wrapper checks the marker, ownership and permissions of the config, binary
and their directory chains. It refuses unsafe paths without falling back to the
installed compositor. Do not loosen those checks. A plain release build does not
carry the debug developer-session marker.

The production **Tilo** entry always starts `/usr/bin/tilo-shell`; it never
reads that path file. Remove the optional developer package through the package
manager when finished.

## Understand the host boundary

The package-owned lifecycle runner supplies `--host-desktop-session`,
`--firstboot` and `--backend drm`, and owns graphical services, activation
environment and logout cleanup. First identity setup is trusted compositor
chrome; do not replace it with manual identity-file creation.

This is a desktop on your existing OS, not the Tilo image's boot chain. Host
XDG/Flatpak apps remain host apps with their original confinement. For Files
authority, initialization and recovery, use
[host Files preparation](../../os/docs/peer-sync.md#fedora-primary-files-service-and-explicit-first-run);
never turn missing recovery state into a new empty workspace.

Private session diagnostics live under `$XDG_STATE_HOME/tilo/session-logs`
(normally `~/.local/state/tilo/session-logs`). After a failed login,
`/usr/libexec/tilo-session-report --previous` summarizes the previous session's
log and frame trace. See the
[host-session contract](../../dist/session/README.md) for lifecycle details.
