# Pre-build production review

Read-only review completed before compilation on 2026-09-06 UTC.
No concrete production correctness blocker found. Test corrections discovered
in this review are part of the pre-build source freeze, not measured results.

- The coarse predicate requires an opaque packed-alpha-255 solid color, an
  unsegmented non-clear native tile under the real fill rule, and no active
  clip/zero-clip/rendered blend. Fractional coverage and visible AA arithmetic
  remain unchanged.
- Cursor reset returns after the original fixed tile header and restores the
  initial command limit. Earlier dynamic chunks stay allocated but become
  unreachable. Global bump counters and earlier failure flags are not reset.
- Historical maximum blend depth and spill allocation remain intact. The PTCL
  header stores a blend allocation offset, not the maximum depth itself.
- Dense-grid, legacy sparse and active-bin variants match their original
  dispatch modes. Missing matching variants retain the original same-mode
  shader, native bounds, grid and resources.
- Full/recovery, CPU and production shared-coarse paths remain ordinary. Only
  the sparse-fine entry can select an opaque-prune coarse variant.
- No production binding, buffer, extra GPU copy or readback is introduced.
  Selection counts are actual matching shader selections, not actual command
  pruning counts. Availability reports the dense-grid variant; dispatch checks
  the selected mode's matching variant separately.

Pre-build test review required replacing opaque lower fills with translucent
fills to force a real old PTCL jump; checking the replacement color rather
than only command tags; passing requested=true when a matching variant is
missing; aligning the 80-pixel readback row to 512 rather than 256 bytes;
and directly checking deep/negative command behavior. The native partial-edge
fixture copies only the valid final 1x1 pixel. Exact pixel assertions are not
relaxed. The final guard fixture is 144 pixels wide with a 768-byte row stride
and gap tiles separating its five cases, so neighboring edge commands cannot
invalidate its negative-case head assertions. Its PTCL walker accepts only
the fixture's actual command types and requires a bounded end marker.
All test and performance outcomes are recorded separately.
