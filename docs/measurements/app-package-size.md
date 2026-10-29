# App package size

"Apps are ~2 MB" is measured as the bytes of the signed `.tapp` a person
downloads, per architecture. The KPI targets are a median of at most 2 MB, a
p90 of at most 4 MB and a max of at most 6 MB (decimal). Each app has its own
budget in `tools/perf/tapp-size-budget/budget.toml`, set at its measured size
plus 1 % headroom, rounded up to 64 KiB. `tools/perf/tapp_size_ratchet.py`
enforces the budgets in these places:

- **Release / apps** (`Shipped tapps within their size budget`) measures each
  tapp it has just packaged and signed, which is the affected apps only. It
  fails when an app is over its budget or has none, so nothing is published.
  To measure apps the pull request job did not pick, dispatch Release / apps
  on the branch with `apps=<dir>`, the architecture you need, and publish
  off. The same job also packages Calculator twice
  (`tools/apps/check-reproducible-tapp.sh`, the second time compiled again
  past the compiler cache) and fails on any byte that differs: an app whose
  package changes without its source re-sends itself in every OS update.
- **`tools/ci/perf-scheduled.sh footprint`** holds every published app to
  its budget. It also holds the apps' median, p90 and max to the fleet
  ceilings: the KPI targets, or an interim ceiling where the fleet does not
  meet a target yet. The budgets are those of the commit the newest release
  was built from, since the registry serves that release until the next one
  publishes.
- **`lint.yml`** runs `check-budget`, a text check. A budget or ceiling
  may only grow by what a raise file new in the same change adds. The budgeted
  apps must be exactly the default collection, so an app added to `apps/`
  brings its budget. A new app may start at the 6 MB max target until it is
  measured.

The OS image's app store holds each app to the same budgets when it packs it:
`os/scripts/pack-apps.sh` reads `budget.toml` and the raise files and refuses
a package over its budget for the build's architecture, naming the app, its
size and its budget, before it applies the format's 8 MB ceiling
(`crates/packaging/tilo-app-package/max-app-bytes`). Both build the app the same way. The
release package the size checks measure also carries the app's search entries and its
instruction-set note, which makes it the larger by about 1 KB (at e82aadce5c on aarch64: Code
Editor 2625536 bytes against 2625024, Messages 1332224 against 1331712, Store 2721792 against
2720768), so an app within its budget in a size check is within it when the image packs it.
A build whose binaries are
not the release profile the budgets measure (`TILO_RUST_LTO=off`) reports the
app instead of refusing it (`TAPP_BUDGET=report`).

A developer session (`dist/dev/nested-session.sh`, `tilolang dev`) installs the apps it
built itself, signed with its own development key, and never publishes them; the `thin-dev`
profile builds them for build speed rather than size. Its store, a build with
`dev-overrides`, installs a package whose signature verifies under a key on the session's
publisher list (`TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE`) under the 64 MiB desktop-build
ceiling instead of the 8 MB one (`package_store::size_limit`). A shipped store reads no such
list, and the registry holds every application to the 8 MB ceiling.

The population is the default collection's apps. The runtime package is the
runtime's own KPI. The runtime and provider stubs are not apps, and apps the
registry still lists but this repository no longer builds are not counted. The
footprint run names every package it leaves out.

To raise a budget for an intended growth, add one file per change. Never edit
the base numbers upward:

```toml
# tools/perf/tapp-size-budget/raises/<change>.toml
reason = "the toolkit gained guarded OS dictation"  # at least 20 characters
run = 90210                  # the run that measured the growth

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

To see what makes an app large, build it as a release does but keep the symbol tables;
`nm -S --size-sort -C` on the app then lists what it compiled itself;
a toolkit type's generic code there (a widget, a dialog, a serde instance) is code every app
using it compiles again, and is usually cheapest to fix in the toolkit. A warm build after a
toolkit change takes about five minutes on sixteen cores.

The symbol table undercounts such code: a small generic function is also inlined at each call,
so a fix often saves several times what `nm` shows for it. What has paid most so far:

- A generic entry point hands its work to a function that is not generic: a constructor taking
  `impl Into<String>` builds from a `String`, a builder taking a closure boxes it first.
  Thirteen of the toolkit's (`ListRow::new`, `EmptyState::new`, `Button::on_effect`, ...)
  took 968,806 loadable bytes out of the 64 x86_64 apps (cdb9027712).
- An impl that would be `#[inline]` is written out: a derived `PartialEq` or `Clone` on a
  large type (`Theme`) is copied into every caller.
- A dependency's inline or generic code that every app reaches goes out of line where the
  runtime holds it once: `vendor/serde_json` (31d03ee94e) took 966,311 bytes out of the same
  apps. Together the two changes made the tapps 1,932,800 bytes smaller on x86_64 (median
  27 KB each) and 1,739,776 on aarch64.

Not every erasure pays. Routing `Widget`'s default methods through a trait object made the apps
larger: the per-type copies of a leaf widget's defaults were identical, so the linker already
folded them into one, and each widget's vtable gained two entries.

`tools/binary-budget.toml` is different: it is a weekly guard against static
linking in release binaries, not this KPI.
