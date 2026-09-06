# Exploratory baseline exception, before any release timing

The v4 plan completed all22 checks:21 passed, with160 individual test passes
and1 failure. All candidate checks and lint passed. The sole failure is the
ordinary default-off native oracle, at candidate4. It found3 colour bytes with
delta1, first(1894,14,ch2,234vs233), outside the damage region. Same-coarse
full/sparse remained exact; a repeated ordinary dense render differed from the
first despite identical packed CPU scene bytes. This matches the earlier
clock-location failure class; it does not prove that case's underlying cause.

The failed invocation is preserved in v4-shell-default-native-oracle.log and
will not be replaced or reclassified as a pass. Candidate-only and
bounds+candidate native oracles both passed, as did fused, row-mask and
quad/opaque combinations, each with2 full and11 sparse canonical selections.
The13 action snapshots and fixed QS225 snapshot were exact across8 repetitions
in each of2 contexts. The old explicit causal replay remains unchanged.

The original prerequisite that every validation check pass would prevent
measuring this candidate because its known-broken, unchanged default-off
control is itself nondeterministic. For this exploratory comparison only,
root permits exactly this retained baseline failure after all candidate
checks pass. No pixel assertion, tolerance, workload, timing command, order,
repetition count, performance target, failure/outlier policy or production
default is changed. The comparison runner checks the exact version, failing
test name and signature before accepting this exception.

This is not a clean validation result, a shipping approval, or goal completion.
All24 timings remain ineligible for final acceptance. A later default
promotion must pass fresh unflagged correctness and performance checks before
the user-requested polish, scoped commit and push. The user merges.
