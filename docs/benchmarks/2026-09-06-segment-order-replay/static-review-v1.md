# Frozen v1 static review (before compilation)

Frozen source was saved in source-v1.sha256. Exactly the four authorized Rust
files differed from the preceding scene-reuse manifest; ordinary shaders,
configuration and all other previous inputs were unchanged. No compiler,
test or GPU replay ran against this version.

Root found these corrections needed before execution:

1. Installed wgpu29.0.3 BufferView is an owned non-generic type; the new helper
   incorrectly supplied a lifetime parameter.
2. Installed serde_core1.0.229 implements array Deserialize only through32;
   the saved fixture must decode to vectors, validate49, then use fixed arrays.
3. The patch/control still rejected whole segment allocations with incomplete
   stride tails. Bound complete reachable24-byte records using logical lengths,
   rather than requiring entire allocation divisibility.
4. PTCL allocation length includes continuation space. It is not a native tile
   count; validate the target against checked viewport tile count as well.
5. Add the planned cap/multiset and recording-identity/order/lifetime tests,
   plus GPU rejection/no-write cases. New GPU proof may not silently succeed
   when no device is available.
6. Native proof needs adapter/extent/case/attempt evidence and equal A/B target
   segment offsets. Preflight input, extra-transfer cap and output lengths/pitch
   before creating the extra target or exposing a partial output.
7. The transfer budget must include initial zero-filled witness uploads as
   well as witness downloads. With two1000-byte witnesses, three980-byte
   order payloads,32-byte control and two native output readbacks, the current
   design transfers66,362,172 logical bytes, not66,360,172. Both fit64MiB;
   neither is total live GPU memory or measured physical bus traffic.

Terra was asked to correct these in the same four files and freeze again.
Source data, expected49/48 pixel values, full-image comparisons, original
zero-difference assertions, ordinary shaders, workload, deadlines and recovery
limits remain unchanged. This is a static-review revision, not a discarded
runtime attempt or a benchmark improvement.
