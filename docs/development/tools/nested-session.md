# Use the nested-session tool

On Linux, `dist/dev/nested-session.sh` runs a private desktop with signed
development apps and their matched runtime. Nothing is installed system-wide.
For the native macOS desktop, use [Run the desktop](../run-desktop.md) instead.

## Start a small session

From the checkout root, on the host's Wayland desktop:

```sh
dist/dev/nested-session.sh --apps "notes chat"
```

Required locally: `t`, Bash, Python 3, jq, bubblewrap, readelf, sha256sum, the
usual Linux command-line tools and the runtime libraries the binaries load
(including `libxkbcommon.so.0`). `XDG_RUNTIME_DIR` must exist. No local Rust
toolchain is needed. Run the windowed script on the host, not inside a
toolbox/container: it checks that boundary before launch.

The script asks t for the architecture-specific `nested-session` build and
downloads its outputs. Apps and `libruntime_dylib.so` must come from the same
build; fetching individual executables is not a replacement. `--apps` selects
which apps to install from that build's fixed set, not which apps t compiles.
Discover the current set without building:

```sh
dist/dev/nested-session-build.sh --apps
dist/dev/nested-session.sh --help
```

`files` is an alias for `file-explorer`. Without `--apps`, the whole supported
set is installed. `--no-build` reuses the last downloaded build; omit it when
testing source changes.

## Inspect or drive it

The script prints the exact `tilo-control` command and private socket
environment. Use that line, starting with `actions list`, to discover resources
and invoke actions. Do not guess another session's socket.

For no visible window:

```sh
dist/dev/nested-session.sh --headless --apps "notes chat"
```

Headless mode still requires `XDG_RUNTIME_DIR` and the runtime dependencies. Use
the printed control line for actions and snapshots. See
[headless testing](../../headless-testing.md) for the control interface. Neither
windowed nor headless operation establishes physical DRM/seat behaviour; use a
[VM](../dev-vms.md) or [host session](../host-session.md).

## Keep or reset state deliberately

State defaults to `$XDG_STATE_HOME/tilo-nested`, falling back to
`~/.local/state/tilo-nested`. `--state DIR` selects another private profile.

It contains the scratch HOME/XDG tree, app store, signed runtime, publisher key,
throwaway identity and `logs/`. Re-running preserves app data and repackages
changed payloads. The private runtime directory and D-Bus session avoid
socket/service collisions, but this is not a VM: audio is connected to the
host's sound server.

- Close the window or press Ctrl-C: the script stops its process groups and
  removes its runtime directory; persistent state remains.
- `--fresh-desktop`: forget workspaces/windows, retaining identity and apps.
- `--reset-identity`: discard the test identity and its desktop.
- `--prepare-only`: build/install, then exit without launching the desktop.

A throwaway identity is not a personal identity backup. The script refreshes it
before expiry; do not use this state for durable personal work. Inspect
`logs/shell.log`, `identity.log`, `model-gateway.log` and `native.log` for the
corresponding startup failure rather than deleting state as a routine fix.
Configure a model provider in the nested Settings app before asking its agent to
work.

## Try two people together

```sh
dist/dev/nested-pair.sh --apps "chat notes"
dist/dev/nested-pair.sh --help
```

This runs two private identities and a local community site. Its state defaults
to `~/.local/state/tilo-nested-pair`; closing either window or Ctrl-C stops both
sessions and the site. The script prints a control line for each. `--headless`
omits both windows; `--ready FILE` writes readiness details for automation.

For the semantic presence/message/follow checks, see
`dist/dev/nested-pair-check.py --help`. It owns a disposable pair unless
`--attach READY` selects a running one; it is not a login-session or hardware
acceptance test.
