# Offline developer inference qualification

The official runtime's locally saved OCI archive has no published HTTPS URL
for those archive bytes. A registry manifest URL describes different bytes.
Use an explicit offline developer receipt rather than invent a release URL.
This route exercises the existing local-model service and OCI supervisor; it
does not publish a model, enroll a production trust key or replace the gateway.

## Separate signed channel

The existing catalogue compiler accepts `--developer-offline` only with a
source declaring `"production": false`. It signs the same typed payload under
the distinct domain `tilo.local-model.developer-offline.v1`. Ordinary
`VerifiedCatalog::load` refuses that domain even when its key is trusted.
Production signing bytes and the production HTTPS requirement are unchanged.

A developer artifact can declare the explicit locator
`offline:sha256:<64 lowercase hex>`. The compiler recomputes the local file's
size and digest; the locator must equal that digest. Unknown offline schemes
and mismatched identities fail. The model can retain its actual pinned HTTPS
origin; this offline transport consumes only the digest-indexed local copy.
Both locations remain inside the signed payload. The runtime's separately
signed OCI image identity records the real immutable registry origin.

For a prepared source and a private, developer-only signing seed:

```sh
cargo run --locked -p local-model-service --bin tilo-model-catalog -- \
  --developer-offline --source /private/candidate/source.json \
  --signing-key /private/candidate/developer-seed \
  --catalog-out /private/candidate/catalog.json \
  --trust-out /private/candidate/keys.json
```

These are command shapes, not an executed model deployment. Preserve actual
artifact origin/digest/size and template inventory receipts with the source.
The existing extension manifest also requires a genuine base-image target
digest; do not invent a desktop base identity or imply that a development
host has passed that target's OS compatibility check.

The observed `registry.tilo.so/tilo/tilo-vm` development-image candidate resolves
to digest `sha256:9b73b09f7f676c81ceb4a0f718c31cc065848b1bb746fceaa0c31a868d4d7d08`
(amd64; source label `12f4dfb2d535f0ea9c64278b66a9bef435d2e4b3`).
That is a genuine possible extension *target*, not the identity of the current
host. Running the developer candidate on this host does not establish target
compatibility or authorize image deployment. Production extension policy is
unchanged.

## Direct-mode containment

The manager selects this channel only with all four explicit overrides:
`--root`, `--catalog`, `--keys`, and `--artifact-dir`. It rejects root,
kernel `NoNewPrivs`, a launcher model-service FD, an app marker, the daemon
`serve` command and accelerator provisioning. RPC clients cannot provide these
overrides. This is an unconfined developer route, not a GUI capability.
The existing durable install/start workers carry the same explicit inputs
and validate the restriction again. Normal installation retains HTTPS.

Place regular files under the selected artifact directory using only their
signed SHA-256 as the filename. An existing verified candidate can be copied
or hard-linked there; a symlink is refused. Linux `openat2` refuses symlinks
and magic links in every path component. The transport requires the signed
size and supported resume offset, has no network fallback and writes only to
the service's already-open partial object. The existing service then checks
the entire digest before atomic promotion, import or launch.

```sh
tilo-model-manager --root /private/candidate/store \
  --catalog /private/candidate/catalog.json --keys /private/candidate/keys.json \
  --artifact-dir /private/candidate/artifacts \
  install --model SIGNED_MODEL_ID --accept-license
```

Snapshot/start/stop use the same explicit overrides. An install command can
enqueue a worker successfully and still fail later; retain its actual service
state and worker outcome. Kernel confinement and path policy remain in force.

## Acceptance evidence

Regression tests exercise the production compiler/loader and service transfer
path: default-domain refusal, signed metadata tampering, incomplete/confined
CLI context, resume, mismatched identity, corrupted bytes, wrong size and
symlinks. They use explicitly synthetic test bytes and establish the transport
contract; they establish no model loading or coding quality.

For the real candidate, record actual catalogue/key identities, source/argv,
object digests, service state, runtime import and process outcome. A TCP-ready
state is insufficient: require healthy loading, exact `/v1/models` alias and
an observed gateway tool round trip, then a calibrated coding task. Preserve
effective thinking policy and reasoning replay separately from final output.
The normal launcher-issued gateway and scoped Tilo actions remain the agent
execution surface. See [local-inference.md](local-inference.md) and
[evaluation.md](evaluation.md) for the remaining model-run evidence.
