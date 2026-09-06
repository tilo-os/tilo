# Validation v6 passed; measured performance gates remain unmet

All 20 planned validation commands pass. Production and vendor lint are warning-clean; the test-inclusive changed-line/compiler gate reports zero relevant diagnostics. The test harnesses report 839 passes, including 13 explicit configuration-guard returns that are not GPU coverage, plus 18 ignored executions. Both new exact native-4K clip proofs were explicitly run and passed (C and BPC); the shared-input opening/closing-clip comparisons and oversized guard passed under default and C. Old shader token/metadata audits pass for all generated candidates.

All 20 saved raw logs were verified against captured output, and all 34 source/config/evidence hashes match the frozen version. See v6-validation-summary.json for per-command counts, actual proof output and coverage limitations. Earlier v1-v5 failures are retained. The prior independent default GPU-oracle instability is not declared resolved by these passing runs.

The ordinary release build and fixed 30-attempt comparison are now complete and audited: 30/30 complete reports, no shell gate passes and one isolated UI gate pass. See RESULTS.md and comparison-summary.json. No default promotion, commit or push has occurred; the goal remains unmet. The next investigation is geometry-only reuse feasibility, not another claim of a clip speedup.
