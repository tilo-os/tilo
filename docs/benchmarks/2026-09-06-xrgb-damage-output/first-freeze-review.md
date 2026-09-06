# First freeze review: test coverage incomplete

Terra reported the first source freeze after scoped rustfmt and git diff --check.
No compiler, test, release build or benchmark had run. Root reviewed the new
native library test: it renders 13 FrameBuilder frames (initial plus 12 real
click actions) through one and two destinations at native 3840x2160, then
compares each result with a test-only full swizzle of that bridge's same current
staging image. This is separate from the unchanged independent GPU oracle.

Before compilation, source was reopened for original contract coverage that
was not present in the first freeze:

- Actual cursor restoration across two targets, with moved/hidden cursor and
  at least one partial write. A footprint-bound test alone is insufficient.
- Actual tracked short-destination/prepared-frame errors: no destination change,
  abort, and a successful subsequent paint.
- Post-map swizzle error followed by a successful paint, proving unmap on error.
- Tracked sparse overflow/dense recovery with full output and invalidation of
  the other stale target.
- Pure layout/scanout epoch/invalid target/large damage/accumulator cap boundaries,
  plus non-wrapping bridge epoch exhaustion.

Production algorithm scope is unchanged. The candidate remains off by default.
This review is not a test failure or a benchmark attempt: runtime has not begun.

