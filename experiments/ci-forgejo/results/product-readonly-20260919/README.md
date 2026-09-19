# Product read-only integration audit

Snapshot collected 2026-09-19 around 00:28 UTC, current main
`4b8277286ca108cc9445dbc0ddaaea72e1afc3d1`. This is historical evidence, not
authorization to merge. API records retain relevant fields; large profile,
repository and patch metadata are omitted. No product permission or branch changed.

| PR | Head prefix | Required check | Independent approvals | Classification |
|---|---|---|---|---|
| 38 | 3971d4393d | 16511 failed | none | Input firewall/routing boundary changes |
| 36 | f253957b7d | 16435 failed | none | CI and release policy changes |
| 34 | aab5cedbe0 | 16417 cancelled | none | CI and release policy changes |
| 32 | aab5cedbe0 | 16417 cancelled | none | Same head as 34 |

All four heads are behind the observed main: a fresh fast-forward integration
candidate is needed before certification. Their trusted harness also differs from
the reviewed current base. Lint successes do not substitute for required checks.
There is no routine product PR ready to exercise automatic integration here.
These blockers are factual findings, not a request to waive or weaken guards.

## Small deployable next slice

Keep the worker read-only and define product policy separately from the private
execution pilot. From the reviewed workflow and a trusted recomputed selector
plan, enumerate `builder`, `select`, `security`, `checks`, every expected native
matrix job and `report-check-result`. Never infer expectations from jobs returned
by the candidate run. Every native matrix job must succeed in its current attempt;
absent, failed, cancelled or unexpected jobs reject the candidate.

The audit now accepts explicit `allowed_skips` only for `check.yml` PR events:
`report-check-result` follows the reviewed workflow's non-PR reporting condition.
`affected (runner)` is the current inert sentinel and additionally requires
`zero_rust_selection`. This proof contains exact base/head/trusted revision,
SHA-256 of committed selector bytes and its complete JSON plan. It is recomputed
by the audit, not copied from candidate output. The operator must supply a clean
checkout at the reviewed current base, including trusted Cargo metadata/config;
candidate code is never checked out or executed. The committed selector runs with
isolated Python imports, using the candidate only as a Git diff endpoint. Its
protected closure must include the selector, workflows and all transitive build
policy inputs. Missing proof, dirty checkout, changed harness or nonempty Rust
selection rejects the skip. Keep receipts outside that checkout. Use native Linux
with unchanged Git blob bytes; automatic newline conversion is rejected.
Deletions, renames and mode/type changes also reject zero-Rust certification:
the trusted selector's filesystem-sensitive helper checks must not see a file
at the base that no longer exists with the same type at the candidate.

The supplied plan must say `rust_required: false`, zero selected packages and an
empty matrix, with nonempty changed-file evidence. Manual/full execution receives
no PR skip allowance. Policy remains operator-owned: this change does not automate
policy derivation or prove completeness of a human-supplied native matrix. A product
worker must generate that complete matrix from trusted workflow/selector sources
before product execution can be considered. Security, CI-policy and release changes
remain on the trusted human-policy route. Existing pilot execution remains hard
limited to the private repository.
