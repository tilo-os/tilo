# Required pure geometry telemetry tests

These are test vectors, not benchmark measurements. Rust changes belong to Terra.
No Cargo/runtime until root review and source freeze.

## Shell LRU, exact bytes and memory

- A/B/A then A: rank2=1, rank1=1, two misses, two hits; payload copy counter
  remains 8 for four-byte A/B fixtures. Verify A's stored Box pointers survive
  both hits; final order is B,A.
- A/B/C/D/A/E/B/A: six misses, two hits, one rank4 and one rank3 hit, four entries,
  retained/peak 16 bytes, copied 24 bytes. This exercises promotion before eviction.
- Same concatenation with different span boundaries must miss. Example
  [a,bc,de,fgh] versus [ab,c,de,fgh]; all four span boundaries are part of identity.
- For each of four spans, mutate one byte keeping every span length equal;
  base then mutant must miss. Different length also misses.
- Four distinct exactly-2-MiB entries retain exactly 8 MiB. A fifth distinct
  exactly-2-MiB entry evicts before snapshotting and never exceeds 8 MiB retained.
  A 2-MiB+1 observation rejects without adding copied bytes, clears history and
  leaves the earlier peak unchanged. Ensure entry cap, byte cap and counters.

## Most-recent descriptor semantics

For each Layout field (10), viewport/tile field (4) and capacity field (7), test
base input, changed-descriptor/same-spans input, and that same changed input again.
First hit records only the expected difference category and descriptor_equal=0;
second hit is equal with no further difference. This proves metadata refresh.
Test a combined layout+viewport+capacity change to allow overlapping counters.
A mode-only Full -> SparseActiveCoarse -> SparseNonActiveCoarse sequence has
two descriptor-equal hits and two mode changes without any payload copy.

## Rejection and profile boundaries

For each public extraction error and each primary rejection route, seed history,
reject, assert empty retained history, unchanged copied bytes and exactly one
primary counter. Accepted plus all primary rejections must equal observations.
Include explicit pending+invalid-route precedence, empty borrowed spans, oversized
input, recovery/invalid mode, CPU error and missing/malformed config/span errors.
Extracted byte totals count every successfully returned input before admission,
including pending/recovery/oversized input; extraction errors contribute no spans.

Extract exact option parsing into a pure value helper if necessary. Check None,
empty, 0, true, 01 and 1; only 1 enables. Check all profile/request combinations
for validity/enabled state, without mutating environment variables in tests.
The separate preserved release executable will prove unprofiled request is
rejected before any adapter header at runtime.

Keep observe/helpers within strict lint limits, with no new lint suppression.
