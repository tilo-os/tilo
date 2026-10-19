# App package size

"Apps are ~2 MB" is measured as the bytes of the signed `.tapp` a person
downloads, per architecture. The KPI targets are a median of at most 2 MB, a
p90 of at most 4 MB and a max of at most 6 MB (decimal). Each app has its own
budget in `tools/perf/tapp-size-budget/budget.toml`, set at its measured size
plus 1 % headroom, rounded up to 64 KiB. `tools/perf/tapp_size_ratchet.py`
enforces the budgets in four places:

- **CI / affected packages** (`tapps / <arch>`, pull requests only) packages
  the tapps of up to eight apps the change affects, on x86_64 and aarch64,
  and holds each to its budget. `tools/ci/select.sh` picks them with
  `tapp_size_ratchet.py select`: apps of the default collection, those
  closest to a budget in the registry first, so a toolkit change measures the
  apps it pushes over first without packaging the whole collection. It
  packages as Release / apps does, from the same compiler cache. A change
  that affects no app runs no such job. A pull request runs its own
  branch's `check.yml`, so a branch older than this job affecting an app
  fails `select` and asks for main to be merged in, rather than landing
  unmeasured.
- **The size gate on main** (`app-sizes-x86_64`, `app-sizes-aarch64` in t) runs on every
  commit of `main` that changes what an app is built from: it compiles the default collection as
  a release does, packages it with throwaway keys (`tools/apps/t-release.sh sizes`) and holds
  every tapp and the runtime to its budget. t names the commit a failure began with, so the
  change that took an app over hears of it the day it lands, not at the next release.
- **Release / apps** (`Shipped tapps within their size budget`) measures each
  tapp it has just packaged and signed, which is the affected apps only. It
  fails when an app is over its budget or has none, so nothing is published.
  To measure apps the pull request job did not pick, dispatch Release / apps
  on the branch with `apps=<dir>`, the architecture you need, and publish
  off. The same job also packages Calculator twice
  (`tools/apps/check-reproducible-tapp.sh`, the second time compiled again
  past the compiler cache) and fails on any byte that differs: an app whose
  package changes without its source re-sends itself in every OS update.
- **Perf / scheduled** (footprint group, nightly) holds every published app to
  its budget. It also holds the apps' median, p90 and max to the fleet
  ceilings: the KPI targets, or an interim ceiling where the fleet does not
  meet a target yet.
- **CI / fast quality** runs `check-budget`, a text check. A budget or ceiling
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
run = 90210                  # the Release / apps run that measured the growth

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
