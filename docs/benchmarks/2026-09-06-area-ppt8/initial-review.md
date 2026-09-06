# Initial frozen-source review

The first frozen candidate is preserved by `review-freeze-1.sha256`. No Cargo,
GPU validation, release build, or performance run was started against it.
Read-only review found the following issues before validation:

1. The new recording test omits the `capture_debug: bool` argument to
   `render_encoding_coarse_with_dispatch`.
2. The 49x17 differential scene paints opaque radial and sweep gradients over
   the preceding gradients. Those overwritten paint paths cannot independently
   prove final-byte parity. Its constant 1x1 image also has no coordinate
   variation. Required coverage needs independently visible paints, non-vacuity
   assertions, and a bounded native-3840x2160 shared-coarse P4/P8 comparison in
   addition to the odd-edge case. Overflow must not yield equal empty outputs.
3. The new GPU tests silently return when no adapter is found. Required fixtures
   must require an adapter and emit positive completion/extent/route evidence.
4. The new default-off routing test reads the ambient environment, so its
   default-off assertion conflicts with the already planned P-enabled suite.
   Default policy should be checked without mutating process environment, and
   explicit fixture selection must work in both D and P runs.

Only the new test/helper code in `vendor/vello/src/lib.rs`, `render.rs`, and,
if needed, `wgpu_engine.rs` is reopened for Terra to address these findings.
Production WGSL/routing remains unchanged. The source must be frozen and
hashed again before any compilation or runtime validation.
