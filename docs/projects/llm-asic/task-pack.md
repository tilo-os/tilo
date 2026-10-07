# Private coding task packs and runtime integration

The next model-selection input is a calibrated task pack, not another platform
readiness score. `tools/chip/task_pack.py` validates independently captured
acceptance outcomes for the unrepaired base, a correct repair and a deliberately
wrong repair. It runs neither agents nor test commands.

No genuinely held-out task pack or model scores are included in this repository
yet. Publicly committed prompts and repairs cannot be represented as private
holdouts. The validator establishes an artifact contract; actual challenges
must be authored and independently reviewed.

## Runtime facilities observed

A read-only inspection of this development host found an NVIDIA RTX 4070 Laptop
GPU with 8,188 MiB VRAM and 32,038,660 KiB total RAM. `podman` and `nvidia-smi`
are available; `vllm`, `ollama`, `tilo-control` and the model-manager executable
were not found on PATH. No default system model catalogue or admin control
socket was observed at the inspected paths. This is a bounded host observation,
not a claim that no remote inference facility exists.

The nominal Qwen27B INT4 payload alone exceeds dedicated VRAM. Host-memory
offload is a possible experiment, not an established workable agent runtime;
weight scales, context state and latency still matter. No weights were
downloaded, providers called, user credentials inspected or GPU jobs started.

The implementation already provides the correct integration components:

- `crates/agent/local-model-service`: signed model/runtime catalogue, installation,
  immutable release selection and supervised vLLM container lifecycle.
- `crates/agent/model-gateway`: provider and locally managed model access through
  the launcher-issued boundary.
- `apps/settings/src/panes/agent_models.rs`: explicit model/provider management.
- `apps/agent/src/session/agent_loop.rs`: permission-filtered task execution.

Use an adequately provisioned Tilo device or workspace host with a reviewed
signed runtime/model entry, then select its immutable model through the existing
gateway. A hosted provider can be a quality reference but an unattested API
alias cannot satisfy local-checkpoint provenance. Do not bypass the gateway or
give provider keys to the evaluated agent. Adding a supported catalogue entry
and resource-scoped grader export remains work; these tools do not install it.

## Pack contract

Keep the pack and acceptance sources in an evaluator-owned filesystem resource
outside the evaluated agent's write/read grants. Give the agent the task prompt,
base checkout and permitted development tools; keep reference repairs, mutants
and private tests inaccessible. Declaration of privacy is not enforcement.

A pack has `schema_version: 1`, `pack_id`,
`visibility: "evaluator_private"` and a nonempty `tasks` array. Each task has:

| Field | Required content |
|---|---|
| `id`, `category` | Stable identity and issue-repair/feature/integration/etc. category |
| `base_revision` | Immutable 40-digit repository commit |
| `prompt` | Pack-relative UTF-8 instruction file and SHA-256 |
| `acceptance_manifest` | Pack-relative existing coding-evaluation manifest and SHA-256 |
| `calibration` | Exactly `baseline`, `reference` and `mutant` artifact references |

All file references are objects with `path` and `sha256`. Calibration references
add their immutable `candidate_revision`; all three revisions must differ and
the baseline must equal the task base. The acceptance manifest uses
`purpose: "coding_tasks"`, targets the same base and names exactly this task.
Its check structure is described in [evaluation.md](evaluation.md).
Its instruction and category must match the prompt file and pack task exactly
(ignoring surrounding instruction whitespace).

Each calibration artifact has `schema_version: 1`, `candidate_revision`,
`grader_identity` and `checks`. Check observations use the existing evidence
format: exact argv/source digest, process return code, artifact-relative log
path/hash and explicit libtest/nextest format. Logs are relative to the proof
file's directory. The validator invokes the same acceptance parser used for
candidate results, so build failure, zero tests or missing observations cannot
serve as the baseline failure evidence.
Negative calibration also requires a runner-observed failure of at least one
expected acceptance test. A nonzero process exit with all expected tests passing,
a timeout or a skipped test does not demonstrate that the tests detect the bug.

Required observed outcomes are **baseline fails, reference passes, mutant
fails**. These are necessary calibration conditions. They do not establish
test completeness, privacy or absence of training contamination. Review each
reference patch and its production-path coverage; record why the mutant is a
plausible wrong repair instead of an unrelated syntax error.

Validate privately:

```bash
python3 tools/chip/task_pack.py /absolute/private/path/pack.json \
  --out /absolute/private/path/calibration-report.json
python3 -m unittest discover -s tools/tests -p 'test_chip_task_pack.py'
```

Outputs refuse overwrite and retain task/calibration identities and limits.
The public repository contains validator tests with explicitly synthetic
observations; they are not task-pack calibration or candidate-model results.

## Concrete execution sequence

1. Prepare independent evaluator-owned acceptance tests and three isolated
   checkouts per challenge. Execute calibration checks through the existing
   Tilo worker/terminal path; preserve exact reports, tool/runtime identity and
   source hashes. Validate the private pack.
2. Provision a supported model through Settings/local-model-service, pin its
   complete weight/runtime/tokenizer/template/generation artifacts, and select
   it through the model gateway. Inspect resources and available actions using
   the existing semantic door.
3. Dispatch ordinary Tilo tasks with the same instruction, base, allowed tools,
   permissions and budget. Agents work in isolated proposals; task traces carry
   authorship, action observations, source revisions and stop reasons.
4. A grader outside candidate write authority obtains the candidate source
   snapshot and applies its own acceptance tests. Use exact check commands and
   preserve machine reports; bind report UUIDs and hashes to that execution.
5. Export the complete provenance `run.json` required by `coding_eval.py`, then
   score it. Missing runtime catalogue, trace or execution identity blocks a
   full score. Narrow receipts remain explicitly narrower evidence.
6. Compare paired repeated tasks for quality, failure types, elapsed time,
   retries and actual resource consumption. Hold model/harness/environment
   contracts constant. Quantization variants get the same task pack.

Connect pack, task, proposal, calibration and result files to existing Tilo
resources/typed Effects and the engineering workbench. This is an integration
plan, not a second scheduler implemented in Python. Process supervision,
cancellation, task dispatch, model access and permissions remain platform-owned.
