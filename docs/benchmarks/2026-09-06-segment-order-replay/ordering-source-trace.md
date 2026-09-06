# Read-only ordering trace

This is source evidence, not a completed causal GPU result.

In vendor/vello_shaders/shader/path_count.wgsl, each intersecting line obtains
seg_within_slice from atomicAdd on the path-local tile's segment_count_or_ix.
That value is packed into the upper16bits of SegmentCount.counts. The atomic
guarantees unique positions but does not choose a canonical order among lines.

In path_tiling.wgsl, seg_within_slice=counts>>16 is preserved, and the final
24-byte Segment is written to segments[seg_start+seg_within_slice]. Thus the
fine-readable order follows the assigned positions; path_tiling is not itself
where that ordinal is chosen. Earlier flatten.wgsl also uses atomic reservations
for LineSoup output, so sorting by a transient line index alone would not prove
cross-run canonical geometry order.

In the unchanged ordinary Area fill_path in fine.wgsl, the loop visits
fill.seg_data+i in increasing i. It computes the existing floating-point area
contribution and adds it into area[i], followed by the existing edge term.
The saved input lists contain identical meaningful tuples in a different order.
Order-sensitive floating accumulation is a plausible mechanism, but this trace
does not establish the final quantized pixel effect or rule out other causes
of past independent-context differences. The planned held-resource GPU replay
is needed for that narrower causal claim.

No production arithmetic, atomic assignment or ordering was changed by this
audit. A future stability repair, if justified by actual results, must retain
geometry/AA fidelity and undergo the original exact correctness tests plus
fresh default release performance measurements. Do not infer that adding a
sort would be faster, cheaper or automatically sufficient.
