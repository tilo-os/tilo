# Initial source review notes

No compiler, test or runtime was active for this review. Source is still being
implemented by the two Terra owners; none of this is validation success.

Approved integration uses the existing sparse damage record upload, an actual
recorded-pass flag, and existing eligible bump readback. Selected frames defer
that existing readback/free until the sorter and fine are complete; CPU,
nonArea, async and ordinary shared/order-replay diagnostic paths remain
unselected. Pending/pinned readback cases must explicitly remain unsupported.

The canonical shader accesses PTCL atomically so a malformed command list can
set the existing ptcl[0]=~0 sentinel before unchanged fine consumes it. A bump
failure alone is insufficient: fine could otherwise walk malformed/cyclic
commands before the host receives the error. Prior bump failure must also leave
that sentinel set while doing no sort.

The first shader draft had a shrinking-heap bug: sift_down(end=1) underflowed
end-2. An exclusive-end heap needs only root>=end/2 as the no-child test; this
also avoids child-index multiplication overflow. Count2/count3 fixtures are
required, along with reverse, duplicate and longer general cases.

The first jump guard subtracted256 from a potentially64/128-word logical PTCL
limit. Ordered subtraction/bounds tests are required; physically oversized
allocation tails must not make an invalid logical jump pass. Use actual dynamic
PTCL allocation count plus the native prefix, bounded by config capacity, and
actual used segment count, not just physical arrayLength.

The first draft's preprocessor conditional rejected all legal fused marker/tag
pairs in the two configured canonical permutations. The generic parser must
accept only the known producer-approved paint/clip successor forms; a marker
on FILL/SOLID/END/JUMP is not a legal fused command.

Root confirmed the current producer's per-native-tile monotonic-range property:
only write_path emits CMD_FILL via atomicAdd(bump.segments,count), its calls are
serial in a native coarse invocation, and opaque pruning discards a reachable
prefix without recycling segment allocations. A linear start>=previous_end
check therefore handles local overlap/duplicate range validation without a
quadratic descriptor walk. Cross-tile disjointness still relies on successful
nonoverflowing atomic allocation; tests must exercise the actual renderer too.

No performance gain, memory reduction, general determinism or completed fix is
claimed from this source review. All original pixel assertions remain intact.

Further static review caught a sparse ownership error: taking FineResources
before the canonical helper's borrow would panic. The dispatch now precedes
that take, and the recording test checks shared config/segments/PTCL/damage
bindings plus transient-free, external-retention and deferred bump lifetimes.
Async entry also resets the last-selected flag after a synchronous selection.

The first GPU fixture uploaded a zero segment allocation count despite a
nonempty fill. This would correctly fail the new actual-allocation guard; the
valid fixture now supplies its used count. The test fixtures include equal
five-word keys with distinct sixth words, rather than claiming ties for a
modulo/quotient pair that was actually unique.

Both telemetry routes count successful recording attempts before settlement,
so sparse overflow and a full recovery remain distinct attempts. Shell prints
its existing total_us and sample count on every completed result. The parser
has a synthetic boundary check: total_us=25000001 over1000 samples fails a
25000us mean gate even though the displayed integer mean is25000us.

All of these observations are source review or parser self-checks, not shader
runtime validation. Malformed-list GPU guards and original native oracles are
required before the predeclared release timing comparison.
