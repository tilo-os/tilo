# Coding evaluation artifacts

This implements a narrow evidence boundary for the
[common evaluation contract](model-selection.md#common-tilo-coding-evaluation-contract).
It does not implement an agent orchestrator or contain model scores.

## Existing execution path

`apps/agent/src/session/agent_loop.rs` already implements the permission-filtered
discover/invoke/observe loop with enforced task bounds and a durable step path.
`apps/agent/tests/discovered_operations.rs` exercises actual broker dispatch,
capability gating and confirmation. Use that production path for agent work;
do not create a second Python tool executor or embed provider credentials here.

`tools/chip/coding_eval.py` only captures a readiness manifest and scores
acceptance evidence exported by an independent grader. It never invokes the
manifest's commands, invokes a model, starts an agent or interprets a tool call.
Connecting grader exports and reports to Tilo Experiment resources remains
implementation work. Until then, these are explicit file artifacts.

## Capture the readiness contract

From the repository root:

```bash
python3 tools/chip/coding_eval.py manifest \
  --repo "$PWD" --out /absolute/path/to/readiness-manifest.json
```

The immutable artifact records the repository commit, two named real Tilo
broker integration tests, exact ordinary Cargo arguments and the test source SHA-256.
On the configured remote builder, ordinary `cargo test` uses the existing
nextest CI profile, which writes `target/nextest/ci/junit.xml`. Elsewhere it can
use libtest. The manifest allows these explicit result formats; record the
observed one rather than assume ordinary Cargo always means libtest.
It refuses an existing output file. The source digest captures the working
tree's test source; ensure that source belongs to the selected base commit for
a clean baseline. A modified-source snapshot is not silently equivalent to the
base revision.

This pack is **platform readiness**, not a coding challenge and not a model
benchmark. Its tests demonstrate that the substrate needed to evaluate agents
can be observed. They contain controlled test providers and do not measure a
candidate model's capability. A readiness report may never be relabeled as a
model score.

## Author actual coding tasks

An evaluator-controlled manifest with `purpose: "coding_tasks"` uses the same
task/check structure. Each task contains:

- `id`, `category` and an explicit coding instruction.
- The immutable starting `repository_revision` shared by the pack.
- One or more acceptance checks with exact `argv`, source path/digest and
  nonempty, unique fully qualified Rust `expected_tests`.

Create packs separately for issue repair, multi-file feature work, integration,
long-context retrieval and RTL verification. Include production-path checks and
test each challenge against its base and a known correct repair. An issue-repair
acceptance test should detect the unrepaired bug, and a deliberately incorrect
repair should fail it. An invalid fixture blocks the task instead of becoming a
model failure. Do not weaken or ignore existing assertions to make a model pass.

Real held-out challenges, their private acceptance artifacts and candidate
model access remain to be assembled. No completed tests in the readiness pack
are presented as unsolved challenges. This prevents an initial tools check
from being mistaken for progress on model selection.

## Run record and independent grading

### Remote runner artifact export

The CI profile suppresses successful named console status lines. A summary such
as `2 tests run: 2 passed` cannot establish which tests passed and is rejected
by the console scorer. Prefer the runner's actual JUnit artifact.

The installed `rcargo --help` documents `--fetch PATH` for successful remote
build/check/run operations, explicitly excluding test; direct remote `nextest`
subcommands are refused. The ordinary Cargo shim forwards the option. For a
future readiness capture, the grader can run these sequentially:

```bash
cargo test --locked -p agent --test it discovered_operations::
cargo --fetch nextest/ci/junit.xml check --locked -p tilo-actions
```

The second command performs an actual check and mirrors the report into local
`target/nextest/ci/junit.xml`. Before any other test invocation overwrites that
shared report path, preserve its exact bytes under the run artifact directory.
Record the real test command as the check's `argv`; the fetch/check operation
is separate artifact-transport provenance. Record the runner run id and report
timestamp/UUID in the trace so an older or unrelated report is distinguishable.
Do not attribute a fetched latest report to an earlier test run after another
test has replaced it. This export command is a documented route, not a claim
that a report has already been retrieved or verified here.

Alternative console capture requires explicitly enabled named success output
on the real runner. Do not invent named PASS records from a summary or append
them to the log. If the runner cannot export the required observations, scoring
remains unavailable even when the process returned success.

The export route was exercised for readiness run
`5f3635b1-8122-47c7-a6e6-14fbc7c67c0e`: ordinary Cargo ran the two broker tests,
then `cargo --fetch nextest/ci/junit.xml check --locked -p shell -p chip-design --tests`
retrieved the report. Console run id and XML UUID matched. The actual JUnit
contained both expected `agent::it` cases with no failures, skips or retries,
and `test_outcome` accepted both. The preserved local artifacts are in
`target/chip-workbench/broker-readiness-5f3635b1/`: console, XML, freshly captured
manifest and a narrow readiness receipt. They are development artifacts, not
committed model scores.

This receipt records actual command identities, reported process status,
artifact hashes and capture-time repository/source identities. It deliberately
does not provide a full `run.json`: independent grader isolation, runtime action
catalogue, environment manifest, durable task trace and exact execution commit
were not exported. A source-linked platform check can be established without
pretending those missing model-evaluation fields exist.

The existing Tilo task/worker executes the task. A separate trusted grader
checks the candidate revision against the evaluator's acceptance source,
runs the exact checks and captures combined UTF-8 output and process status.
Acceptance tests and their records must be outside the evaluated agent's write
capabilities. The scorer does not enforce that isolation or authenticate the
grader; matching digests prove consistency only. Synthetic or model-authored
logs cannot establish real outcomes.

The exported `run.json` requires:

| Field | Meaning |
|---|---|
| `schema_version` | `1` |
| `mode` | `baseline_readiness` or `model_evaluation`, matching the pack |
| `manifest_sha256` | Digest of the exact manifest file bytes |
| `candidate_revision`, `harness_revision` | Immutable 40-digit commit identities |
| `environment_sha256`, `action_schema_sha256`, `trace_sha256` | Digests of the environment description, action schema and task trace |
| `harness`, `permissions`, `network_policy`, `grader_identity` | Explicit execution/access descriptions |
| `tasks` | Exactly one record per manifest task; no omissions or extras |

For `model_evaluation`, additionally record `context_tokens`, `token_limit` and
`attempts` as positive integers and a `model` object containing:

```json
{
  "id": "publisher/checkpoint",
  "revision": "immutable 40-digit checkpoint revision",
  "precision": "explicit weight, activation and recurrent-state formats",
  "weights_sha256": "64-digit weight-manifest digest",
  "tokenizer_sha256": "64-digit tokenizer digest",
  "template_sha256": "64-digit chat-template digest",
  "runtime_sha256": "64-digit runtime-manifest digest",
  "quantization_sha256": "64-digit quantization-recipe digest",
  "generation_config_sha256": "64-digit generation/budget-policy digest"
}
```

This is a shape illustration; its descriptive placeholders deliberately fail
validation. The recipe must describe even an unquantized checkpoint; generation
configuration includes seed, reasoning, sampling, compaction and cache policy.
Task timing, token usage, failed tools and energy/cost observations belong in
the trace or additional run fields. They are not inferred from test output.

Each task record has `id` and `checks`. Each check has `id`, exact `argv`,
`test_source_sha256`, integer `returncode`, artifact-relative `log` and
`log_sha256` and `format`. The artifact must show every expected named test's completion once.
A zero-test run, omitted check, build-only
log, ignored test, failure, nonzero exit or altered log cannot count as success.

Supported explicit formats are `nextest_junit` (preferred machine evidence),
`nextest_text` (console fallback when remote routing returns no XML) and
`libtest_text`. The manifest pins allowed `formats`; nextest formats also require
`expected_binary` such as `agent::it`. JUnit test cases must match both binary
and name. Failure, error, skipped and retry/flaky tags fail acceptance; nextest
sometimes omits skipped cases, so missing expected names also fail. This follows
the [official nextest JUnit format](https://nexte.st/docs/machine-readable/junit/)
and the repository's existing `tools/ci/nextest_report.py` conventions.

The console fallback requires each named PASS plus the completion summary.
Filtered-out unrelated tests can appear in its skipped count; an expected SKIP
does not pass. Missing PASS lines (for instance a profile suppressing status
output) cannot be accepted: supply JUnit instead. A plain `cargo test` routed
through nextest must be recorded as nextest evidence, never interpreted as
libtest output. Pytest/compiler-only formats remain unsupported.

## Score observed evidence

Place logs underneath the directory containing `run.json`, then run:

```bash
python3 tools/chip/coding_eval.py score \
  --manifest /absolute/path/to/readiness-manifest.json \
  --run /absolute/path/to/run.json \
  --out /absolute/path/to/score.json
```

Exit status is zero only when every task passes. Observed acceptance failures
write the report and exit `1`; invalid or missing evidence exits `2` without a
success report. Output replacement is refused. The report retains run
provenance, per-check results and explicit limits; it does not estimate coding
quality, throughput, energy or October 2028 competitiveness.

Compare models only within the same valid task/harness/environment contract,
with paired repeats and defined attempts. These artifacts do not pool vendor
leaderboards or rank incomparable task packs. Independently measured complete
task time and useful outcomes drive hardware selection; unit-test pass counts
alone do not establish a useful coding agent.

## Verification and next integration

```bash
python3 -m unittest discover -s tools/tests -p 'test_chip_coding_eval.py'
```

Tests use labeled synthetic logs to challenge scorer behavior; they are not
model observations. They cover missing/zero tests, failed/ignored tests,
nonzero processes, changed source/log evidence, manifest/run mismatch, missing
model provenance, traversal and the real CLI artifact/overwrite path. They also
cover nextest JUnit/console outcomes, skips, retries, wrong binary identities and
malformed schema/log types.

Next steps are Tilo supervised acceptance export, verified evaluator isolation,
complete pinned artifact verification, genuinely held-out tasks and actual
candidate runs when model access is available. Each is separate from this
implemented scorer and must provide its own production-path evidence.
