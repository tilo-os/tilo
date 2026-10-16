# Chip Design

Chip Design is a first-party Tilo application for a user's own engineering
project. It starts empty. Find **Chip Design** through system search and open
it in the workspace whose collaborators should share the project.

## Operator graph

The operator graph stores nodes, typed ports, positions and connection
candidates in the system-owned collaborative document. Its first operator
vocabulary covers FP32 inputs, RMS normalization, normalization weights,
SiLU, multiplication, addition, a projection input, matrix-weight inputs and linear
projections. Declared tensor shapes determine
port compatibility. The editor refuses sequential cycles, incompatible ports
and duplicate drivers. Concurrent conflicting connection candidates remain
inspectable; a deterministic projection chooses the active valid graph.

This graph describes structure. It does not execute a model, generate RTL or
qualify hardware. The shared GraphCanvas widget renders the graph; native
resource actions perform edits and system undo restores them.

Matrix nodes store only a bounded shape descriptor, never the matrix payload.
The weight port is FP32 `[output_width, input_width]`; a linear projection maps
`[..., input_width]` to `[..., output_width]`. Public `add-node` requires
`output_width` for these two kinds; `set-output-width` and the shared inspector
field edit it. `set-shape` and the shared shape field edit input dimensions;
the last input axis sets the normalization width. Changing a port shape retains incompatible connection candidates
as rejected edges, and undo restores the prior geometry and connections. Matrix
and output descriptors admit at most 2^32 elements, with output width at most
1,048,576. Existing activation-shape bounds remain in force. This is FP32
structural metadata, not quantized weight binding or a matrix multiplication
implementation.

## Evidence

### Staged architecture comparisons

The `architecture_study` evidence class describes a user's comparison of two
to sixteen implementations of the same inputs. It uses the existing
`import-verification`, child `inspect`, `read` and removal actions, so the native
view and automation inspect the same collaborative record. It stores no model
weights or executable artifacts. Detached summaries report producer claims;
importing one does not authenticate the referenced receipts or their artifacts.

The optional `architecture_study` object in a schema-version-1 verification
receipt contains `stage`, `original_receipt_sha256`, `producer_sha256`,
`input_identity_sha256`, `expected_comparison_words`, `links` and `variants`.
Stages are `prepare`, `numerical`, `fault`, `synthesis` and `aggregate`. A link
contains `receipt_sha256`, `producer_sha256` and the same
`input_identity_sha256`. Numerical and synthesis stages link preparation;
fault controls link preparation and numerical verification; an aggregate links
all four. A failed stage may retain partial observations and available links.

Numerical variants report cold programming, vector load and warm execution
cycles separately, accepted packed weight and descriptor bytes, cache reads,
quantizer packets, compared words and an output digest. A passed comparison
requires every variant's declared complete word count and output digest to
match. Generic synthesis variants report logic cells and storage bits, with
physical memory macros and synthesized-netlist equivalence explicitly
unqualified. A synthesis stage cannot contain numerical observations. Aggregate
variants contain both observation types; numerical and synthesis stages contain
only their own type. The native property view shows these distinctions and the
producer and stage identities.

The required stage check keys are respectively `input_identity_recorded`,
`all_variants_complete_outputs` plus `protocol_controls`,
`wrong_coefficient_rejected` plus `wrong_row_rejected`,
`generic_structure_observed`, and `linked_stages_match_inputs`. All summaries
exclude `full_model_execution`, `coding_quality`, `physical_ppa`,
`manufacturing`, `payload_authentication` and
`synthesized_netlist_equivalence`. Reported cycles are not a clock frequency or
token-rate measurement; generic cells and initialized bits are not physical
area or energy. Declared device directories are removed before sharing and
redacted from a failure description, using the same receipt import path.

### Other reports

Use **Import report** to choose bounded JSON through the consented framework
file chooser, or invoke the published import action with JSON. Storage
reports expose their assumptions and exclusions. Verification receipts expose
the declared checks, numerical coverage, source identities and limitations.
Import checks schema and internal consistency; detached receipt text does not
prove execution, publisher authenticity, coding quality or physical results.

All installed examples are explicitly synthetic contract fixtures used only
by tests and capture scenarios. A live project has no seeded experiments.
Real project evidence, checkpoint snapshots, benchmark inputs, RTL and
manufacturing studies live in that project's repository and workspace, not
inside the application or Tilo source tree.

## Agents and accessibility

The same resource tree and actions are available to people, assistive
technology and agents. Inspect the live vocabulary with
`tilo-control actions list` and the app's published resources. The `chip_design.operator_graph` child exposes `add-node`, `connect`,
`disconnect`, `move-node`, `remove-node`, `set-output-width` and `set-shape`. Its read view includes typed ports,
active edges and rejected candidates.
The app uses the existing workspace collaboration, permissions, attributed
history, search and theme services. It opens no network socket of its own.

Supported receipt families are versioned producer contracts, not an arbitrary
JSON evidence viewer. The Q4_K superblock envelope currently requires its
declared instrumentation and artifact roles; accepting a new producer or
coverage shape requires a corresponding schema extension.

Addition has two same-shape FP32 inputs (`left`, `right`) and one `result`
output. It represents residual connections through the same public graph
actions, shared canvas and system undo; it does not execute arithmetic.

## Reported execution and physical evidence flows

The `evidence_flow` receipt family presents a read-only dependency graph. Import
through `import-verification`, inspect its stage, artifact, and subflow child
resources, or open their search anchors. `open-evidence-subflow` on a subflow
child invokes the same view navigation as its button and refuses unloaded
references; its `read` content reports the resolved resource when available. It does not lower the structural operator
graph, execute artifact paths, authenticate detached reports, or establish model
quality, silicon measurements, signoff, or manufacturability.

The standard receipt envelope contains `schema_version: 1`, `evidence:
"evidence_flow"`, `status`, `checks`, `tools`, `artifact_sha256`, `excludes`, and
`evidence_flow`. Required exclusions are `artifact_authentication`,
`silicon_measurement`, `manufacturing`, and `model_quality`. Tool versions may
explicitly be unknown. Artifact keys are bounded relative references, with
lowercase SHA-256 declarations; their bytes are not opened by the importer.

`evidence_flow` contains `title`, `input_identity_sha256`, `stages`, `edges`, and
`subflows`. Each stage contains `id`, `title`, `status`,
`original_receipt_artifact`, `producer_sha256`, `input_identity_sha256`, `checks`,
`limitations`, and nullable `observations`. The receipt artifact must exist in
the envelope's artifact map. Stage observations use a strict tagged `scope`:

- `cpu_reference`: `comparison` (`exact` or `characterization`), `compared_words`,
  `differing_words`, and `output_sha256`. Exact comparison requires zero
  differing words. Successful characterization can retain backend differences.
- `rtl_simulation`: `outcome` (`completed` or `expected_refusal`),
  `refused_inputs`, `cycles`, `accepted_inputs`, `accepted_outputs`, and
  `output_sha256`. Cycles are observed simulation cycles, with no frequency or
  tokens-per-second conversion. Simulator wall time is not hardware latency.
  Expected refusal requires zero accepted outputs and a positive refusal count;
  it is distinct from completed numerical execution.
- `generic_synthesis`: `logic_cells` and `logical_memory_bits`. These counts
  do not establish licensed memory macros or physical area.
- `library_physical`: `library`, `clock_constraint_ns`, `setup_slack_ns`,
  `hold_slack_ns`, `routed_signal_nets`, `router_violations`, and
  `library_area_um2`. Timing and area are reported extracted library estimates.
  Negative slack and nonzero router violations remain visible observations;
  they do not become successful timing or signoff assertions.

Edges contain `from`, `to`, `kind` (`data`, `build`, or `verification`), and
`artifact`. Node IDs and referenced artifacts must exist; dependencies must be
acyclic. They describe declared dependencies, not proven lineage. Independently
sampled physical tiles must not be represented as routed versions of a different
connected execution subject.

A subflow contains `id`, `title`, and `summary_artifact`. Its artifact SHA binds
the exact raw imported summary bytes. The view offers navigation only after a
matching evidence-flow summary has been imported. Unloaded links remain references
and carry no execution or success claim. Each view admits at most 32 combined
stages/subflows and 64 edges; hierarchical summaries can cover a full design.
The bounded project capacity is 128 receipts, supporting a root plus 64 layer
flows while preserving the 1 MiB limit per imported report.

The `evidence-flow` capture scenario uses anonymous synthetic contract data only.
Live applications still start empty. Research producers and their adapters belong
in the independent inference-hardware repository.
