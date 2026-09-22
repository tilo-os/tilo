# Real app publication receipts and pinned channel consumption

`tools/apps/app_channel_manifest.py` supplies the schema/adapter and reader for the
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
- Every tapp file's SHA256, app/version, signed-root package ID, publisher and
  signed architecture ISA declaration. The BLAKE3 package ID covers the complete
  signed root, including its Ed25519 signature and manifest/payload hashes.
- Runtime version, BLAKE3 root, publisher and verified Ed25519 signature.
- The hash-pinned verifier implementation and its identity formats.
- Publication state, URL, and an independently trusted upload receipt digest.

`create-receipt` first pins the archive and verifier file hashes, streams the tar
against the supplied local collection tree, rejects links/special paths, checks
full SHA256SUMS coverage and package inventory, then invokes the verifier's
`verify_tapp`/`verify_runtime`. The verifier is **trusted host code that executes**;
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
and collection index; the historical tapp signature is not a Git-source attestation.
Publisher allowlisting here is not a complete revocation policy or installation
authorization; the production package trust boundary remains required.

## Commands

```sh
python tools/apps/app_channel_manifest.py --trust-policy trusted-policy.json select \
  --manifest app-channel.json --manifest-sha256 TRUSTED_MANIFEST_SHA256 \
  --resolved-commit RESOLVED_CHANNEL_COMMIT --architecture x86_64
python tools/apps/app_channel_manifest.py --trust-policy trusted-policy.json create-receipt \
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

`release-apps.yml` invokes `tools/apps/create_app_receipt.py` after packaging, including
manual `publish=false` runs. It calls `tilo-app-package verify` with explicit trusted keys
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

The workflow consumes the platform-owned immutable verifier bundle described below.
It rejects mismatched independently pinned hashes before execution,
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

`tools/apps/final_app_receipt.py verify` performs final canonical verification;
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

## Platform-owned reusable verifier bundle

Final qualification now downloads a platform verifier bundle instead of transporting
binaries from every app build. App changes do not change these verifier hashes.
The existing independently pinned final-receipt policy supplies
`canonical_verifier_bundles`, keyed by native execution architecture. Each descriptor
contains the immutable registry URL, archive SHA256/size, binary SHA256 pair, verifier
source SHA and compatible immutable builder image. The existing publisher keys,
allowed app ISA and Python adapter pin remain part of the same policy. There is no
new per-PR input, service or author-agent instruction.

Platform tooling maintenance can use a manual native producer or bundle
already-built tools.

The manual-only `canonical-verifier-bundle.yml` workflow now prepares those tools
on native x86-64 and ARM64 workers. Platform maintenance dispatches it at a reviewed
verifier source revision, optionally supplying a qualified immutable builder index.
The workflow resolves one index for both lanes, checks the checkout SHA and native
architecture, restores shared Cargo downloads, and builds only `tilo-app-package-cli` and
`runtime-bundle` with the committed lockfile. The optional compiler cache affects
speed, not correctness. No app fleet or OS image is built.

Each lane checks that both CLIs start with `--help`, produces the bundle twice and
requires byte-identical archives/descriptors. The artifact
`canonical-verifiers-ARCH-SOURCE_SHA` retains only `ARCH.tar.gz` and
`ARCH.tar.descriptor.json` for 14 days. These are unpublished platform candidates:
the descriptor URL is the intended eventual content-addressed location, not an
upload receipt. Archive determinism for fixed binaries does not establish compiler
reproducibility; `--help` establishes native loading, not package/signature semantics.
Independent canonical verification of representative app/runtime artifacts on both
lanes remains required before the platform publisher uploads and approves policy.

This workflow has no push/PR/schedule trigger, package upload, signing credential or
trust-policy promotion. It reuses the existing registry pull/resolution credential;
native build steps receive no new secret. Run it once per reviewed verifier version
or builder-compatibility change, preserve accepted artifacts before expiry, and let
the platform owner publish/promote the descriptors centrally. Application authors
do not supply per-build verifier hashes. The subsequent
[x86-64 producer acceptance](results/native-verifier-23012/README.md) records a
successful native build/startup and independent artifact verification. ARM remains
pending; publication, policy promotion and representative signed-artifact replay
remain separate gates.

To package already-built native tools manually:

```sh
python3 tools/canonical_verifier_bundle.py produce \
  --pkg-binary /trusted-tools/tilo-app-package \
  --runtime-binary /trusted-tools/tilo-runtime-bundle \
  --builder-image code.sandbox.example/qos/tilo-builder@sha256:BUILDER_DIGEST \
  --source-sha VERIFIED_TOOL_SOURCE --architecture x86_64 --output verifier.tar.gz
```

This creates a deterministic archive and `verifier.tar.descriptor.json`. The existing
platform package publisher can upload it at the descriptor's content-addressed
`tilo-canonical-verifiers/SHA/ARCH.tar.gz` URL and promote the generated descriptor
into its centrally approved policy. The producer itself does not upload or confer
trust on its source/build metadata. Its invoking trusted build must bind those
inputs to the actual compilation. Repeat for native ARM64. Only verifier changes
or builder compatibility changes require a new platform bundle, not app changes.

Both qualification and final publication consumers fetch with the dedicated
`TILO_VERIFIER_BUNDLE_READ_TOKEN` Actions secret: existing service identity
`tilo-packaging-controller`, token 153, scoped only to `read:package`. Its encrypted
source is the controller registry SOPS secret; provisioning matches the decrypted
credential against the live token ID/name/scope metadata in memory. No registry
write credential or fallback is exposed to these steps. Historical Actions artifact
reads retain `github.token`; the package secret is not used for artifact APIs.

Forgejo `16.0.3+gitea-1.22.0` rejects this private organization's generic package
fetch with the built-in job token (qos/qos run 23176, UI run 9680: HTTP 401).
This is not a Bearer-versus-Basic mismatch: [package routes accept both](https://codeberg.org/forgejo/forgejo/src/tag/v16.0.3/routers/api/packages/api.go),
but [package permissions](https://codeberg.org/forgejo/forgejo/src/tag/v16.0.3/services/packages/perm.go)
do not yet implement ActionUser access from the task's repository. All supported
job-token schemes resolve the same synthetic identity, which is not a private org
member. Re-qualify built-in package read before removing the dedicated secret after
a Forgejo upgrade; do not widen token scope to work around an authentication error.

The consumer rejects redirects and mutable URLs, checks archive size/digest before
bounded decompression, validates the exact
four-member regular-file inventory, manifest, ELF architecture and binary hashes.
It executes only after the existing adapter pin and binary preflight succeed.
Qualification requires an exact compatible builder digest; a changed builder fails
explicitly until platform tooling is qualified there. This conservative constraint
avoids guessing dynamic-library compatibility across images.

Receipts retain the actual binary hashes and additionally identify their approved
bundle digest/native execution architecture. The manifest reader validates these
against central bundle descriptors. Legacy explicit-binary policies remain readable,
but the workflow's final qualification now requires a bundle descriptor; it does not
fall back to app-built tools. Selected-app build receipts still use the tools already
built during packaging and remain build-observed evidence only.

Qualification remains disabled. The x86-64 bundle's durable package readback is
recorded in `results/native-verifier-23012/registry-readback.json`; this credential
change does not change trust policy or qualify full app/runtime acceptance.
Production activation still needs one centrally released bundle per
native architecture and independent verification of its actual package/runtime CLI
compatibility. The coordinator's trusted helper closure must include
`tools/canonical_verifier_bundle.py` before accepting this workflow revision.

The bundle includes its frozen `canonical_app_verifier.py`; final verification loads
that digest-checked copy rather than the candidate checkout's adapter. Qualification
and publication helper commands run under the platform's isolated
`python3 -I tools/ci/python.py tools/SCRIPT.py` launcher, with stdlib imports ahead of
reviewed helper paths and no inherited PYTHONPATH. This commit depends on that
launcher being integrated; it does not supply a second launcher implementation.

## Trusted final-artifact acceptance callback

`tools/accept_app_artifacts.py` provides `FinalArtifactAcceptance` for explicit
injection as `ProductForgejo.verifier`. It is not enabled by the coordinator CLI.
The platform must run this installed code, the API client and the independently
pinned verifier policy outside candidate checkouts under its isolated interpreter.
Neither artifact metadata nor candidate paths can select code, policy or commands.
The central coordinator policy must bind `final_receipt_policy_sha256` to the
independently supplied verifier policy. This binding is rechecked after replay;
rotating it changes the desired coordinator policy identity and prevents baseline
reuse across the change. The existing scheduler still owns execution slots; this
adds no service.

Acceptance checks the full immutable dispatch inputs and trusted helper policy,
then identifies the current successful `qualify-collection` job by run/repository,
job ID, attempt, task ID and handle. The trusted assembly helper emits
`TILO_COORDINATOR_FINAL INTENT ATTEMPT ARCH RECEIPT_SHA ARCHIVE_SHA BYTES` only after
canonical final verification. The callback requires exactly one matching marker
for every requested architecture from that current job's authenticated log API.
An artifact's own provenance document cannot authenticate its receipt or source.

Each exact run-bound artifact ID is fetched with the existing read-only transport.
Its receipt and archive bytes must match the job marker. The callback extracts and
replays the complete canonical verification using the platform's immutable verifier
bundle. It compares source, collection, runtime and all package identities against
the emitted receipt; a different approved native verifier architecture is allowed
for replay, since verification does not execute app payloads. After replay it reads
the trusted policy, run, complete jobs list and artifact list again. Any changed
attempt/task/handle, dispatch, artifact identity or policy prevents acceptance.
The returned existing `tilo.coordinator-verification/v1` proof retains per-architecture
receipt hashes plus the qualifying job/task/handle and numeric artifact IDs.

Runtime qualification remains outstanding: no real final-collection job has been
executed for this callback, no callback is installed, and no baseline was advanced.
The job API's attempt and `GITHUB_RUN_ATTEMPT` correspondence, current-job log routing,
log timestamp format and task-token historical artifact access require a bounded
live qualification before activation. A mismatch fails closed. Canonical binaries
must run in the platform's qualified native environment compatible with the pinned
builder; unit tests mock cryptographic execution and do not establish that runtime
compatibility. Transport uses existing per-request timeouts and byte limits, not a
new global scheduler deadline or admission mechanism. The caller must keep replay
outside the coordinator's short state transaction and retain its existing final
intent/run recheck before committing the proof.
