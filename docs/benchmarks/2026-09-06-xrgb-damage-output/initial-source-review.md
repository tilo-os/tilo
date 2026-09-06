# In-progress source review

This records root's early review of Terra's partial implementation, before any
compilation or runtime. Items are requirements, not claims of completed fixes.

- Replace cloneable per-history transaction tokens: counters from different
  histories collide, and wrap/reset can revive stale transactions. A non-Clone
  borrowing transaction with consuming commit/abort and abort-on-Drop is the
  preferred ownership boundary. Terra has started that refactor.
- Bind prepared and rendered frame deltas, not just renderer/extent/pitch:
  different revision/previous-revision/regions must not reuse the prepared
  partial plan. Reject a mismatched call and abort; full-write followed by
  committing a different prepared revision would still invent false history.
- Keep destination contents a caller-owned invariant. Do not infer content
  validity from a reused address. Legacy full-output APIs remain safe for
  callers without explicit history.
- Unmap staging on every post-map error. Preserve the swizzle Result, drop the
  mapped view, unmap, then propagate the error. Add error/next-frame coverage.
- New bridge identity and configuration epochs must never wrap into a prior
  identity. Exhaustion should disable tracking/full-fallback rather than
  unnecessarily fail ordinary renderer construction.
- A full GPU/recovery boundary must invalidate other target claims; a complete
  full destination write may seed only that target's new chain.
- Bridge and history must reject short destinations before publishing a
  complete target. Padded row tails stay untouched.
- Cursor footprint must cover every pixel written by the existing cursor
  renderer, including clipped/negative/edge coordinates and hardware fallback.
- A cfg(test) library helper is not visible to an example's linked normal
  library. Put native staging-reference tests in library tests or use an
  explicitly invoked diagnostic path with no extra ordinary reference copy.

No benchmark has run for this candidate. No source is treated as validated.

Further production-route review before compilation:

- The new single-output present entry must drain flip completion events before
  its flip-pending guard, matching the original entry point. Otherwise the first
  queued flip can leave subsequent frames permanently skipped.
- When page flipping is unavailable, preserve flush_dirty and its set_crtc
  fallback rather than ignoring dirty_framebuffer errors. The offscreen
  benchmark cannot establish these KMS routing semantics.
- Any legacy/unknown destination writer must invalidate tracked content when
  it writes. New tracking-unavailable cases must continue full rendering, not
  silently drop the requested frame.
- Validate complete mapped source and all rectangles before the first tracked
  CPU write. Rejecting a later rectangle after writing earlier ones would leave
  a partially modified single-buffer destination.

Further review while implementation remains unfrozen:

- Latch QOS_DRM_XRGB_DAMAGE independently of profiling. Ordinary profile-off
  X/BX/BCX comparisons must exercise the production candidate and report actual
  full/partial counts and CPU destination bytes. Only extraordinary reference
  copies belong in tests or explicitly diagnostic paths.
- Compare the bridge-owned identity separately from the scanout-owned epoch;
  history compares the full identity. A nonzero real scanout epoch must not
  reject every tracked paint while the one-target benchmark passes.
- Bind the transaction to its prepared previous_revision, not the history's
  prior successful revision. A genuine gap or oversized damage is a full
  fallback, not a prepared/actual mismatch. Oversized input validation remains
  bounded; no extra full damage snapshot is necessary.
- Always attempt page_flip after the best-effort dirty_framebuffer call.
  `!dirty_ok || !flip_to(...)` accidentally skips the flip on drivers where
  dirty_framebuffer is unsupported. Evaluate presentation before combining
  failure results for history invalidation.
