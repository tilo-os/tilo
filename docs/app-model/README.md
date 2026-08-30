# The qOS app model

qOS distributes every executable — apps, services, format providers, plugins,
and the shared runtime itself — as a signed, content-addressed bundle called
a `.qpkg`. This directory documents everything an outside publisher needs to
build one, ship it, and expect it to run.

## The promise

You give qOS a manifest and a payload. In return:

- Your package has a **stable identity**: a BLAKE3 hash of the signed root
  is the `qpkg:` URL. Two identical bundles collide; any change flips the
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
- The **runtime is a separate bundle**. Your `.qpkg` carries your code and
  your declared deps; the loader (ld-linux, libc, libgcc_s) lives in a
  content-addressed `qos-runtime/<version>` bundle the launcher mounts into
  the sandbox at start.

## The tools

| Tool                | What it does                                                       |
| ------------------- | ------------------------------------------------------------------ |
| `qos-pkg`           | Pack / sign / verify / inspect a `.qpkg`.                          |
| `qos-install`       | Install / list / remove / run installed packages; trust registry; runtime bundle management. |
| `qos-launcher`      | The privileged spawn point — bwrap + Landlock + exec. Called by `qos-install run` (or via PATH shims). |
| `qos-cli-shim`      | Tiny wrapper the installer drops in `~/.local/share/qos-apps/bin/` for every `[cli].commands` entry. |
| `qos-runtime-bundle`| Build / verify a `qos-runtime` bundle from a host root. Usually invoked via `qos-install runtime install`. |

## The flow

```text
    source                .qpkg                install tree               running process
  ┌─────────────┐      ┌───────────────┐    ┌──────────────────────┐    ┌─────────────────────┐
  │Cargo.toml    │     │qpkg.toml       │    │registry.toml          │    │bwrap namespace       │
  │src/main.rs  ├──╮   │signed-root     │    │  entry[..]            │    │ + seccomp filter     │
  │Cargo.lock   │  │   │manifest.toml   │─╮  │payloads/              │    │ + Landlock ruleset   │
  │manifest.toml│  │   │collection.qcl  │ │  │  <pkg-id>/            │──▶ │  cwd=payload/        │
  └─────────────┘  ╰─▶ │blobs/<hex>...  │─╯  │    manifest.toml       │    │  QOS_SVC_*_FD=<fd>   │
                       └────────────────┘    │    bin/<entrypoint>    │    │ identity broker fd   │
      cargo build       qos-pkg pack          └───────────────────────┘    └──────────────────────┘
      (release)         (BLAKE3 + sign)       qos-install install          qos-install run
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
  and the `dsh` profile that trades its isolation for nested containers.
- **Shipping a codec?** Read [`formats.md`](./formats.md).
- **Distributing?** Read [`installation.md`](./installation.md) and
  [`cross-target.md`](./cross-target.md).

Every code block in every file was executed against tools built from
`origin/main` at `01033500327a2bb37d36049a8bb54ee7abb95d9b`. See
[`verified.log`](./verified.log) for the exact command history and tool
hashes. The manifests have since lost their `[sandbox.fs]` block, so the
example manifests and the Landlock rule counts have moved on from that
run; the rest of the command history still holds.
