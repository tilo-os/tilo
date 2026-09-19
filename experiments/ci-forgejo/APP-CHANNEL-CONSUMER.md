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

## Final merged archive qualification (opt-in)

Manual publishing supports `verify_final_receipt=true` (default false). Before
using it, provision `TILO_APP_FINAL_RECEIPT_POLICY` and its independently reviewed
exact-byte SHA256 in `TILO_APP_FINAL_RECEIPT_POLICY_SHA256`. This policy must pin
the reviewed adapter and native executable hashes, approved app/runtime publisher
keys, and allowed ISA labels. Do not copy trust automatically from build artifacts.
The hashes must refer to the first requested architecture's verifier executables;
that native verifier can cryptographically verify both target architectures.
The publisher uses the same immutable builder resolved by the plan job.

The workflow transports only the already-built canonical binaries in dedicated
artifacts. It rejects mismatched independently pinned hashes before execution,
checks the Python adapter hash before importing it, then verifies the complete
newly assembled collection, including reused packages. Verifier subprocesses receive
only PATH and locale variables, excluding publisher tokens and loader overrides.
It never treats the selected-app build receipt as the merged collection receipt.
After immutable archive upload it downloads that exact URL and requires matching
size/SHA256 before producing a published receipt plus upload observation. These
JSON files are uploaded alongside the collection using architecture-specific names.
An enabled qualification failure is fatal; there is no fallback to unverified mode.
The readback establishes availability at that moment, not permanent retention or
protection against a registry administrator replacing bytes later.

`tools/final_app_receipt.py verify` performs final canonical verification;
`bind-readback` binds its pinned receipt to the caller's authenticated download of
the exact URL. The latter is a trusted uploader operation: merely supplying an
arbitrary local file and URL cannot independently establish network provenance.
Its resulting receipt is accepted by the existing immutable manifest reader using
the same independently supplied policy, with exact architecture selection.
No channel manifest is written, no OS consumer is switched, and continuous main
packaging remains paused. Enabling production continuous promotion must require
this final-archive verification and independently pinned policy; the optional
qualification flag is not a production trust bypass for that future channel.

The collection source identifies assembly; reused package identities retain their
signed package IDs and digests. Historical package signatures are not per-app Git
source attestations. Versioned tag URLs without a full source/content identity are
intentionally refused by binding; this first opt-in qualification uses manual main
publication's full-SHA package version.

## Nonpublishing coordinator collection artifacts

The guarded coordinator path now has `qualify-collection`, after native builds.
It invokes `tools/assemble_verified_apps.py`, reuses already-built canonical tools,
and never compiles or publishes packages. It produces one immutable workflow
artifact named `final-app-collection-ARCH`, containing only the complete tarball,
`final-publication-receipt.json` and `assembly-provenance.json`. The receipt remains
unpublished. The assembly record reports source/builder/intent and reused package
names; these self-reported fields are evidence to compare, not independent trust.

For an incremental run, the coordinator supplies its retained independently
verified baseline proof and canonical JSON hash. The helper checks its exact
source, builder, collection and policy scope; reads that exact successful dispatch
run; resolves one unexpired architecture artifact and its numeric ID; and requires
the downloaded receipt's hash to match the coordinator proof. The tarball must
match that pinned receipt before bounded extraction. Traversal, links, sparse files,
duplicate paths and excessive entries/expanded bytes are rejected. Only exact
current catalog members are reused; partial reuse requires the same runtime root.
Final canonical verification covers the complete newly assembled archive.

The job uses the existing repository task token for GET requests only. There is no
publisher OIDC credential, new service, credential fallback or registry write.
API redirects are refused so credentials cannot escape the configured origin.
Actual read-only API inspection confirmed Forgejo `commit_sha`, bare artifact lists,
and artifact `run_id` fields. Historical artifact permission for the built-in task
token remains unqualified: unauthorized/expired/missing artifacts fail explicitly.

Remaining integration requirements are centralized in the disabled coordinator,
not instructions for each author agent: independently verify the run/source/builder
against its dispatch intent, download and canonically replay the resulting artifact,
then supply the cached verification proof to its short state transaction. Workflow
success alone does not advance the baseline. The existing independently reviewed
final receipt policy/tool hashes must be provisioned before qualification. No live
dispatch or publication was performed for this change. If selection yields zero
builds, this stage produces no proof and cannot advance the baseline; a deliberate
no-op identity policy is still needed instead of treating a skipped job as verified.
The final artifact retention period also bounds incremental reuse; expiration
requires an explicit full build rather than silently trusting mutable channels.
