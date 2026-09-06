# Causal segment-order GPU replay

Goal unchanged: ordinary default release build, 1,000 real native-4K actions,
UI p99 <= 30 ms; shell exact mean <= 25 ms and p99 <= 50 ms.
Success must precede final polish, scoped commit/push. User merges.

The prior goal turn was progress: 35 tests and seven validation commands passed;
six native-4K 1,000-action diagnostic runs produced 6,000 admitted observations,
zero exact-payload hits at capacities 1/2/4 and no rejected observations.
A small whole-scene cache was therefore deprioritized, not implemented.
See ../2026-09-06-scene-reuse/README.md. All jobs were terminal and the frozen
v1 source/config manifest matched at this turn's start. Goal remains unmet.

## Existing evidence

The fixed quick-settings pose 225-permille failure is preserved in
../2026-09-06-native-tile-bounds/geometry-all-commands-native-2.log.
It used native 3840x2160 outputs and the same nonempty 187,976-byte resolved
CPU scene. Native tile 1435 had 33 commands and five fills: all command
metadata and fill multisets matched across the failing captures, but at least
one fill's ordering differed. Tile 1436 matched in order as well.

For tile 1435 logical command 29, CMD_FILL=1, size_and_rule=98 (49 segments,
nonzero rule), backdrop=0, both complete ordered lists of five meaningful
u32 fields are saved in captured-orders.json. These were extracted from that
existing raw log without changing values. The right order is left indices
39..48 followed by 0..38. The exact multisets, including multiplicity, match.
Segment stride is 24 bytes; the sixth word is padding, not geometry.

The failed full/full comparison reported two bytes differing by one level,
first native (3770,95), XRGB byte0: current/right 48, baseline/left 49.
An older captured pair also has supporting CPU stepwise-f32 area evidence.
None of these observations alone proves GPU final-pixel causality: earlier
captures differed in config capacity and did not capture every bound resource.

## Intended causal question

Can changing only this exact fill's segment ordering reproduce the known
pixel change through the actual ordinary full Area fine shader, while one
coarse snapshot, config/PTCL/info/images and all other segment values remain
fixed? This is a mechanism diagnostic, not a rendering-correctness or latency
acceptance pass. A successful mechanism test must not erase original failures.

The diagnostic has now been implemented and exercised. Its positive causal
result is recorded below; no production ordering repair or new ordinary
release timing result is claimed.

Required design properties:
- Explicit diagnostic-only/test-only entry point; no normal shader/default,
  production sorting/cache/AA, benchmark action or pixel tolerance change.
- Native3840x2160 fixed-pose scene; ordinary full Area shader remains unchanged.
- Guard exact expected metadata and meaningful segment multiset before mutation.
  Missing, malformed, mismatched or unavailable inputs are failures, not skips.
- Hold all non-order inputs fixed; verify ordering actually applied on GPU.
  Preserve stride/padding and protect every buffer boundary.
- Distinct output and mutable blend scratch per fine pass; shared immutable
  resources stay alive until every pass and readback completes.
- No repeated coarse build as a substitute for the held snapshot.
- Explicit bounded diagnostic copies/resources; preserve the existing 64MiB
  geometry-capture cap, one-second map deadline and four-attempt recovery bound.
- Queue writes do not inherently order mutations between dispatches in one
  submission; the design must establish actual GPU ordering.
- Compare known native pixel in XRGB byte order (raw RGBA blue is byte2).
  Require all pixels outside the target tile to match exactly; report all
  differences inside. Do not assume both historical differences came from
  this one fill.
- Predeclare a finite diagnostic run count; no jitter sweep, repeated trials
  until a desired pixel appears, or performance claims from added GPU work.

Execution plans were fixed before their respective validations. All source
owners freeze during validation; each source manifest and failed log is kept.

## Accepted implementation design

The replay will build ordinary dense coarse once per compared pair, then patch
order A, run the unchanged ordinary full Area fine shader, patch order B, and
run that same fine shader again. Config/PTCL/info/gradient/atlas and the segment
buffer are shared; output textures and writable blend scratch are distinct.
The patch is an on-demand diagnostic shader, never initialized or selected by
normal rendering. No production Area shader arithmetic is changed.

A bounded GPU PTCL walk must validate the target logical command, metadata,
segment bounds and exact saved multiset, including duplicates, before changing
any segment field. It writes only the five meaningful words, preserving the
sixth padding word. An owned status/witness readback must establish successful
guards and the actual applied A/B tuples. A small GPU fixture will read its
whole segment buffer to verify untouched records and padding.

Both output readbacks remain FULL native images, not just target-tile crops:
otherwise outside-tile equality would be unmeasured. Their combined row-padded
payload is 66,355,200 bytes at3840x2160. A separate explicit64MiB replay transfer
cap also includes small order/status payloads. This bounds added transfers,
not all live textures/coarse allocations or peak VRAM. The existing geometry
capture cap remains untouched. A diagnostic-only map path uses one combined
absolute one-second deadline for both outputs and status, with full cleanup
on partial-map/poll/guard errors.

The ignored native test has fixed cases AA, AB and BA. Every compared pair holds
its own single coarse snapshot; no equality is inferred across separate coarse
builds. AA requires exact whole-image equality. AB/BA verify applied order and
known native XRGB pixel49/48 and48/49, respectively, plus exact outside-tile
pixels. Report all differences. Two default diagnostic invocations are fixed
in validation-plan-v1.json before execution. Missing inputs, guards or the
expected effect remain preserved failures/inconclusive evidence, never a fix.

## Observed causal result (v5, revalidated in v6 and v9)

Both predeclared invocations passed on llvmpipe (LLVM22.1.8,128bits), Vulkan,
device type CPU. Every image was3840x2160 and every pair completed in one render
attempt. Both witnesses verified exact metadata and all49 applied tuples; the
segment start matched within each pair. Different pairs deliberately rebuild
coarse, so their different segment starts are not compared for equality.

| Held-snapshot comparison | Changed image bytes | Outside tile1435 | XRGB byte0 at(3770,95) | XRGB byte0 at(3772,95) |
| --- | ---: | ---: | --- | --- |
| AA, identical orders | 0 | 0 | Equal | Equal |
| AB, captured left then right | 2 | 0 | 49 -> 48 | 48 -> 49 |
| BA, reversed orders | 2 | 0 | 48 -> 49 | 49 -> 48 |

The results were identical in both invocations. Changing only this fill's
segment order through the unchanged ordinary Area fine shader is therefore
sufficient to reproduce the two captured pixel changes on this adapter. This
is stronger than the earlier CPU floating-point prediction: it observes final
quantized native output while holding other bound inputs fixed within a pair.

See native-causal-round-1-v9.log, native-causal-round-2-v9.log and
causal-results-v9.json for the final source version; the earlier positive v5/v6
logs are also preserved. Original failures remain preserved. This result does
not automatically explain the older clock-location mismatch, establish all
renderer nondeterminism, or justify tolerance relaxation.

The extra logical transfer payload is66,362,172bytes per pair, including two
full output readbacks, three order inputs, control, two zero-witness uploads
and two witness downloads. This diagnostic cost is not a normal-rendering
copy reduction, total GPU allocation, peak VRAM or physical bus measurement.

## Validation history

- v1 and v2: static review only; no executions. Dependency API and boundary
  test corrections are recorded in static-review-v1.md and static-review-v2.md.
- v3: formatting passed; vendor test compilation stopped on two ambiguous
  generic dispatch arrays. No native replay ran.
- v4: formatting passed;92 vendor tests passed and two new GPU tests failed at
  shader parsing because a diagnostic local used a reserved WGSL identifier.
  No native replay ran.
- v5:94 vendor tests,33 shell example tests, both native causal invocations,
  and both unchanged default/bounds native pixel oracles passed (131 test
  passes total). Strict production Clippy then stopped on seven new bridge
  indexing/conversion/deadline lints; later lint commands were not run in v5.
- v6: the same131 runtime tests passed, including both predeclared native
  replay invocations after the readback cleanup. Strict production Clippy
  stopped on similar local names and a102-line function against the100-line
  limit. These are preserved failures, not benchmark samples.
- v7/v8: lint-first validation stopped on helper argument/reborrow warnings;
  no runtime tests or native replays were repeated against these versions.
- v9(final): all10 planned validation commands pass.94 vendor tests,33 shell
  example tests, two causal replays and both original native pixel oracles
  pass, totaling131. Strict production and vendor-test Clippy pass. The
  test-inclusive check reports zero compiler errors or primary diagnostics
  intersecting259 changed hunks across10 owned Rust files; this does not claim
  every unrelated existing test warning is absent. Final formatting/diff
  checks pass and all25 hashed source/config/fixture inputs remain frozen.

The v6/v9 native invocations were predeclared revalidations after bridge source
refactors. All six invocations across v5/v6/v9 are retained and show the same
AA/AB/BA result; none replaces a failed or undesired native outcome. See the
versioned plans, source manifests and validation summaries.

## Next experiment

See follow-up-design.md for a read-only assessment of general in-place
O(n log n) canonical ordering. It avoids inherently quadratic sorting or a
mandatory additional global scratch buffer, but may still add substantial
GPU loads/stores and tail latency. It has not been implemented or measured.
The next step must keep original pixel checks and measure the entire native
1,000-action workload before any default change is justified.

No default production ordering, anti-aliasing math, action workload, pixel
tolerance or latency gate changed. The overall performance goal remains unmet;
no release benchmark, commit, push or merge occurred in this iteration.
