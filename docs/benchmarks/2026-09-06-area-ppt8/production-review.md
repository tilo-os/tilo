# Pre-validation production review

Static review only; no correctness or performance pass is implied.

- Shader construction adds separate opt-in Area full/sparse permutations.
  Default P4 and MSAA remain separate. Native tile dimensions, dispatch tile
  counts, bindings, damage records, output stores, and blend-spill indexing stay
  the same. Per-invocation private arrays grow; their driver scratch cost is
  unmeasured.
- P8 Area coverage shares segment/y terms but reconstructs both original
  four-pixel X origins before floating-point evaluation. Linear gradients also
  reconstruct each original subgroup origin. Other paints retain the original
  subgroup-plus-lane coordinate expression. This reduces a known arithmetic
  reassociation risk; exact-byte GPU tests are still required.
- Ordinary synchronous full/sparse selection requires Area, an available P8
  shader, and no incompatible fine/coarse experiment. Low-level selection also
  checks Area and excludes actually selected incompatible permutations.
  Public shared-coarse diagnostics and async paths remain P4.
- Shell and UI selected-attempt counters are recorded after successful command
  recording, before bump settlement. They count selected attempts, including a
  selected pass that later requires recovery; they do not claim completion.
  UI clears these counters before its measured loop. Counters do not add GPU
  timestamp queries or readbacks.
- The four-way private correctness helper records one coarse result and four
  fine passes. Config, segments, PTCL, paint data, gradients, and atlas are
  shared; each output/spill pair is distinct. The recording test checks shader
  identity and bindings. Its runtime scenes are being strengthened as described
  in `initial-review.md`.

Generated old-permutation token and binding/workgroup metadata hashes will be
compared after compilation. That audit is not compiled-binary equivalence.
