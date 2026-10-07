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
