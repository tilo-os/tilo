# Reuse frozen native binaries with a reviewed Windows provider correction

This optional development admission accepts a direct child of source4583 whose
complete tracked-tree difference is exactly:

- `os/dev/windows_provider.py`
- `os/tests/test_windows_provider.py`

No Rust, Cargo, Containerfile, other source, or file-mode change is accepted.
The corrected checkout must be clean. The workflow fetches two source commits
and checks the admission before building the tools container; the controller
checks it again before registry/build work.

Workflow inputs are `native_source_compatibility` (exact JSON bytes) and
`native_source_compatibility_sha256` (independently approved SHA256). They require
all reconstructed-native receipt/approval fields and a corrected source revision.
Legacy native import, native compilation and missing/invalid approvals cannot
use this path. Choose a coherent helper compatible with the frozen source tools;
changing this workflow alone does not make newer crypto helpers compatible.

Admission schema:

```json
{
  "schema": 1,
  "purpose": "tilo-windows-provider-native-source-compatibility",
  "native_source_revision": "4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc",
  "assembly_source_revision": "REVIEWED_DIRECT_CHILD_COMMIT",
  "native_receipt_sha256": "EXACT_COMPLETED_NATIVE_RECEIPT_SHA256",
  "approved_reconstruction_sha256": "INDEPENDENT_NATIVE_RECONSTRUCTION_SHA256",
  "changes": {
    "os/dev/windows_provider.py": {
      "old_blob": "ORIGINAL_GIT_BLOB_ID",
      "new_blob": "REVIEWED_GIT_BLOB_ID",
      "old_sha256": "ORIGINAL_BLOB_BYTES_SHA256",
      "new_sha256": "REVIEWED_BLOB_BYTES_SHA256"
    },
    "os/tests/test_windows_provider.py": {
      "old_blob": "ORIGINAL_GIT_BLOB_ID",
      "new_blob": "REVIEWED_GIT_BLOB_ID",
      "old_sha256": "ORIGINAL_BLOB_BYTES_SHA256",
      "new_sha256": "REVIEWED_BLOB_BYTES_SHA256"
    }
  }
}
```

Blob hashes are calculated from Git object bytes, not newline-transformed
working files. Source epoch and assembly epoch are independently read from
their commits. After the complete-tree proof, native expected inputs differ
from assembly inputs only in source revision and epoch. The existing importer
still requires exact original receipt inputs, approved reconstruction hash,
immutable registry identity and full actual payload inventory. It never changes
the completed native artifact or silently compiles replacement binaries.

Assembly uses the corrected source revision and its own epoch throughout.
Public evidence includes the full compatibility proof in `inputs.json`,
`reconstructed-native-adoption.json`, `result.json` and a separate
`native-source-compatibility.json`; the workflow retains those evidence files.
The adoption receipt records the original native expected inputs separately.

This compatibility proof says nothing about correctness of the reviewed Python
fix itself; review that change independently before approving its exact blobs.

## Rootfs-only follow-up

Purpose `tilo-rootfs-native-source-compatibility` additionally requires
`provider_revision` and `wpe_bridge`. The explicit chain is frozen source,
provider-only correction, then a Containerfile-only correction. Full tree diff
contains exactly the two provider files and Containerfile. Prefix through the
rootfs FROM, suffix from the split FROM, and every FROM directive must remain
byte-identical. Exact changed blobs still require independent review.

`wpe_bridge` contains `original_receipt_json`, `original_receipt_sha256`,
`assembly_receipt_json`, and `assembly_receipt_sha256`. These are exact UTF-8
receipt bytes, not reserialized historical documents. The new receipt must
differ only in audit.compatible_recipes["os/Containerfile"] and audit_sha256.
Encode the new audit as sorted compact JSON followed by LF to calculate its
SHA256. Preserve original unknown build provenance and all image/config/payload
identities. Save both receipts independently. This is a new consumer compatibility
review; it does not claim the historical WPE build used the new recipe.

The workflow uses the new receipt normally. Before reproducing native inputs,
the controller verifies the selected receipt's exact bytes against this bridge.
Only then does it restore the original native source epoch, source revision,
Containerfile hash, and WPE receipt hash for the unchanged strict native importer.
Actual assembly input evidence continues to identify the new source and receipt.
Three-deep authenticated source checkout is required. No compilation fallback
is introduced, and WPE payload/config inspection still runs in its importer.

### Reviewed runtime policy follow-up

An optional `rootfs_revision` adds one final policy commit to the explicit
frozen-source → provider → rootfs-recipe → runtime-policy chain. The final
commit must change exactly `os/scripts/verify-runtime-crypto-packages.sh`,
`os/usr/libexec/tilo-security-audit`, and `docs/security/boot-chain.md`, each
with independently reviewed old/new blob identities in the admission. The
recipe commit must still change only Containerfile. No other tracked changes
are accepted. The unchanged recipe must reference all three policy inputs
inside rootfs, with none of those references before rootfs or after split.

The builder's broad `COPY . /build` includes these files as context; the
frozen Cargo/build scripts do not consume them to produce native outputs.
This is reviewed source compatibility, not identical build-context provenance.
Native receipt identities remain original and the real assembly source remains
recorded. The recipe and WPE consumer receipt are unchanged by the policy
commit. Authenticated source checkout needs depth four.

### Reviewed optional runtime-test follow-up

An optional `policy_revision`, valid only with `rootfs_revision`, adds one
Containerfile-only commit after the reviewed runtime-policy commit. Every
parent in the five-commit chain is checked, and both the follow-up diff and the
complete frozen-source diff remain explicit. Recipe changes must remain inside
the existing rootfs stage; native stages, split/UKI stages and stage directives
cannot change. The approval pins the final recipe blob and SHA256.

The deferred AirPlay execution leaves the installed test scripts, executable
modes, plugin probes and protocol tests intact. This is not a successful AirPlay
validation claim. Its new recipe requires a correspondingly updated WPE
consumer receipt: only the reviewed recipe hash and canonical audit hash may
change. Original native inputs continue to use the original recipe and WPE
receipt. Authenticated source checkout needs depth five.
