# App package size

"Apps are ~2 MB" is measured as the bytes of the signed `.tapp` a person
downloads, per architecture. The KPI targets are a median of at most 2 MB, a
p90 of at most 4 MB and a max of at most 6 MB (decimal). Each app has its own
budget in `tools/perf/tapp-size-budget/budget.toml`, set at its measured size
plus 1 % headroom, rounded up to 64 KiB. `tools/perf/tapp_size_ratchet.py`
enforces the budgets in these places:

- **One app's size, when a change concerns it** (`app-size/<app>-<arch>` in t, for example
  `t test app-size/notes-aarch64`): compiles that app in the default collection's graph as a
  release does, packages it with throwaway keys (`tools/apps/t-release.sh app-size`) and holds
  its tapp to its budget. What every app's check builds alike, the shared runtime, its engines
  and the packing tools, `thin-runtime-<arch>` builds once (`tools/apps/t-release.sh
  thin-runtime`): each `app-size` check needs it, receives its build directory and compiles only
  its app on it. Its key is what the app is built from: its own crate and the local crates it
  reaches, the manifests and lockfile Cargo resolves the collection's graph from, the packaging
  scripts, and the key of `thin-runtime-<arch>` (the runtime, its engines and the packing
  tools; the tapp names the signed runtime bundle, which covers them). A change elsewhere,
  another app's code included, leaves its result standing. `runtime-size-<arch>` does the same for the shared
  runtime (docs/measurements/runtime-size.md). None of these runs on main's commits by default:
  run the ones for the apps your change touches, before you push. `tools/ci/t_toml.py`
  generates one per app of the default collection, so a new app has its own.
- **The whole collection** (`app-sizes-x86_64`, `app-sizes-aarch64` in t) compiles the
  default collection as a release does, packages it with throwaway keys
  (`tools/apps/t-release.sh sizes`) and holds every tapp and the runtime to its budget. It reads
  every app, so it runs on each new head of `stable` and when dispatched
  (`t dispatch app-sizes-x86_64 --ref main`), not on every commit of `main`.
- **Release / apps** (`Shipped tapps within their size budget`) measures each
  tapp it has just packaged and signed, which is the affected apps only. It
  fails when an app is over its budget or has none, so nothing is published.
  To measure apps the pull request job did not pick, dispatch Release / apps
  on the branch with `apps=<dir>`, the architecture you need, and publish
  off. The same job also packages Calculator twice
  (`tools/apps/check-reproducible-tapp.sh`, the second time compiled again
  past the compiler cache) and fails on any byte that differs: an app whose
  package changes without its source re-sends itself in every OS update.
- **t's `perf-footprint` check** (nightly) holds every published app to
  its budget. It also holds the apps' median, p90 and max to the fleet
  ceilings: the KPI targets, or an interim ceiling where the fleet does not
  meet a target yet.
- **t's `changes` check** runs `check-budget`, a text check. A budget or ceiling
  may only grow by what a raise file new in the same change adds. The budgeted
  apps must be exactly the default collection, so an app added to `apps/`
  brings its budget. A new app may start at the 6 MB max target until it is
  measured.

The OS image's app store holds each app to the same budgets when it packs it:
`os/scripts/pack-apps.sh` reads `budget.toml` and the raise files and refuses
a package over its budget for the build's architecture, naming the app, its
size and its budget, before it applies the format's 8 MB ceiling
(`crates/packaging/tilo-app-package/max-app-bytes`). A build whose binaries are
not the release profile the budgets measure (`TILO_RUST_LTO=off`) reports the
app instead of refusing it (`TAPP_BUDGET=report`).

The population is the default collection's apps. The runtime package is the
runtime's own KPI. The runtime and provider stubs are not apps, and apps the
registry still lists but this repository no longer builds are not counted. The
nightly names every package it leaves out.

To raise a budget for an intended growth, add one file per change. Never edit
the base numbers upward:

```toml
# tools/perf/tapp-size-budget/raises/<change>.toml
reason = "the toolkit gained guarded OS dictation"  # at least 20 characters
run = 90210                  # the run that measured the growth (or a t run: "r5260902b38")

[apps."so.tilo.settings"]    # bytes added to each app's budget
x86_64 = 65536
aarch64 = 65536

[fleet.x86_64]               # only if a fleet ceiling is crossed
p50 = 65536
```

A failing Release / apps job prints this file with each over-budget app's
numbers. Each architecture's job prints its own lines, so merge them. A raise
may at most double a budget. When an app shrinks, both gates print the lower
budget to commit (`[apps."<id>"] <arch> = <bytes>`), with the app removed from
its raise files. They also print when an interim fleet ceiling can be lowered,
or removed once its target holds. To re-seed the budgets from the registry,
run `python3 tools/perf/tapp_size_ratchet.py seed entries.json`, where
`entries.json` is `https://apps.tilo.so/entries`.

To see what makes an app large, `t test app-binaries-aarch64` (or `-x86_64`) builds the
collection exactly as a release does but keeps the symbol tables, and `t outputs
app-binaries-aarch64` puts the apps, `libruntime_dylib.so` and its engines in
`target/app-binaries/aarch64/`. `nm -S --size-sort -C` on an app lists what it compiled itself;
a toolkit type's generic code there (a widget, a dialog, a serde instance) is code every app
using it compiles again, and is usually cheapest to fix in the toolkit. A warm build after a
toolkit change takes about five minutes on sixteen cores.

`tools/binary-budget.toml` is different: it is a weekly guard against static
linking in release binaries, not this KPI.
