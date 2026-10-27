# Downloaded development layers

`tilo device swap-artifact` consumes an upstream native CI job's downloaded layer
streams. It never builds, invokes Cargo metadata, selects a builder, pulls a CI
image, or falls back to an installed layer. Acceptance runs it from the exact
source checkout that produced the artifacts.

Before it reads a byte of the cohort it checks that checkout: the commit HEAD
names (`git rev-parse --verify HEAD`, as build provenance and the harness read
it) must equal `--source`, or it refuses and names the checkout, the commit it
found and the one requested. Resolving HEAD does not open the commit object, so
an unprivileged user's worktree of a root-owned CI clone, whose shared object
store it may not read, still answers; when HEAD cannot be read at all the
refusal carries git's own words.

The CLI that runs it need not be the source's own. t's vm-acceptance-cohort
builds the consumer from main's head (vm-cohort-verifier-x86_64) and runs it in
the checkout of the commit under test: the code under test does not verify its
own artifacts, and a branch that predates a consumer fix can still be tested.
The commit's own tilo-vm remains the packer (vm-cohort-x86_64,
tools/ci/vm-cohort.sh).

```sh
tilo-vm --json swap-artifact apps+runtime --slot acceptance \
  --receipt /tmp/layers/manifest.json \
  --source "$SOURCE_COMMIT" --builder-image "$BUILDER_IMAGE" \
  --run-id "$UPSTREAM_RUN_ID" --run-attempt "$UPSTREAM_RUN_ATTEMPT" \
  --receipt-sha256 "$UPSTREAM_MANIFEST_SHA256"
```

The expected source, immutable builder, run ID, attempt and manifest digest come
from the upstream job: on t, the producer's `handoff.json`, which t hands with the
layers only to the checks that need that producer's result (t numbers no runs, so
the producer names its handoff with a random run ID). Computing the expected
manifest digest from the downloaded file itself does not establish its identity.
Transfer is t's outputs of a need; the CLI adds no keys, grants, publisher
authorities or transport.

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
