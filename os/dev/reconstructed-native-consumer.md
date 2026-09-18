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

The manual `development-compile.yml` workflow exposes `source_revision` and the
three reconstruction inputs. The workflow checkout is the controller commit;
the optional source is a separate exact-commit checkout. It is copied separately
and never patched with controller changes. Public export includes
`controller-source.json` and `reconstructed-native-adoption.json`.

## Shared producer preparation

`--resume-checkpoint PATH SHA256` (one to four occurrences) selects the separate
ARM native-payload producer. It excludes both import modes. The shared controller
performs existing cache, WPE and source-input preparation, then stores inputs and
environment in mode0600 private files and invokes the adjacent reviewed producer.
It returns after that producer, without legacy export or another build call.
Producer workflow/launcher pins controller helpers independently from source.

These changes enable consumption once a complete reconstruction exists; they do
not prove that reconstruction, raw assembly, finalization or boot has succeeded.
