# Provenance

`src/split.rs` is `src/lib.rs` of [wasm-split-cli
0.7.10](https://crates.io/crates/wasm-split-cli/0.7.10) from the
[Dioxus](https://github.com/DioxusLabs/dioxus) project, by Jonathan Kelley
and contributors, licensed MIT OR Apache-2.0. It builds the call graph of a
module linked with `--emit-relocs` and moves what only a split point reaches
into its own module.

Changes:

- A module without a `main` function (a `cdylib`, as the browser bundles
  are) anchors the call graph at wasm-bindgen's `__wbindgen_start` export.
- The JavaScript loader template it embedded is gone; `src/main.rs` writes
  the bundle's own loader (`__wasm_split.js`, module URLs relative to the
  loader, a callback on failure too, and a `tilo-module-unavailable` event
  on the page when the server no longer has a module) and file names.

Fixes, each of which broke the split program of a large Rust module at run
time:

- Type-index relocations (the signature of a `call_indirect`) are no longer
  read as symbol indices, which linked callers to unrelated functions.
- A function symbol resolves to its function by index, not by name: several
  functions share a name (copies of a generic shim), and a name lookup picked
  an arbitrary one.
- The call graph rebuilt for the bindgened module is merged into the one the
  split uses; it was built and then dropped.
- Functions are paired between the linked and the bindgened module by table
  slot where they have one. An `#[inline]` function gets one copy per codegen
  unit, all under the same name, so name pairing could install a copy with
  another signature into a table slot.
- Static data is placed by `src/data.rs`, not by the call graph. Upstream
  cleared a data symbol from the main module when the call graph put it in a
  split module, but code-to-data edges come through name pairing, and a
  static wrongly attributed was cleared while main still read it. Now a
  function's references are read from the immediates in its bindgened code,
  matched against the addresses the linked module's relocations produced,
  and a static moves into a split module or shared chunk, at its own
  address, only when no other module's code or data can reach it.
- Every function the kept part of the main module still references (through
  its code or the call graph) stays, even when the split analysis missed it.
- Shared chunks hold code again. Upstream's loop that collected the
  functions several split modules use recorded nothing, so its one shared
  chunk was empty and every split module carried its own copy of all such
  code. Functions are now grouped by which split modules reach them (small
  groups merged into one chunk), a chunk imports what it calls from the main
  module and other chunks, and the chunk functions called from outside get
  a table slot the main module leaves empty and the chunk fills.

Memory: upstream parses the whole program once per split module, all at
once, which took 140 GB for a 170 MB module split in 34. Here each split
module is parsed with only the function bodies it keeps (the rest are
`unreachable`), the linked module is parsed without bodies (the call graph
comes from its relocations), the main module is emitted before the split
modules so its parse is gone by then, and only as many split modules are
emitted at once as available memory (the cgroup limit, else MemAvailable)
allows. The same split now fits in 7 GB.

`src/main.rs` is Tilo's.
