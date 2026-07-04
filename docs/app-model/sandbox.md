# The sandbox: what your manifest is granting

Every qOS package is launched inside a nested sandbox with three
independent enforcement layers. This document maps the manifest's
`[sandbox.*]` blocks onto the concrete mechanisms.

## The layers

```text
                      ┌────────────────────────────────────────┐
                      │        qos-install run <pkg-id>         │
                      └────────────────────┬───────────────────┘
                                           │ exec
                                           ▼
                      ┌────────────────────────────────────────┐
                      │         qos-launcher run …             │
                      │  (fork, open service sockets, plan     │
                      │   Landlock ruleset, build bwrap argv)  │
                      └────────────────────┬───────────────────┘
                                           │ exec  bwrap …
                                           ▼
        ╭──────────────── Layer 1: bwrap namespaces ────────────────╮
        │  --unshare-all  (user, pid, uts, ipc, mount, [+ net])      │
        │  --ro-bind payload → /var/qos-apps/<pkg-id>                │
        │  --ro-bind runtime → /usr/lib/qos-runtime/<ver>            │
        │  --tmpfs /tmp   --proc /proc   --dev /dev                  │
        │  --setenv QOS_PACKAGE_ID=…  QOS_SVC_*_FD=…                 │
        │  --file <fd>   /run/qos/services/<name>.sock              │
        │  --seccomp <fd>  (Layer 2 filter installed by bwrap)      │
        │                                                            │
        │       ╭───────── Layer 3: sandbox-exec shim ─────────╮    │
        │       │  install Landlock v3 ruleset:                 │    │
        │       │   • ro rules for [sandbox.fs.read] entries    │    │
        │       │   • rw rules for [sandbox.fs.write] entries   │    │
        │       │   • network egress restriction if net = off   │    │
        │       │   • port allowlist scoped to [sandbox.net]    │    │
        │       │  execvp entrypoint                            │    │
        │       ╰───────────────────────────────────────────────╯    │
        ╰────────────────────────────────────────────────────────────╯
```

Each layer is defence-in-depth against the layer above. A kernel bug in
namespace isolation still leaves seccomp and Landlock; a broken seccomp
policy still leaves Landlock; and vice versa. The `qos-hello` demo below
proves both bwrap and Landlock are enforcing in a real run.

## Layer 1: bubblewrap

Driven entirely by `[sandbox]` fields.

### `namespaces`

`--unshare-all` is always applied; the `namespaces` list is currently
informational (the launcher unshares the full set by default). Include
the ones you actually want unshared — omitting `net` in a future launcher
would leave the network namespace shared.

### FS bind mounts

The launcher composes these bwrap args from `[sandbox.fs]`:

- `payload/` → `/var/qos-apps/<package-id>/` (read-only). Your entrypoint
  is `/var/qos-apps/<package-id>/<entrypoint.bin>`.
- Runtime bundle → `/usr/lib/qos-runtime/<version>/` (read-only), plus a
  `LD_LIBRARY_PATH=/usr/lib/qos-runtime/<version>/lib` env var, plus
  re-binds of the loader at its canonical path
  (`/lib64/ld-linux-x86-64.so.2` etc.) so the kernel's `INTERP` lookup
  works.
- Every `[sandbox.fs.read]` entry → `--ro-bind <host> <host>` (path is
  identical inside and outside the sandbox, so `~/Documents` resolves to
  the same absolute path either way).
- Every `[sandbox.fs.write]` entry → `--bind <host> <host>` (read-write).
- `--tmpfs /tmp`, `--proc /proc`, `--dev /dev` always.

### Portals

If any `[sandbox.portals]` entry is `"via-host"` AND the D-Bus session bus
address is `unix:path=<path>` (the common case), the launcher does:

```
--ro-bind <bus-socket-path> <bus-socket-path>
--setenv  DBUS_SESSION_BUS_ADDRESS <original-address>
```

Otherwise the bus is NOT in the sandbox. Your `ashpd` /
`runtime::portal_xdg` calls will surface `PortalError::NotAvailable`. This
is the graceful-degradation contract — see [`runtime.md`](./runtime.md#portal_xdg).

Portal names the launcher understands today:

- `file-chooser` — Open / Save dialogs.
- `screencast` — start a PipeWire capture session.
- `notifications` — desktop notifications.
- `secret` — retrieve the per-application secret from the portal keyring.
- `open-uri` — hand a URL back to the host browser / mailto handler.

### Service sockets

For every `[sandbox.services]` entry the launcher opens a connected Unix
socket and passes the fd via bwrap's `--file <host-fd> <in-sandbox-path>`,
plus a `QOS_SVC_<NAME>_FD=<child-fd>` env var. Your SDK code reads
`QOS_SVC_COLLAB_FD` (or whatever) and takes it from there — see
[`runtime.md`](./runtime.md#portal--service-fds).

If a service is `required` and the launcher can't resolve its
socket, launch fails. If it is `optional`, the launcher warns and does
not pass an fd; your app then sees `NotAvailable::ServiceFdMissing`.

## Layer 2: seccomp

Driven by `[sandbox].seccomp`. The only shipped profile is `qos-default`,
which blocks a small ring of syscalls (mount ops, kernel-module load, etc.)
that a normal Rust GUI app has no reason to make. The seccomp filter is
compiled by the launcher and installed via bwrap's `--seccomp <fd>` before
the child execs.

## Layer 3: Landlock

Driven by `[sandbox].landlock` and the `[sandbox.fs]` / `[sandbox.net]`
blocks. The `sandbox-exec` shim (the launcher itself, invoked recursively)
installs the Landlock ruleset inside the bwrap namespace before
`execvp`ing the app.

Rules derived from the manifest:

- Each `[sandbox.fs.read]` entry → RO access rights on that path.
- Each `[sandbox.fs.write]` entry → RW access rights on that path.
- `[sandbox.net] mode = "off"` → all outbound connections denied.
- `[sandbox.net] mode = "allowlist"` → allowlist of TCP-connect ports (host
  filtering is not enforced by Landlock; see the note below).
- `landlock = "strict"` — abort on Landlock unavailable / partially-
  enforced. `landlock = "lax"` — warn but continue. `landlock = "off"` —
  skip the shim.

You can see the landlock enforcement live in `qos-hello`'s probes:

```
== sandbox probes ==
  read(        /proc/self/status): permission denied — blocked (bwrap or Landlock)
  read(              /etc/passwd): entity not found — blocked (bwrap or Landlock)
  stat(                     /tmp): OK
  stat(            /qos-launcher): OK
  write(/tmp/qos-hello-probe): OK
  write(/var/qos-apps/probe): permission denied — blocked (bwrap or Landlock)
```

`hello`'s manifest declares no `[sandbox.fs.read]` and no
`[sandbox.fs.write]` entries; Landlock therefore denies everything outside
the launcher-provided base (payload, runtime, `/tmp`).

Note the launcher's log line:

```
qos-sandbox-exec: landlock: partially enforced (kernel ABI dropped some rights) (12 ro, 2 rw, net_restricted=true)
```

"partially enforced" is not a bug — Landlock supports up to ABI v5 today,
and older kernel ABIs silently drop rights the kernel doesn't understand.
The launcher records the count of successful rules and continues. On a
kernel with Landlock v3+ the enforcement is total.

### Hostname allowlist

`[sandbox.net] mode = "allowlist"` with `hosts = ["api.example.com"]` is
enforced at the DNS resolution layer today (the launcher's DNS proxy
intercepts A/AAAA queries and rejects non-allowlist names). Kernel-level
per-connection host filtering is not a Landlock feature; the current
allowlist is a defence-in-depth layer on top of DNS.

If your app connects to an IP address directly (skipping DNS), the
allowlist does NOT block it — but Landlock's port-level restriction still
applies. Rule of thumb: bake hostnames, not IPs.

## What gets into your process

After all three layers, your `main()` sees:

- `argv[0]` = `/var/qos-apps/<pkg-id>/<entrypoint.bin>`.
- CWD = your payload root inside the sandbox.
- `HOME` inherited from the launcher's env; typically `/home/<user>` if you
  bind-mounted it, otherwise unset.
- `LD_LIBRARY_PATH=/usr/lib/qos-runtime/<version>/lib`.
- `QOS_PACKAGE_ID=<your reverse-DNS id>`.
- `QOS_SVC_<NAME>_FD=<fd>` for every service the launcher opened.
- `DBUS_SESSION_BUS_ADDRESS` (only if a portal is `via-host`).
- `QOS_WORKSPACE_ID` — set when running inside a real qOS session.

## Debugging

- `qos-install run <pkg> --no-services` skips service resolution entirely.
  Useful when a `required` service is what's preventing launch.
- `qos-install run <pkg> --no-landlock` (hidden, debug-only) disables
  Layer 3. Layers 1 and 2 stay enforced.
- `qos-pkg inspect <pkg>` shows the manifest without launching. Grep for
  `[sandbox.fs]` and `[sandbox.services]` first.

Next: [`runtime.md`](./runtime.md) — the SDK your entrypoint links against.
