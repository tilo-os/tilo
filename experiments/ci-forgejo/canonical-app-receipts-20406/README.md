# Canonical verification of retained artifact 34748

This is actual native ARM64 execution of the retained Rust `tilo-app-package` and
`tilo-runtime-bundle` tools against the historical x86_64 collection from run
20406, source `525aa7824c7a8476aa5ea006499ddd01a8b3c2ea`. Verification does not
execute application binaries or establish native application compatibility.

All 51 packages passed canonical signature verification with an explicit trusted
publisher, and the runtime passed required publisher/signature/x86_64 checks.
The nonconsumable `canonical-audit.json` still rejects contacts, notes and store:
their historical signed manifests lack the required ISA declaration. No fallback
or signature rewrite was used. Main's subsequent ISA fix is not qualified by this
historical archive; a fresh native packaging run remains necessary.

`historical-canonical-policy.json` records this experiment's explicitly selected
historical publishers and verified tool hashes. It is an experiment input, not a
production trust root. The archive SHA256 is
`4cd9ab079136b8bcc97fa9000ee92485911a2182b870eab2411586fb6f1d1e84`.

Tool provenance is recorded in `tooling-provenance.json`. Only the two necessary
OCI checkpoint layers were fetched (162,017,970 compressed bytes), checked against
the immutable descriptor digests and sizes, and the exact regular binary entries
were extracted. Binary SHA256 values matched the retained native reconstruction
inventory. Compressed temporary layers were removed; no image pull, compilation,
new runner or publication was performed. These are development reconstruction
tools, not independently attested production tooling.

The real audit used WSL Fedora Linux 42 system Python (standard library only),
`app_channel_manifest.create_receipt(..., audit_only=True, canonical_binaries=...)`
and `tools/apps/canonical_app_verifier.py`. Both actual canonical commands were invoked:

```sh
tilo-app-package verify --trusted-key EXPLICIT_APP_KEY package.tapp
tilo-app-package inspect package.tapp
tilo-runtime-bundle verify runtime-directory --require-publisher EXPLICIT_RUNTIME_KEY --require-signature --require-arch x86_64
```

Unit tests separately exercise canonical failure/protocol handling, executable
replacement, archive ambiguity, policy binding, and actual staged receipt writing.
Their subprocess mocks are not the evidence for the real signature verification
above. No current full packaging run, immutable remote upload, production channel
or production consumer is qualified by this audit.
