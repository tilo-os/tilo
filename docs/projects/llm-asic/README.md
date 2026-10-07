# LLM ASIC and Tilo engineering workbench

## Objective and current state

Develop a useful model-specialized LLM ASIC for agentic coding, together with
Tilo applications that make complex engineering work with human and agent
collaborators visible, reproducible and controllable. The product must have a
credible usefulness case in October 2028. The initial $100,000 is development
capital; complete-model fabrication cost and financing remain unresolved.

Qwen3.8-27B is the initial model/compiler target, **not a frozen fabrication
checkpoint**. [Model selection](model-selection.md) records the current
alternatives, source caveats, storage calculations and evaluation contract.

This directory provides a research baseline, captured model metadata and an
[architecture exploration contract](architecture.md). The
[coding evidence scorer](evaluation.md) separates platform readiness from
candidate model outcomes. It does not establish measured model quality,
quantization quality, full-model RTL correctness, physical area, power,
timing closure or a manufacturing quote.
The [arithmetic tile](../../../hardware/llm-asic/README.md),
[bounded real-weight compiler](../../../hardware/llm-asic/REAL_WEIGHT_SLICE.md),
[recurrent contract](../../../hardware/llm-asic/RECURRENT_CONTRACT.md) and
[research-library mapper](../../../hardware/llm-asic/RESEARCH_MAPPING.md)
provide executable experiments toward that objective. They cover limited
contracts; they do not complete the ASIC. [Private task calibration](task-pack.md)
provides the acceptance gates for future common coding evaluations.

## Reproducible inputs

The metadata CLI lives at `tools/chip/model_profile.py`:

```bash
python3 tools/chip/model_profile.py --help
python3 tools/chip/model_profile.py profile \
  docs/projects/llm-asic/models/Qwen3.8-27B.json \
  --logical-parameters 27000000000 --context-tokens 32768 \
  --weight-bits 4 --kv-bits 16
```

The result is an analytical storage bound, not an ASIC performance prediction.
The parameter count is an explicit input: the advertised text-backbone count
and the repository's complete tensor inventory are different quantities.
Unsupported cache architectures are reported as unsupported rather than given
the ordinary grouped-query-attention formula.

For a new capture, choose a new destination instead of replacing an existing
evidence record:

```bash
python3 tools/chip/model_profile.py snapshot Qwen/Qwen3.8-27B \
  --out /absolute/path/to/new-snapshot.json
```

Capture downloads configuration and Hub metadata only. A record contains the
model id, immutable configuration revision and URL, canonical configuration
SHA-256, configuration, and license/tensor inventory observed at capture.
The captured license label and inventory are not a pinned full license or a
complete per-tensor manifest. Subsequent evaluation must additionally pin
weights, tokenizer, chat template, license text, runtime and harness. No model
weights or executable remote model code are downloaded by this CLI.

For checkpoint storage, use the separate bounded header inventory:

```bash
python3 tools/chip/tensor_inventory.py \
  docs/projects/llm-asic/models/Qwen3.8-27B.json \
  --out /absolute/path/to/new-tensor-inventory.json
```

This fetches the pinned safetensors index and bounded header ranges, validates
dtype/shape/byte-span agreement and complete non-overlapping shard layout, and
records source URLs and digests. It refuses servers that do not honor ranges.
The initial Qwen capture contains 1,199 tensors in 18 shards: 27,781,427,952
BF16 storage elements and 55,562,855,904 payload bytes. These describe the
complete stored checkpoint, including components outside the text backbone.
This inventory CLI fetches or verifies no payload bytes. The separate real-weight
compiler fetches a bounded sample, with its own evidence. Stored elements in packed formats
must not be relabeled as logical model parameters.

## Tilo application model

Follow the existing [resource/action model](../../architecture/README.md),
[agent architecture](../../architecture/agents.md) and
[collaboration boundary](../../architecture/collaboration.md). The workbench
must use `ResourceApp`, `tilo-resource` and the framework dispatcher. It must not
introduce a parallel action registry, private CRDT or app-specific sync loop.

| Resource | Shared state and actions | Views and outside work |
|---|---|---|
| Project | Requirements, decisions, task dependencies and evidence links | Dependency graph, milestone and decision views |
| Proposal | Assignee, base revision, intended change, review and integration status | Diff and linked worktree/PR inspection |
| Experiment | Immutable inputs, hypothesis, result provenance and comparison | Typed Effect starts a supervised, cancellable job |
| Chip design | Architecture, constraints, module references and annotations | Hierarchy, RTL, waveform and physical-design views |
| Artifact | Content address, producer, input hashes and evidence class | Open at tensor, trace sample, violation or timing-path anchor |

These are a proposed resource mapping, not implemented application claims.
Document actions record decisions and edits; typed Effects run simulations,
evaluation and EDA jobs; local Views navigate results. Job completion publishes
results through resource actions. Large reports and waveforms belong in
content-addressed filesystem artifacts with bounded resource publication, not
unbounded CRDT arrays. Search, history, undo, presence, settings and theming use
the platform services.

Agents retain non-human identities and explicit capabilities. Worker execution
must preserve workspace sandboxing and scoped access; tool availability is not
authority. Immutable builds consume source revisions and input digests even
when people collaborate on mutable plans. Concurrent editing and a successful
CRDT merge do not prove an RTL or software integration is correct.

Branch creation, proposal inspection, integration, run cancellation and opening
results must have semantic actions reachable by people, accessibility clients
and agents. A graphical-only operation is unfinished. Follow the existing
`tilo-control` door for verification and existing Tilo agent/runtime services
for delegation. General project workflow improvements belong in reusable
platform components; chip-specific interpretation belongs in the workbench.

## First milestone backlog

The first milestone is an inspectable engineering baseline: a common coding
evaluation contract, model profiles with honest bounds, a real Tilo proposal/run
workflow and one reproducible chip-design experiment. It is not tapeout.

### Tooling implementation

1. **Inspect and extend existing project/agent surfaces.** Locate production
   task, process, branch and action paths before adding a new abstraction.
   Acceptance: a task can link a real proposal, source revision and result;
   all operations are available through semantic actions.
2. **Expose profiles and evidence in Tilo.** Import metadata and analytical
   reports as typed resources. Acceptance: configuration provenance and
   exclusions are visible; estimates cannot be confused with measured results.
3. **Supervise reproducible jobs.** Record tool/input identities, progress,
   cancellation and failure. Acceptance: a completed result identifies its
   exact input revision; missing observations fail instead of silently skip.
4. **Integrate concurrent proposals.** Connect existing branch/worktree and PR
   paths to tasks. Acceptance: a stale-base proposal and a merge conflict are
   visible; successful integration includes the relevant production-path checks.
5. **Add chip artifact views incrementally.** Start with tensor/operator
   inventory and actual synthesis reports, then waveform and physical-design
   navigation. Acceptance: selecting a result resolves to the producing source
   and experiment. Use shared controls, framework search and OS chrome.
6. **Upstream coherent increments.** Narrow tests, required compile checks,
   repository hooks and task-branch PRs accompany each change. CI monitoring is
   a side task while independent authorized work proceeds.

### Model and silicon research

1. **Run the common evaluation.** Establish capability, failure classes and
   context requirements before setting a quality floor or freezing precision.
2. **Inventory complete computation and state.** Include prefill, decode,
   DeltaNet recurrence, attention, output projection, sampling and host work.
3. **Evaluate numerical implementations.** Sweep weight, activation and state
   precision using complete agent trajectories and a bit-exact reference.
4. **Compare physical implementations.** Synthesize and place representative
   fixed-weight/ROM-backed blocks under real process constraints. Extrapolation
   must identify omitted interconnect, interfaces, state and physical overhead.
5. **Establish manufacturing options.** Obtain process/IP, packaging, test and
   fabrication quotes tied to actual requirements. Do not infer a complete LLM
   chip's feasibility from an inexpensive shuttle's headline price.
6. **Build full-model hardware execution and independent verification.** FPGA
   and test-silicon choices follow the risks established above. A matrix kernel
   alone does not prove the complete model or system economics.

Each item produces a source-linked artifact and an explicit status. Quality,
numerical accuracy, physical feasibility and commercial production are distinct
gates; no paper estimate can substitute for their missing evidence.
