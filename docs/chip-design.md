# Chip Design

Chip Design is a first-party Tilo application for a user's own engineering
project. It starts empty. Find **Chip Design** through system search and open
it in the workspace whose collaborators should share the project.

## Operator graph

The operator graph stores nodes, typed ports, positions and connection
candidates in the system-owned collaborative document. Its first operator
vocabulary covers FP32 inputs, RMS normalization, normalization weights,
SiLU, multiplication, a projection input, matrix-weight inputs and linear
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
