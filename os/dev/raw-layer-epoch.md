# Raw assembly layer timestamps

`compile-development-os.py --assembly-layer-epoch-anchor <full-commit>` enables
stable OCI layer normalization only for verified reconstructed-native, adopted-WPE,
raw-only development assembly. The anchor must be an ancestor of the exact assembly
source. Its Git commit timestamp cannot exceed the assembly source timestamp.

The controller records the anchor, source revision, both timestamps and policy
`raw-layer-ancestor-v1` under `assembly_registry_cache.layer_normalization` in
`cache-configuration.json`. Existing source/helper identity evidence still applies.
`SOURCE_DATE_EPOCH`, its build argument, native compatibility inputs, raw handoff
receipts and finalization/signing epochs retain the actual source commit timestamp.
Only the raw Podman invocation's layer normalization timestamp changes. No cache
entry constitutes artifact acceptance; normal raw-layer audit and finalization apply.

To reuse run 8921's populated cache on a subsequent frozen-source correction, use
anchor `344257f56275919abc63181d8954e838f9b5303e` (timestamp `1789753377`) on a
source branch descending from that exact commit. Its cherry-pick on main is not
an ancestry substitute. The selected source must contain the explicit
`TILO_RAW_LAYER_EPOCH` API in `build-image.sh`; older sources fail closed.

## Source compatibility admission

The current frozen-native verifier permits rootfs Containerfile corrections, not
build-image.sh changes. Before deploying this API to a frozen-source successor,
add a separately reviewed correction step for exactly `os/dev/build-image.sh`:
record exact parent/source commit, old/new blob IDs and SHA256, require the complete
tree diff to contain only that path, and verify this precise API hunk. Keep existing
provider/rootfs/policy checks and original native receipt unchanged. Do not broadly
allow arbitrary build scripts or change the native source epoch. This implementation
does not itself expand that admission or change workflow dispatch inputs.
