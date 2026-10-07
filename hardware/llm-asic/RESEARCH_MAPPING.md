# Library-mapped checkpoint slice

`tools/chip/map_weight_tile.py` maps a compiled checkpoint tile to the pinned
Nangate45 typical library, then runs the mapped netlist against the existing
independent integer oracle. The library's upstream README explicitly calls
it **non-manufacturable**, intended for research and EDA flow exploration.
This is a reproducible mapping experiment, not a process selection or physical
design result.

The library, README and Apache-2.0 license are pinned to OpenROAD Flow Scripts
revision `e97f852b01dfcaaa1031510d1bc1f4df7bb90139`, with individual SHA-256
identities enforced before tools run. Library fetching is separate from offline
observation and preserves the upstream license:

```sh
python3 tools/chip/map_weight_tile.py \
  --fetch-library target/chip-workbench/new-nangate45-library
python3 tools/chip/map_weight_tile.py \
  --sample target/chip-workbench/new-real-weight-tile \
  --library target/chip-workbench/new-nangate45-library \
  --out target/chip-workbench/new-research-mapping \
  --toolchain-image sha256:YOUR_RECORDED_IMAGE_ID
```

Run observation in the existing isolated EDA image with networking disabled.
Mount `tools/chip` and `hardware/llm-asic` read-only at their relative repository
locations, the compiled sample and library read-only, and only the selected
output parent writable. Each run requires a new output directory. Inputs are
bounded, hash-validated and copied into the run before synthesis. An independent
BF16 decoding route checks the sample's quantization metadata and regenerates
the coefficient wrapper, vectors and oracle testbench. Self-declared artifact
hashes or a testbench that merely prints a success marker cannot pass this gate.
Failed or
cancelled invocations produce their own receipt; previous receipts remain intact.

The flow synthesizes and flattens the real-coefficient fixture, maps registers
and combinational cells using the same library, and rejects remaining generic
hardware cells, unknown cell types and incomplete mapped hierarchy. Cell counts
and the complete area sum are checked against functional library models.
Library functional cell models are generated from the pinned
Liberty file. The mapped netlist runs the original synthetic-activation bench,
including stall/reset checks and independent integer answers. Missing tools or
missing simulator proof fail. The receipt includes tool versions/executable
digests, all input identities, output hashes, individual checks and limitations.

Actual observation in the initial EDA image passed input validation, library
mapping and mapped-netlist/oracle simulation for the four-row, 128-column real
weight sample. Yosys reported 2,600 mapped hardware cells (excluding its
`$scopeinfo` metadata cell), with a library area sum of 3,784.382. That sum is
the mapped cell areas in this research library, excluding placement, routing,
clock distribution, I/O and other chip components. It must not be extrapolated
to complete-model area or a commercial process. The ABC delay target is
1,000 ps; it is a mapping request, **not** measured clock frequency or timing
closure. No static timing, routed layout, power measurement or manufacturing
access is established.

Primary references:

- [Pinned platform README](https://github.com/The-OpenROAD-Project/OpenROAD-flow-scripts/blob/e97f852b01dfcaaa1031510d1bc1f4df7bb90139/flow/platforms/nangate45/README.md)
- [Pinned library](https://github.com/The-OpenROAD-Project/OpenROAD-flow-scripts/blob/e97f852b01dfcaaa1031510d1bc1f4df7bb90139/flow/platforms/nangate45/lib/NangateOpenCellLibrary_typical.lib)
- [Pinned license](https://github.com/The-OpenROAD-Project/OpenROAD-flow-scripts/blob/e97f852b01dfcaaa1031510d1bc1f4df7bb90139/flow/platforms/nangate45/LICENSE)
