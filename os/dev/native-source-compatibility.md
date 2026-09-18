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
