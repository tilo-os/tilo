# Real app publication receipts and pinned channel consumption

`tools/app_channel_manifest.py` supplies the schema/adapter and reader for the
production transition. The release build now generates and uploads unpublished
receipts using its already-built canonical Rust verifiers. **Channel consumption
and promotion remain inactive; no channel is written.**
The private atomic-promotion CLI remains unchanged and restricted to its fixture.

## Existing consumers and concrete migration

`.forgejo/workflows/build-image.yml`, step **Materialize the pinned app collection**,
currently reads `TILO_APP_COLLECTION_URL[_X86_64|_AARCH64]` and the corresponding
SHA256 variables independently, then downloads/extracts the collection. The
`release-apps.yml` publisher also reads these pairs as `PRIOR_X86_*`/`PRIOR_ARM64_*`
for artifact reuse, and updates six variables independently after publication.

The next wiring change should:

1. Resolve the protected channel ref once through authenticated Git, retaining
   that full commit. Read its manifest from **that commit**, never reread the
   moving ref for another architecture or digest. Derive the expected manifest
   SHA256 from this trusted fetch and retain it with the build's inputs.
2. Invoke `select` with those two pins, the actual native architecture and a
   separately provisioned trust/ISA policy. Parse its single JSON result for URL,
   archive digest/size, architecture-specific source and runtime identity.
3. Download the selected collection, verify its pinned digest, and retain existing
   package/runtime verification before use. Record the channel commit, manifest
   digest and entry source in `os/dev/release-provenance.sh`, alongside the archive
   digest already recorded there.
4. Use the same reader for the release publisher's previous-collection lookup.
   The publication coordinator's last successful baseline remains independent of
   the last push. Replace the variable writes only after the real publisher emits
   trusted upload receipts and atomic manifest promotion is production-qualified.

An explicit manual archive override must remain an explicit complete URL/digest/
trust input set. If a new channel input is supplied but invalid, fail; do not fall
back silently to old variables or another architecture. Keep the migration opt-in
until actual workflows and protected-ref credentials are qualified. No automatic
OS release or macOS enablement is part of this change.

## Receipt contract and trust boundaries

A `tilo.app-publication/v1` receipt binds one architecture and source SHA to:

- Collection format, complete archive SHA256 and size.
- Every qpkg file's SHA256, app/version, signed-root package ID, publisher and
  signed architecture ISA declaration. The BLAKE3 package ID covers the complete
  signed root, including its Ed25519 signature and manifest/payload hashes.
- Runtime version, BLAKE3 root, publisher and verified Ed25519 signature.
- The hash-pinned verifier implementation and its identity formats.
- Publication state, URL, and an independently trusted upload receipt digest.

`create-receipt` first pins the archive and verifier file hashes, streams the tar
against the supplied local collection tree, rejects links/special paths, checks
full SHA256SUMS coverage and package inventory, then invokes the verifier's
`verify_qpkg`/`verify_runtime`. The verifier is **trusted host code that executes**;
its expected hash must come from reviewed configuration, never the downloaded
receipt/archive. The reader does not add crypto dependencies to normal CI.

The adapter requires separately configured app/runtime publisher keys, approved
verifier hash and allowed signed ISA labels per architecture. It does not infer
an ISA from a filename. This is declared compatibility, not native execution or
proof of generated CPU instructions. Current Rust installer semantics say a
missing host-architecture ISA means no build is available.

By default the result is `unpublished`. To mark it published, supply a trusted
uploader's `tilo.app-upload/v1` JSON with `source_sha`, `architecture`, `sha256`,
`bytes`, `url`, plus its independently supplied expected SHA256. All identities
must match the verified collection. The URL must use the expected package origin
and pin a full source/content identity in its version path. This checks naming,
**not remote immutability or availability**: the uploader must independently
confirm stored bytes and retention before issuing its receipt.

A `tilo.app-channel/v1` manifest contains positive `generation`, top-level
`source_sha`, and `architectures` mapping to these complete receipts. Partial
updates keep each architecture's own source/runtime identity. `select` uses the
requested architecture exactly, never an x86/ARM fallback. It rejects unpublished
receipts, audit records, incompatible ISA, unapproved keys/verifier, duplicate JSON
keys and digest mismatches. `inspect-candidate` permits valid unpublished receipts
for review but returns `usable_for_download: false`.

**The reader trusts the externally pinned manifest; it does not recheck signatures
without artifact bytes.** Its `resolved_commit` is supplied by the trusted caller,
not independently bound to the bytes inside this standalone reader. The workflow
resolver must obtain both commit and manifest digest from the same authenticated
Git snapshot. Source attribution comes from the trusted build/publication receipt
and collection index; the historical qpkg signature is not a Git-source attestation.
Publisher allowlisting here is not a complete revocation policy or installation
authorization; the production package trust boundary remains required.

## Commands

```sh
python tools/app_channel_manifest.py --trust-policy trusted-policy.json select \
  --manifest app-channel.json --manifest-sha256 TRUSTED_MANIFEST_SHA256 \
  --resolved-commit RESOLVED_CHANNEL_COMMIT --architecture x86_64
python tools/app_channel_manifest.py --trust-policy trusted-policy.json create-receipt \
  --archive tilo-apps-x86_64.tar.gz --tree verified-collection \
  --archive-sha256 TRUSTED_ARCHIVE_SHA256 --source-sha SOURCE_COMMIT \
  --architecture x86_64 --verifier reviewed-verifier.py \
  --verifier-sha256 TRUSTED_VERIFIER_SHA256 --output candidate-receipt.json
python -m unittest discover -s tools/tests -p test_app_channel_manifest.py -v
```

All nine tests use the standard library. Receipt creation additionally requires
the explicitly selected verifier's dependencies; the retained historical replay
uses its isolated WSL venv with `blake3` and `cryptography`. No verifier is downloaded,
no global package is installed, and no publication is performed by this tool.

## Staged canonical build receipts

`release-apps.yml` invokes `tools/create_app_receipt.py` after packaging, including
manual `publish=false` runs. It calls `tilo-pkg verify` with explicit trusted keys
and `tilo-runtime-bundle verify` with required publisher, signature and architecture.
The Python adapter uses the standard library; production receipt generation does
not depend on the experimental Python crypto implementation. Signer public keys
are captured from the signing invocation, not inferred from downloaded packages.
Canonical executable hashes and the adapter hash are recorded and checked.

Only `publication-receipt.json` and `verification-policy.json` are added to public
artifact uploads; the work directory and private signing identities are excluded.
The generated policy and executable hashes are **build-observed metadata, not
independently trusted provenance**. Consumers must use separately reviewed policy
and must never automatically adopt the uploaded policy as a trust root.

These receipts cover the build archive, which can contain only selected apps.
The publisher later merges prior artifacts and assembles a different collection:
its final archive still needs canonical verification and a fresh receipt before
trusted upload confirmation and channel promotion. This change does not qualify
that final publication path or enable continuous publishing. The next production
slice should retain canonical tools/public signer inputs through assembly, verify
the merged archive, and bind its immutable upload receipt before promotion.

See [retained artifact evidence](canonical-app-receipts-20406/README.md) for actual
canonical verification without recompilation.
