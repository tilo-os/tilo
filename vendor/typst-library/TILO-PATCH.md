# Tilo: Typst's plugins execute on the shared wasm host

Source: crates.io `typst-library` 0.15.1, carried as it ships apart from this change.

Typst ran plugin modules on a wasm interpreter of its own making: `src/foundations/plugin.rs`
built a `wasmi::Engine`, a bare `Store` and a two-function linker, and gave a plugin function
call no fuel, no memory cap and no deadline. A plugin that loops hangs the compile that called
it for as long as it loops, which in this tree is an editor's compile worker.

- `src/foundations/plugin.rs` builds its engine, store, module and call with
  `tilo_wasm_host::wasmi`: the engine every guest in the tree runs on, a store under `LIMITS`
  (500 million fuel per call, a 256 MiB memory cap, one memory and instance, four tables), and
  `begin` before every call. `CallData` holds the store's `StoreLimits` and implements the
  layer's `Limited`.
- A call that spends its budget says so: `stopped` reports an out-of-fuel or deadline failure
  as ``plugin function `name` did not finish``, and any other failure as
  ``plugin function `name` stopped``.

What this does not add: the store's instance is still Typst's own pool of instances and the
snapshot/transition API is unchanged, and the interpreter is still wasmi, which cannot be
interrupted, so the deadline in `LIMITS` is declared and unenforced there. Fuel is what stops a
plugin that loops.

Remove this patch once upstream runs plugins on a host whose calls are bounded, or takes the
runtime it executes them on as a parameter.

The manifest is marked `publish = false`: this fork carries `tilo-wasm-host` as a path
dependency, and only the tree builds it.
