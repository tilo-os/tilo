# tilo-swipe

Rust bindings to the **FUTO swipe-library** on ExecuTorch through a C-ABI shim.
Used by the Tilo on-screen keyboard (`apps/osk`).

The FUTO swipe-library is GPL-3.0-or-later; Tilo is AGPL-3.0-only. Section 13
of the GPL-3.0 permits combining the two, so it incorporates cleanly.

## How it works

- `csrc/swipe_ffi.cpp` — a C-ABI shim over the library's `SwipeEngine`
  (`include/swipe_decoder/engine.hpp`): create (encoder + optional decoder +
  optional context-LM + a `.combined` dictionary), `recognize` a swipe, and
  `predict_next`.
- `build.rs` — compiles the shim with the `cc` crate and links the prebuilt
  `libswipe_decoder.a` + the ExecuTorch static libraries. It uses
  `cargo:rustc-link-lib`/`link-search` (which propagate to the dependent OSK
  binary); rust-lld resolves the ExecuTorch archives' circular references
  without `--start-group`.
- `src/lib.rs` — a safe `FutoSwipe` wrapper. The encoder is universal; pair it
  with the en_qwerty decoder + context-LM for best accuracy.

## Fallback decoder

When `TILO_SWIPE_LIB` is unset at build time, the crate builds a stub:
`available()` is `false` and `FutoSwipe::new`
returns `None`, so the OSK falls back to its geometric SHARK²-style decoder.
Set `TILO_SWIPE_LIB` at build time to enable FUTO.

## Building with FUTO

```bash
distrobox enter my-distrobox -- ./tilo-swipe/setup-futo.sh   # clone + build + fetch models
export TILO_SWIPE_LIB=.../swipe-library
export TILO_SWIPE_MODELS=.../models
cargo run -p osk
cargo test -p tilo-swipe        # decodes real swipes (hello, the, world, swipe)
```

## Models

From HuggingFace `futo-org/futo-swipe` (~16 MB, `futo-model-weights-license-1.0`):

| dir                  | role      | file              |
|----------------------|-----------|-------------------|
| `honorable_sturgeon` | encoder   | `model_fp32.pte`  |
| `magic_macaw`        | decoder   | `model_fp32.pte`  |
| `hungry_jellyfish`   | contextLM | `context_lm.pte` + `vocab.txt` |

Plus `en_w2.combined` (the dictionary). The OSK resolves the model dir from
`$TILO_SWIPE_MODELS`, then `/usr/share/tilo/swipe-models` (the OS-image install
path), then `tilo-swipe/models`.

## Productionizing in the OS image

`os/build.sh` should run `setup-futo.sh`, export `TILO_SWIPE_LIB` for the `osk`
release build, and install `$TILO_SWIPE_MODELS` to `/usr/share/tilo/swipe-models`.
(The swipe-library can alternatively be vendored as a git submodule so the
build is fully hermetic.)
