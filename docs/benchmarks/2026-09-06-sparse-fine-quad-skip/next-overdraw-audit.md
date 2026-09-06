# Next bottleneck: conservative opaque full-tile overdraw

Read-only audit after all 24 quad-skip invocations were terminal. No code was
changed and no benchmark result for this idea is claimed.

Current coarse processing appends every color fill; a full-tile opaque color
does not discard commands underneath it. Fine executes the old commands before
the later solid color covers their output. This is a potential way to avoid
more work than the 4.56% of shell quad groups skipped by the latest candidate.

Candidate predicate, before recording a DRAWTAG_FILL_COLOR:

- no clip-zero state, no active source clip, no active rendered blend;
- zero path segments in this native tile;
- backdrop is non-clear for the actual nonzero/even-odd fill rule;
- premultiplied packed color alpha is exactly 255.

Under those conditions, a future off-default sparse permutation could restart
this tile's command cursor immediately after its preserved blend-header slot,
restore the initial command-block limit, then append the solid color and later
commands in their original order. Native coordinates and visible AA/segment
arithmetic need not change. This needs exact GPU validation, not just a
mathematical assertion, including nested completed clips and partial native
edges. Do not generalize to active clips, alpha254, crossing segments,
gradients, images, blur brushes or clear/even-backdrop tiles.

Previously allocated dynamic PTCL blocks and segments must remain allocated.
Their old command chain becomes unreachable, but decrementing global atomic
bump counters would race. Earlier allocation failures must remain failures;
this idea cannot retroactively prevent an overflow. Resetting historical
maximum blend depth is potentially valid at a fully closed stack, but needs
its own spill/recovery proof and must not be assumed to save GPU memory.

A shader-selected flag would not prove that any tile actually pruned commands.
Actual pruning should first be established with bounded test-only PTCL
inspection/diagnostics, without adding production readbacks or inventing a CPU
counter for GPU decisions.

Suggested bounded validation before timing: exact predicate matrix; opaque
tile over complex lower paint plus a later translucent accent; completed deep
blend stack before the opaque fill and a later clip; negative alpha/crossing/
active-clip cases; old PTCL jump chain replaced without invalidating allocated
segments; existing bounded overflow recovery; native-4K action pixel oracle.
Keep all exact assertions and existing default/CPU/unsupported fallbacks.

The current quad-skip and tile-bounds changes stay frozen/off-default. This
file records the next investigation, not an implementation or promotion.
