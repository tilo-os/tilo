# Explicit reconstructed native payload adoption

The legacy native importer continues to reject reconstructed receipts. Full OS
assembly opts in with all three independent inputs:

```text
--reconstructed-native-receipt /work/reconstructed-native.json
--reconstructed-native-sha256 EXACT_RECEIPT_BYTES_SHA256
--approved-reconstruction-sha256 REVIEWED_RECONSTRUCTION_SHA256
```

The reconstruction approval must come from reviewing the producer's derivation,
retained inventory and missing targets. Do not fill it automatically from an
unreviewed receipt. The importer authenticates exact receipt bytes, repository
manifest/config, metadata/reconstruction hashes, expected source inputs and the
entire executable/runtime/app-store inventory. Failure stops assembly; there is
no legacy-import or native-compilation fallback.

This path is restricted to ARM full-build using application source
`4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc`. The application's original Containerfile,
build script and native-input contract remain authoritative. Only the named
builder image context receives the verified complete reconstructed payload.

## Source and controller separation

Run the reviewed controller with `--source-dir /work/source` and
`--controller-revision FULL_CONTROLLER_COMMIT`; `--revision` remains the frozen
application commit. Both checkouts must be clean and match their independently
pinned revisions before source helpers load. `controller-source.json` records
both revisions, controller bytes and source recipe hash. Python bytecode writes
are disabled. Existing callers without a separate source directory retain the
original single-checkout behavior.

The manual `development-compile.yml` workflow exposes `source_revision`,
`helpers_revision` and the three reconstruction inputs. Main remains the workflow
orchestrator. `helpers_revision` selects a separate authenticated controller
checkout, which is copied to `/work/repo`; its exact commit becomes
`--controller-revision`. The application source is another authenticated sibling
checkout, copied separately to `/work/source`. Neither is patched with changes
from main. Reconstructed frozen-source assembly requires an explicit helper pin.
Existing callers selecting neither source nor helpers retain main's behavior.

For the current source4583 assembly, coherent helpers
`5a410e5303f11b4f5ff27aefc4ba9fc26df5c77c` preserve the compatible OpenSSL signing
setup, reconstructed import and native-only kernel independence. Newer main's controller compiles a newer crypto
CLI, so choosing only source4583 while executing main's controller is insufficient.
The tools recipe is independently verified against the **application source**
Git blob, retaining its required OpenSSL/sbsigntools dependencies. Public evidence
includes `tools-source.json`, `controller-source.json` and
`reconstructed-native-adoption.json`.

Native8077's attempted run used the original tools recipe but is terminal.
Reusing a tools image from any producer later would require retaining and
independently verifying its immutable identity; it is not currently an available
input and this workflow does not invent one.

## Shared producer preparation

`--resume-checkpoint PATH SHA256` (one to four occurrences) selects the separate
ARM native-payload producer. It excludes both import modes. The shared controller
performs existing cache, WPE and source-input preparation, then stores inputs and
environment in mode0600 private files and invokes the adjacent reviewed producer.
It returns after that producer, without legacy export or another build call.
Producer workflow/launcher pins controller helpers independently from source.

These changes enable consumption once a complete reconstruction exists; they do
not prove that reconstruction, raw assembly, finalization or boot has succeeded.
