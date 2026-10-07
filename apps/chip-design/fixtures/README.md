# Capture fixtures

These reports are imported only by named capture scenarios; live projects start empty.

`checkpoint-verification.json` preserves the reported checks, tool identities,
sample manifest hash, output hashes and exclusions from the locally observed
`real-weight-verification-runs/immutable-first/verification.json` receipt.
Device input and run directories are omitted. This fixture is reported evidence
for a bounded real coefficient sample and synthetic INT8 activations. The app
validates its structure; it does not authenticate or replay its artifacts.
It establishes no full-model quality, chip PPA or manufacturability claim.

`storage-report.json` is an analytical storage assumption fixture; it does not
establish calibrated quantization or measured chip performance.
