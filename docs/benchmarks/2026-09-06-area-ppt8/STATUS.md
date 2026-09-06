# Area-PPT8 status

Goal remains unmet. No default promotion, commit, push or merge. The v5 release
and all 30 planned comparisons are complete and audited; see RESULTS.md.

Follow-up diagnostics are also complete, without source changes. The isolated
cache experiment in ../2026-09-06-area-ppt8-cache/RESULTS.md reproduces cold
initial-paint failures for both variants and an extra sample-1 failure for PPT8
in both replications. Third launches complete with zero reported cache misses.
PPT8 remains opt-in; the extra first-use failure and higher RSS prevent promotion.
The four UI profiles in ../2026-09-06-ui-tail-profile/RESULTS.md preserve 4,000
matched samples and point toward earlier compute work as the next audit target.
Neither diagnostic replaces the ordinary comparison or satisfies the goal.

Initial precompile test review and repairs are preserved in the two review
freeze manifests. Production candidate is unchanged by those test repairs.

Validation history:

- v1: format passes; strict vendor-test Clippy passes. Strict production Clippy
  reports UI profile formatter 101/100 lines. No runtime tests.
- v2: extracted formatter is 288 bytes by value; strict production Clippy
  requires a borrow. No runtime tests.
- v3: UI lint repairs pass. Strict production Clippy finds shell sparse-readback
  function 105/100 lines. Test-inclusive compilation completes and reports only
  that warning twice (library and test). No runtime tests.

All attempted source manifests, commands, raw outputs, exit results and unchanged
postvalidation hashes are preserved. Terra's behavior-preserving shell counter
helper extraction was frozen as v4; only paint.rs changed from v3. Static
review simplified the new helper's single-use option matches before any v4
validation ran. The corrected helper is frozen as v5. All four lint checks
pass, with zero changed-line/compiler diagnostics. Vendor tests pass 107/107;
the native proof's broad-suite ignore is covered by its separately invoked
passing test (3840x2160, 33,177,600 exact full-output bytes, no overflow).
The 49x17 four-way proof and explicit route proof also emitted positive
completion evidence. All 20 v5 validation commands now pass. The harness
reports 824 passed executions, including 13 explicit configuration-guard
returns that are recorded separately rather than treated as exercised GPU
coverage. Sixteen ignored executions are documented in the validation summary.
All four original native shell oracles pass; P and BP each report 2 full plus
11 sparse actual PPT8 selections. All 20 saved raw logs were checked exactly
against captured output; all 33 source hashes match the frozen version.
Ordinary release passed in 13m17.91s. All 30 comparison attempts ran; 28 have
full reports. Two fence timeouts are retained (initial desktop paint and
measured sample 1). Six complete UI B/BP reports fail positive-bounds eligibility
because applied bounds is zero; they remain in a separate descriptive table.
Best BXP shell median is 20.645 ms mean / 54.805 ms p99; no shell gate passes.
UI P median p99 is 38.105 ms, with one pass in three runs. Goal is not met.
All 33 source hashes and both binary hashes match; 30 raw logs and 30 parsed
metadata records were verified. Source remains frozen. No runtime job is live.

Seven old generated fine permutations retain their normalized token and
metadata hashes. Both new variants have [2,16,1] workgroups and unchanged
bindings/workgroup-buffer metadata. The first audit parser rejected whitespace
before a field colon; its script/error and corrected successful audit are
preserved. No shader change was needed for that parser correction.

Active validation plan has 20 commands, including an explicit invocation of
the ignored native-4K P4/P8 shared-coarse proof. Active comparison plan remains
comparison-plan-v2.json (30 ordinary-release attempts). Neither ignored tests
nor instrumented diagnostic captures count as performance success.
