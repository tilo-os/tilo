# Cross-target: same source → `.tapp` and Flatpak

Tilo apps are engineered to run on stock Linux desktops too. The `runtime`
SDK's graceful-degradation contract is the mechanism; this document is
the recipe.

## The contract

Every SDK entry point that touches a system service returns
`Result<T, NotAvailable>` and never panics. When no service is
reachable, the app is expected to fall back to a local-only mode.
Concretely:

| Call                              | Tilo session                     | Stock Linux                       |
| --------------------------------- | ------------------------------- | --------------------------------- |
| `runtime::collab::try_connect`    | Returns a `CollabClient`.       | `Err(NotAvailable::ServiceFdMissing)` — collab tab disabled. |
| `runtime::fs::try_connect`        | Returns an `FsClient`.          | `Err(NotAvailable::ServiceFdMissing)` — open files via `std::fs` under a fallback state dir. |
| `runtime::identity::load`         | Returns the user's `SigningIdentity`. | `Err(NotAvailable::ServiceFdMissing)` — display "signed in as (anonymous)"; generate a session-local keypair if you need one. |
| `runtime::portal_xdg::Notification::send` | Fires desktop notification (if the manifest declared `notifications = "via-host"`). | If a bus is reachable AND a portal frontend is present, fires; else `Err(PortalError::NotAvailable)` — fall back to in-app toast. |
| `runtime::is_tilo_native()`        | `true` (`TILO_WORKSPACE_ID` set). | `false`.                          |

The `tilo-hello` demo above proves this: the same binary run under
`tilo-install run` outside a live Tilo session (no services, no bus in the
sandbox) prints "unavailable (no fd — graceful)" for every service and
"unavailable — graceful" for every portal call. No panic, no crash.

## What to do in your code

### Feature-detect, don't `unwrap`

```rust
// Bad — panics on stock Linux.
let collab = runtime::collab::try_connect().unwrap();

// Good — fall back to no collaboration.
let collab_state = match runtime::collab::try_connect() {
    Ok(client) => CollabState::Active(client),
    Err(_)     => CollabState::Local,
};
```

### Prefer `runtime::` wrappers, but don't require them

For filesystem work, the `cli` SDK's `resolve_workspace_path` picks
between the FUSE tree and a per-package fallback dir. GUI apps should
mirror the pattern: try `runtime::fs::try_connect()`, fall back to
`dirs::data_dir().join(package_id).join(rel)` on `NotAvailable`.

That fallback needs no Tilo-specific branch. On stock Linux it resolves to
`~/.local/share/<package-id>/`; under the Tilo launcher `XDG_DATA_HOME` is
`/run/tilo/app/.local/share`, so the same call lands in the one directory
the app was given. Do not try to pick a location yourself — a Tilo manifest
cannot name a path, and an app that hard-codes one outside its own
directory gets `EACCES` from Landlock.

### First-run UI

Use `runtime::is_tilo_native()` for cheap "which experience are we in"
gating (welcome message, help text) — never as an authorization gate.
To gate features, call `try_connect` and match on the
result.

## Manifest side-by-side

The same source ships as:

- A `.tapp` under `tilo-app-package pack --manifest manifest.toml --payload
  payload/ --output my-app.tapp`.
- A Flatpak using the standard `org.flatpak.Builder` YAML. The Flatpak
  runs the same entrypoint binary, but the sandbox is Flatpak's rather
  than Tilo's, and no `TILO_SVC_*_FD` env vars are set.

Your Cargo.toml doesn't fork per target — same crate, same deps. Only
the packaging metadata differs.

### Flatpak example manifest fragment

```yaml
# my.example.hello.yml (excerpt)
app-id: my.example.hello
runtime: org.freedesktop.Platform
runtime-version: '24.08'
sdk: org.freedesktop.Sdk
command: my-example-hello

finish-args:
  - --socket=wayland
  - --socket=fallback-x11
  - --share=ipc
  - --talk-name=org.freedesktop.portal.Desktop

modules:
  - name: my-example-hello
    buildsystem: simple
    build-commands:
      - install -Dm755 my-example-hello /app/bin/my-example-hello
```

Note the `--talk-name=org.freedesktop.portal.Desktop` — Flatpak's way of
declaring D-Bus portal access, analogous to `[sandbox.portals]` in the
Tilo manifest.

## Build both targets

For a codebase with one crate and one entrypoint binary, use this workflow for
both package formats:

```
# Rust build (used by both flows)
$ cargo build --release
$ ls target/release/my-example-hello

# Tilo packaging
$ cp target/release/my-example-hello payload/bin/
$ tilo-app-package pack --manifest manifest.toml --payload payload --output my-app.tapp

# Flatpak packaging (same binary)
$ flatpak-builder --repo=my-repo build-dir my.example.hello.yml
```

Both distribution channels serve the same binary. The `.tapp` embeds a
Tilo manifest describing the Tilo-specific sandbox; the Flatpak embeds a
Flatpak manifest describing its own sandbox. The binary itself is
sandbox-agnostic — it uses `runtime` SDK calls and gracefully degrades
when they fail.

## What to feature-flag out for stock Linux

No feature flag is needed for the usual service-availability case. If you
find yourself reaching for `#[cfg(feature = "tilo-native")]`, check whether
the SDK already returns `NotAvailable` for that call on stock Linux.

Cases where a feature flag might be justified:

- Depending on a native library that is licensed for Tilo distribution
  only. Split the crate; ship the Tilo-only variant.
- Wanting a smaller binary for a Flatpak build (`--no-default-features
  --features "core-only"`). Fine, but make sure the missing surface
  fails gracefully at runtime rather than at compile time — a user who
  installs the Flatpak on Tilo should still get the Tilo features.

## What NOT to do

- **Do not assume `runtime::collab::try_connect().is_err()` means "we
  are on stock Linux"**. The service might be temporarily down. Handle
  it as "no live collaboration right now" rather than "we are on a
  different OS forever."
- **Do not use `runtime::is_tilo_native()` to gate calls**. It's a
  telemetry hint. The launcher can set `TILO_WORKSPACE_ID` in scenarios
  where no service fd is available (integration tests, degraded boots).
  `try_connect` is the source of truth.
- **Do not swallow `NotAvailable` silently.** Surface it in the UI —
  users on stock Linux should understand why "share" is greyed out.
- **Do not `unwrap` a portal call** — the bus might be missing even
  inside a Tilo session (if the manifest didn't declare the portal, the
  launcher didn't plumb it, and the call returns `NotAvailable` by
  design).

## The single-manifest question

Long-term, there is no reason a single `.tapp` couldn't also be
convertible to a `.flatpak` bundle by tooling — the payload is the same
and both manifests describe overlapping sandboxes. That tooling doesn't
exist yet. Today, publishing to both channels means two build steps and
two manifest files.

Next: [`hello-world.md`](./hello-world.md) — the full recipe from
`cargo new` to launched process.
