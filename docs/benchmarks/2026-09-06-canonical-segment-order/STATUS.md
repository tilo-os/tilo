# Validation and timing status

Source version: v4 (28 SHA-256 inputs). All source owners are frozen.

| Stage | Result |
|---|---|
| Formatting / strict production Clippy / strict vendor-test Clippy / changed-line Clippy | All passed; changed-line diagnostics 0 |
| Vendor tests | 103 passed |
| Shell harness tests | 33 passed, 7 explicit diagnostics left ignored in this invocation |
| UI harness tests | 8 passed |
| UI accounting | 2 passed |
| Candidate parity / edge / overflow checks | 5 passed |
| Native oracle combinations | Bounds, canonical, bounds+canonical, fused+canonical, row-mask+canonical, quad+opaque+canonical passed |
| Current default native oracle | FAILED: 3 bytes, delta 1, first (1894,14,ch2,234 vs233), outside damage |
| Canonical repeatability | 13 action snapshots plus fixed QS225 snapshot exact across8 repetitions in each of2 contexts |
| Existing causal replay under canonical flag | AA0; AB2; BA2 reversed; outside0, unchanged |
| Ordinary release build | Passed in14m17.73s; copied executables hashed;28 source/config/fixture inputs still match |
| Performance comparison | 24/24 attempted; 23 complete; 1 initial-paint timeout retained; only 1 UI numeric pass, no shell numeric passes |

Totals:160 test passes,1 failed test;21/22 validation commands passed. This is
not green default validation and is not goal completion. See the preserved
[baseline exception](pre-timing-baseline-exception.md) for why only exploratory
comparisons may proceed. The failed default invocation was not replaced.

All five canonical native-oracle combinations reported2 full and11 sparse
canonical passes, matching every retained render attempt in those tests.
Selected passes are not proof of performance or physical bus traffic.

The stage count includes checks rather than claiming every assertion as an
independent test. Earlier v1-v3 failures and source manifests remain retained.

The performance targets remain UI p99<=30ms; shell exact mean<=25ms and
p99<=50ms, with1000 real actions and native3840x2160, ordinary default release.
The physical2010-laptop/constant60FPS request is not certified by this host.

See [the completed matrix and interpretation](RESULTS.md). Canonical ordering
remains opt-in: BC/B median mean increased 10.516% for shell and 7.991% for UI.
All 28 inputs and both copied executable hashes matched after timing. Nothing
has been staged, committed or pushed.
