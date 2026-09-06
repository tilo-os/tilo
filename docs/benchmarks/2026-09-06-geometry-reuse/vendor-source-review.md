# Root vendor/accessor source review (before compilation)

- Pure parser lifetime is tied only to scene bytes. ConfigUniform is copied as a
  small typed POD with unaligned-safe decoding; geometry slices remain borrowed.
- Exact config length, checked u32-word conversion, monotone in-bounds bases,
  whole-word scene length, zero-path padding and empty selected spans fail closed.
- The unique-upload lookup rejects missing/duplicate scene or uniform config.
  Existing ordinary rendering/recording code is untouched; method only reads it.
- All 10 Layout fields, four target/tile dimensions and seven capacity values are
  copied into the descriptor. Base color is intentionally excluded. Pending is
  just a diagnostic guard, not a successful-readback assertion.
- Five vendor test functions cover pointer identity/span offsets, malformed
  inputs and every base position, exact/short/long/unaligned config, the complete
  descriptor mapping, excluded draw/base-color and included geometry mutations,
  CPU/pending handling and unique uploads. These are written, not yet executed.
- Bridge forwarding is read-only and contains no render/timing mutation.
- Modified source/config manifest entries are still only the three allowlisted
  files. No shader/render/recording/budget/config source has changed in this probe.

Shell telemetry tests/review, compile/lint, native pixel validation, ordinary
release build and the six diagnostic captures remain pending. No performance
acceptance or GPU-cache reuse is established.
