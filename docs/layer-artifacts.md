# Downloaded development layers

`tilo-vm swap-artifact` consumes an upstream native CI job's downloaded layer
streams. It never builds, invokes Cargo metadata, selects a builder, pulls a CI
image, or falls back to an installed layer. Acceptance runs it from the exact
source checkout that produced the artifacts.

```sh
tilo-vm --json swap-artifact apps+runtime --slot acceptance \
  --receipt /tmp/layers/manifest.json \
  --source "$SOURCE_COMMIT" --builder-image "$BUILDER_IMAGE" \
  --run-id "$UPSTREAM_RUN_ID" --run-attempt "$UPSTREAM_RUN_ATTEMPT" \
  --receipt-sha256 "$UPSTREAM_MANIFEST_SHA256"
```

The expected source, immutable builder, workflow API run ID, attempt and manifest
digest come from the authenticated upstream job. Computing the expected manifest
digest from the downloaded file itself does not establish its identity. Transfer
uses the existing same-run CI artifact service; the CLI adds no keys, grants,
publisher authorities or transport.

The receipt schema is `tilo.vm.layer-artifacts/v1`. Unknown fields are rejected.
An example with placeholders follows; a complete cohort has one layer entry each
for `launcher`, `install`, `shell` and `apps+runtime`:

```json
{
  "schema": "tilo.vm.layer-artifacts/v1",
  "repository": "tilo/tilo",
  "source_commit": "<40 lowercase hex>",
  "builder_image": "registry.tilo.so/tilo/tilo-builder@sha256:<64 lowercase hex>",
  "run_id": 123456,
  "run_attempt": 1,
  "architecture": "x86_64",
  "layers": [{
    "component": "shell",
    "profile": "release",
    "features": ["shell/dev-overrides"],
    "target": "x86_64-unknown-linux-gnu",
    "toolchain": "rustc <exact producing version>",
    "source_digest": "<inventory SHA256>",
    "layer_hash": "<canonical content hash from layer header>",
    "stream": {"path": "shell.stream", "sha256": "<framed file SHA256>", "bytes": 123},
    "inventory": {"path": "shell.inputs.json", "sha256": "<inventory SHA256>", "bytes": 456}
  }]
}
```

Each inventory is the original `.tilo-vm-inputs-*.json` verified before and after
the maintained assembler runs. Its provenance must identify the same clean source
and component. `source_digest` equals the inventory file's SHA256. The stream is
the ordinary full `tilo-vm layer-pack` output, with no cached-layer offers. Its
SHA256 and byte count cover the entire framed file; `layer_hash` identifies
canonical installed content, not the tar archive. Paths are relative to the
manifest directory. Missing, escaping, substituted, cache-only or incomplete
files are errors. Both ordinary text and binary framing are supported.

The CLI verifies every receipt, inventory, file digest, header and stream footer,
retaining private copies before any daemon action. The `apps+runtime` layer must
have the `thin-app` profile and all four matching prerequisite components.
Import uses normal authenticated upload, `LayerImport` and `LayerVerify`, checking
both content and recorded provenance. Layers remain local and non-promotable.

For a cohort, activation uses normal `Swap` for launcher, install, shell, then
apps+runtime. The daemon's existing SDK and TAPP signature, trusted-publisher,
runtime, architecture and native-closure verification stays authoritative.
After each activation the CLI reads the addressed slot and checks its installed
component hash. An import, verification or activation failure stops the route;
the receipt retains the activations already completed. This is not a transaction
that rolls back earlier prerequisites automatically.

Infra owns the upstream build/sign/manifest writer, same-run artifact transfer,
and the acceptance harness's opt-in invocation. The CLI owns verification and
normal import/apply. Integration owns composition and acceptance dispatch. The
harness must retain its original component receipts, slot readback and P01,
W19 and W20 assertions.
