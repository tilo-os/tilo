# Shared W4 product-generation experiment

`rtl/integer_group_tile_shared.sv` is an experimental alternative to the
published `integer_group_tile.sv`; it preserves the signed W4/A8 integer,
32-bit group sum, ready/valid, stall, overlap and synchronous reset contract.
It computes activation multiples 0 through 8 once, then each lane selects the
magnitude of its fixed signed nibble and applies the coefficient sign. Twelve
bits are used before shifts, sums and negation, including `-128 * -8 = 1024`.
Group rescaling remains external and unimplemented.

`tools/chip/compare_product_tiles.py` validates and independently regenerates
all original sample artifacts through the production artifact validator before
using them. The alternative wrapper is derived by changing exactly one module
name; its constants, interface, activation schedule and integer oracle are
identical. The two RTL files are mapped separately with identical pinned
Nangate45 library, Yosys passes and ABC `-D 1000` setting. Each original RTL and
mapped netlist executes the same oracle/protocol testbench. Every mapped cell
must have a functional library model and a consistent nonzero area.

A separate **synthetic** 16-lane, group-size-one fixture checks each coefficient
`-8..7` against each activation `-128..127` separately: all 4,096 products,
without sum cancellation. The same production bench exercises input bubbles,
output stalls, reset with a pending output, reset during a group and concurrent
result consumption/input acceptance. These synthetic lanes are not the area
comparison and are not evidence of model quality.

Run offline after compiling an immutable real-weight sample and fetching the
pinned research library using the existing tools:

```sh
python3 tools/chip/compare_product_tiles.py \
  --sample /absolute/path/to/sample \
  --library /absolute/path/to/nangate45-source \
  --out /absolute/path/to/new-comparison-run \
  --toolchain-image sha256:<actual-toolchain-image>
```

The output directory must be new. `comparison.json` records failures, input and
source hashes, exact tool versions/executable hashes, per-variant library area
and cell count, all log/netlist hashes and the observed percentage area change.
The image identity is caller-declared; execution versions and executable hashes
are independently observed. Tool unavailability is a failure, never a skip.

This compares library-cell area for the sampled integer tile. ABC's delay target
is an optimization setting, not measured timing closure. There is no physical
layout, power measurement, manufacturable library, full-model area estimate,
throughput measurement or calibrated quantization-quality claim. Four- and
eight-lane real samples use synthetic activations. The bounded eight-lane
compilation reads 2,048 unique BF16 payload bytes plus model index/config/header
metadata; total network transfer exceeds the payload count.

## Observed result

The isolated Fedora 43 EDA image
`sha256:da8c943173a98e6a14e9734bcfd2583c64fbf8885228c06f294aab592e1ae403`
ran Verilator 5.046 and Yosys 0.63. Both final comparisons passed original RTL
and mapped-netlist oracle/protocol checks, and the exhaustive 4,096-product
check. Final receipts are retained locally at
`target/chip-workbench/shared-product-experiment/runs/{four-final,eight-final}/comparison.json`;
these generated outputs and build binaries are not source artifacts.

| Real sample lanes | Baseline cell count | Shared cell count | Baseline library area | Shared library area | Area change |
| --- | ---: | ---: | ---: | ---: | ---: |
| 4 | 2,600 | 2,667 | 3,784.382 | 3,847.956 | +1.680% |
| 8 | 5,148 | 4,832 | 7,453.852 | 7,173.222 | -3.765% |

Cell counts exclude Yosys scope metadata. Area is the sum of the pinned
library's cell-area attributes. The eight-lane sample's first four raw BF16
rows exactly match the four-lane sample (SHA-256
`e693689b519f229d21363cf92c52be42d0c2de360f79da71ead76cf214a2404a`).
A separate mutation removing negative-coefficient sign handling failed the
exhaustive oracle with an actual product mismatch.

The candidate is worse at four lanes and modestly smaller at eight for these
exact fixed coefficients. This does not establish a representative layer-area
reduction or a timing/energy benefit; shared fanout and routing remain
unmeasured. The published baseline remains the reference implementation.
