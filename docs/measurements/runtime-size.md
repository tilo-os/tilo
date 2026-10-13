# Runtime size

Every thin app maps one shared Rust dylib, `libruntime_dylib.so`. A device
downloads it when an app's runtime is missing, and every app process applies
its dynamic relocations before `main`. `tools/perf/runtime_size_ratchet.py`
reads four numbers from the ELF file, per architecture: `bytes`,
`relocation_bytes` (the relocation tables), `relocated_bytes` (the 4 KiB pages
those relocations dirty in each app process) and `symbol_relocations` (the
relocations that need a symbol lookup). The budgets are in
`tools/perf/runtime-size-budget/budget.toml`, with the same headroom, rounding
and raise files as the [app budgets](app-package-size.md).

GLib and GStreamer are part of the runtime but not of every app's start:
`tools/ci/rustc-thin-app-wrapper.sh` links `libruntime_dylib.so` through
`tools/ci/runtime-dylib-lazy-link.py`, which keeps them out of `DT_NEEDED`,
serves the runtime's calls into them through hidden trampolines that load them
on first use, and records their SONAMEs in the `.tilo.lazy_needed` section. The
runtime bundle tools (`os/scripts/collect-runtime-dependencies.py`,
`verify-runtime-bundle-closure.sh`) ship and verify those libraries like
`DT_NEEDED` ones. A library of `[targets] needed_absent` in the runtime's
`DT_NEEDED`, in what every app loads with it, or in what a built app of
`apps_without_media` loads at exec fails the build gate; if the runtime starts
reading a GLib or GStreamer *variable*, the link itself fails, since nothing can
provide one before the library loads.

- **Release / apps** (`Shared runtime within its size budget`) measures the
  dylib it has just built, and the closures above. Its next steps report the
  loader's cost for an app without media (statistics, the objects it loads,
  its private dirty memory) and, when Video Player is built, decode its H.264
  fixture through the thin runtime.
- **Perf / scheduled** (footprint group) fetches the newest published
  `so.tilo.runtime` of each architecture, holds it to the budgets and records
  `footprint.runtime.<metric>.<arch>`.
- **CI / fast quality** fails when a budget grows without a raise file new in
  the same change.

To raise a budget for an intended growth, add one file per change:

```toml
# tools/perf/runtime-size-budget/raises/<change>.toml
reason = "the toolkit gained a PDF engine"  # at least 20 characters
run = 90210                  # the Release / apps run that measured the growth

[runtime.x86_64]             # added to each metric's budget
bytes = 4194304
```

When the runtime shrinks, both gates print the lower budget to commit.

What the runtime may carry is a rule of its own. It holds the SDK, crates
several apps share and, until each moves out, engines too heavy for their one
app's tapp; never an OS daemon or other OS-internal crate (the filesystem, the
launcher, the app host, the install CLI, the peer transport) and nothing no
app reaches. An app that calls into such a crate links it itself and gets only
the code it calls; the Store, for one, depends on `app-host` without its
default `service` feature. `tools/policy/check_runtime_dylib_surface.py` (CI /
fast quality) resolves the runtime with the default collection the way the
release does and fails on either, and on a crate of an engine that has moved
out.

An engine one app uses moves out of the runtime into an engine library of its
own: a crate under `crates/runtime/engines/<name>/` that is a Rust
`dylib` chained on the runtime (Typst: `libtilo_typst_engine.so` for Typst
Editor; Spotify playback: `libtilo_spotify_engine.so` for Music). It is signed
into the runtime bundle beside `libruntime_dylib.so`, so the runtime's id,
signature and verity cover it, but only its app names it `DT_NEEDED`; every
other app no longer maps or relocates it. The conventions the build relies on:

- the package is `tilo-<name>-engine`, its library `tilo_<name>_engine`; every
  dependency sits behind a `thin` feature, so builds other than the thin one
  (workspace checks and tests, `tilo-app dev`) see an empty library;
- a `[[bin]] tilo-<name>-engine-check` with `required-features = ["thin"]`:
  `os/scripts/build-thin-apps.sh` builds and runs it in every runtime build,
  whichever apps are selected, and writes the libraries it built to
  `target/thin-app/runtime-engines`, which packaging signs into the bundle;
- `tools/ci/rustc-thin-app-wrapper.sh` gives `tilo_*_engine` crates the
  runtime's link flags (`-C prefer-dynamic` is what lets one Rust dylib link
  another);
- the app links the engine under a feature of its own (`runtime-engine` in
  Typst Editor and Music), named in `dist/apps/app-collections.toml`, the
  Containerfile's `APP_FEATURES` and `os/components.toml`; without it the app
  links the engine's crates statically, as `tilo-app dev` does;
- the engine's crates go into the surface check's `ENGINES`. They must also
  stay out of the workspace-hack, which is linked into the runtime: hakari
  lists a crate there when members build it with different feature sets, so
  give every member the same set (`tilo-typst-fonts` enables typst-assets'
  `fonts` on every target for this). Excluding the crate in
  `.config/hakari.toml` instead fails `cargo hakari verify`.

Release / apps reports each engine's bytes, relocations and dirtied pages
beside the runtime's, fails when an app of `apps_without_media` loads one,
renders Typst Editor through its engine and runs Music's Spotify engine
(`music --probe-spotify-engine`, librespot's code without an account,
network or audio device).
