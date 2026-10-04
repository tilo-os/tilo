# The Tilo app model

Tilo distributes every executable — apps, services, format providers, plugins,
and the shared runtime itself — as a signed, content-addressed bundle called
a `.tapp`. This directory documents everything an outside publisher needs to
build one, ship it, and expect it to run.

For the shortest path from an idea to a native app, start with
[`app-development.md`](../app-development.md). It covers `tilo-app new`, the
edit/restart and test/inspection loop, the standard live/headless lifecycle,
the professional-widget quality bar, and the app-aware `tilo-app package`
command that builds, signs, verifies, and publishes a `.tapp`.

## The promise

You give Tilo a manifest and a payload. In return:

- Your package has a **stable identity**: a BLAKE3 hash of the signed root
  is the `tapp:` URL. Two identical bundles collide; any change flips the
  hash.
- Your publisher key is **signed once, verified everywhere**. The signature
  is checked at install AND at every launch (revocation can arrive between
  the two).
- Your app runs in a **hard sandbox**: a Linux user-namespace, a seccomp
  filter, and Landlock. It names no filesystem path — the launcher gives it
  one writable directory and a fixed system read profile, and everything
  beyond that is a capability the user grants at runtime. Network egress is
  the one thing the manifest still scopes.
- Your app can run on **stock Linux** too. The `runtime` SDK is graceful-
  degradation-first: every service call returns `Result<T, NotAvailable>`
  and never panics, so the same binary is shippable as a Flatpak.
- The **runtime is a separate bundle**. Your `.tapp` carries your code and
  your declared deps; the loader (ld-linux, libc, libgcc_s) lives in a
  content-addressed `tilo-runtime/<version>` bundle the launcher mounts into
  the sandbox at start.

## The tools

| Tool                | What it does                                                       |
| ------------------- | ------------------------------------------------------------------ |
| `tilo-app-package`           | Pack / sign / verify / inspect a `.tapp`.                          |
| `tilo-install`       | Install / list / remove / run installed packages; trust registry; runtime bundle management. |
| `tilo-launcher`      | The privileged spawn point — bwrap + Landlock + exec. Called by `tilo-install run` (or via PATH shims). |
| `tilo-cli-shim`      | Tiny wrapper the installer drops in `~/.local/share/tilo-apps/bin/` for every `[cli].commands` entry. |
| `tilo-runtime-bundle`| Build / verify a `tilo-runtime` bundle from a host root. Usually invoked via `tilo-install runtime install`. |

## The flow

```text
    source                .tapp                install tree               running process
  ┌─────────────┐      ┌───────────────┐    ┌──────────────────────┐    ┌─────────────────────┐
  │Cargo.toml    │     │tapp.toml       │    │registry.toml          │    │bwrap namespace       │
  │src/main.rs  ├──╮   │signed-root     │    │  entry[..]            │    │ + seccomp filter     │
  │Cargo.lock   │  │   │manifest.toml   │─╮  │payloads/              │    │ + Landlock ruleset   │
  │manifest.toml│  │   │collection.qcl  │ │  │  <pkg-id>/            │──▶ │  cwd=payload/        │
  └─────────────┘  ╰─▶ │blobs/<hex>...  │─╯  │    manifest.toml       │    │  TILO_SVC_*_FD=<fd>   │
                       └────────────────┘    │    bin/<entrypoint>    │    │ identity broker fd   │
      cargo build       tilo-app-package pack          └───────────────────────┘    └──────────────────────┘
      (release)         (BLAKE3 + sign)       tilo-install install          tilo-install run
                                              (verify + unpack + register) (fork + exec bwrap)
```

## Where to start

- **New to the model?** Read [`hello-world.md`](./hello-world.md) — a full
  recipe from `cargo new` to a launched sandboxed process.
- **Publishing?** Read [`packaging.md`](./packaging.md) (manifest schema)
  and [`trust.md`](./trust.md) (identities + attestations).
- **Writing code?** Read [`runtime.md`](./runtime.md) (the SDK) and
  [`sandbox.md`](./sandbox.md) (what your manifest is granting the app).
- **Shipping a CLI?** Read [`cli-apps.md`](./cli-apps.md).
- **Wondering how a terminal has a shell at all,** when no app is granted an
  executable directory? Read [`ush.md`](./ush.md) — the runtime that hosts one,
  in a guest built from an [environment](./environments.md) record.
- **Shipping a codec?** Read [`formats.md`](./formats.md).
- **Distributing?** Read [`installation.md`](./installation.md) and
  [`cross-target.md`](./cross-target.md).

The examples are maintained against the current workspace. Run them from a
checkout with the pinned toolchain and treat command output as dependent on
the local platform and installed services; this directory does not claim a
reproducible verification run for a particular historical commit.
