# qOS supply-chain security (operations)

Status of security-plan §9 (signing / reproducibility / transparency) and how to
operate it. Companion to `docs/superpowers/specs/2026-06-30-qos-security-architecture-plan.md`.

## What's in place

- **Image signing.** CI signs every promoted image by digest with cosign
  (`--tlog-upload=false`, own key). The tracked `keys/cosign.pub` is the
  verifier; signing requires `COSIGN_KEY_REF` to name a non-exportable
  KMS/HSM key. Exportable private-key material is rejected by the workflow.
- **Verify-before-update.** The image ships `/usr/share/qos/cosign.pub` + an
  `/etc/containers/policy.json` that **requires** a valid signature for
  `code.sandbox.example/qos/qos` (+ `registries.d` sigstore-attachment lookup). So
  `bootc upgrade` refuses an unsigned/tampered OS image.
- **SBOM.** CI generates an SPDX SBOM (syft) and attaches it as a **signed**
  in-toto attestation to the image (`cosign attest`). Audit with
  `cosign verify-attestation --key keys/cosign.pub --type spdxjson <image>`.
- **Reproducible builds (release-blocking).** Every release is built twice. The
  second build bypasses the first build's layer cache, and the workflow refuses
  publication unless both OCI manifests are byte-identical.
  Achieved by: base pinned by **digest** against our mirror (below), Rust resolve pinned
  (the tracked `Cargo.lock`), a deterministic compile (`CARGO_INCREMENTAL=0`,
  `--remap-path-prefix=/build=.`, `LC_ALL=C.UTF-8`, `TZ=UTC`), and a
  `SOURCE_DATE_EPOCH` derived from the HEAD commit time with layer/image
  timestamps clamped to it (`podman --source-date-epoch --rewrite-timestamp`;
  buildkit honours `SOURCE_DATE_EPOCH` + `provenance: false` in CI).
  Reproduce: `SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) ./os/dev/build-image.sh`
  twice → identical `podman image inspect --format '{{.Id}}'`.
- **Cross-time reproducibility (frozen RPM repo).** Package versions are frozen
  too, so a rebuild *weeks later* is still byte-identical. The exact resolved RPM
  set is published as a digest-pinned OCI image (`os/Containerfile.rpms`, built +
  pushed by the `snapshot-rpms` CI job to `code.sandbox.example/qos/rpms`), pinned in
  `os/rpms-snapshot.env` (`RPMS_REF=…@sha256`). Every `os/Containerfile` `dnf
  install` mounts that repo and installs from it only
  (`--setopt=reposdir=/repo`), never live Fedora mirrors.
  **When you add/remove/rename a package in `os/Containerfile`,** re-run the
  `snapshot-rpms` workflow (`workflow_dispatch`); it prints the new `RPMS_REF` —
  commit that into `os/rpms-snapshot.env`. Otherwise the build fails
  `No match for argument: <pkg>`. The package list is auto-derived from
  `os/Containerfile`, so there is no separate list to maintain.
- **Cross-time reproducibility (mirrored base image).** Pinning the base by
  digest is not enough on its own: Fedora rebuilds the `fedora-bootc` tag daily
  and garbage-collects the manifest it replaces almost immediately, so a
  `FROM quay.io/fedora/fedora-bootc@sha256:…` pin stops resolving after roughly
  a day (`manifest unknown`) and every build fails at its first `FROM` — the
  `snapshot-rpms` job included, which made the package set unrefreshable too.
  So the base is **mirrored into `code.sandbox.example/qos/fedora-bootc`** and every
  Containerfile takes it as `--build-arg BASE_REF`, pinned in
  `os/base-snapshot.env`. Our registry collects nothing, so the pin keeps
  resolving. The copy uses `skopeo copy --all --preserve-digests`, so the
  multi-arch index survives and the mirrored digest is the digest Fedora
  published — the mirror adds durability, not a second artifact to trust.
  **To refresh the base:** edit `BASE_SOURCE` if the Fedora release is
  changing, dispatch `mirror-base`, commit the `BASE_REF` it prints, then
  re-run `snapshot-rpms` and commit the new `RPMS_REF` — the frozen package set
  is resolved against the base and has to follow it. The mirror job refuses to
  report a reference that is not a multi-arch index or whose `ostree.linux`
  does not match the Fedora release `BASE_SOURCE` claims, so a silent major
  bump fails the job instead of shipping.
- **Persistent GitHub input cache.** Every GitHub-hosted tarball, source
  snapshot, catalogue, and license consumed by the OS image is declared once
  in `os/external-assets/github-assets.tsv` with an immutable URL and SHA-256.
  `os/dev/cache-github-assets.sh` performs resumable, retrying downloads and
  refuses a checksum mismatch. Both OS and CI-builder workflows restore that
  directory through Forgejo's cache before materializing missing entries, so
  ephemeral runners normally make no GitHub release-asset requests at all.
  CI and local scripts expose the cache as a separate named build context;
  Containerfiles consume it through read-only mounts. Archives therefore never
  enter layer history or the Rust workspace context, and builds never fetch
  from GitHub. Bump the manifest entry to update an input—the
  manifest hash creates a fresh CI cache key automatically.
- **Staged, verified promotion.** A boot-tested build is first uploaded under a
  non-channel candidate tag. CI signs its digest, attaches signed SPDX and SLSA
  provenance attestations, and verifies all three against the public key. Only
  then does it move `latest`, the version tag, and the dense `gen<N>` tag to
  that digest. Release jobs share one global concurrency lock, preventing two
  jobs from allocating the same generation.
- **Pinned release machinery.** Every external action executed by the OS
  release workflow is referenced by its full commit hash. Moving an upstream
  `v3`/`v5` tag cannot alter the signing job.

## ⚠️ Operational caveat — first signed image

Turning on the `sigstoreSigned` policy means a system can only `bootc upgrade`
to a **signed** image. There is a transition window: any image published
*before* the CI signing step existed is unsigned, so an upgrade to it is
(correctly) refused. **Resolution:** ensure CI has published at least one signed
`:latest` (the signing + attest steps are green) before relying on auto-update on
an installed machine. New installs are fine — they install from the local booted
image, and only *future* upgrades are gated.

## Key rotation / revocation

The signing key is the root of update trust — rotate on exposure or schedule.

1. Create a new non-exportable signing key in the release KMS/HSM and give the
   Forgejo workload identity sign-only access.
2. Update the `COSIGN_KEY_REF` secret with the key URI. No private key or key
   password enters Forgejo.
3. Replace `keys/cosign.pub` (tracked) → the **new** image carries the new
   verifier key. Installed systems pick it up when they upgrade to an image
   signed by the *old* key that already contains the *new* pub (sign one
   transition image with the old key carrying the new pub, then cut over).
4. Re-sign current `:latest` with the new key.
5. **Revocation:** delete the compromised key's signatures from the registry
   (`cosign clean <image>`) and publish a transition image as above. A
   transparency log (below) would make revocation auditable.

## Still missing (§9 follow-ups)

- **Transparency log.** We sign with our own key and do **not** upload to the
  public sigstore/Rekor. A *self-hosted* append-only log of released digests —
  and a measured-boot PCR policy that *requires* the running image to appear in
  it (plan §4/§9) — is not yet stood up. This is what makes "trust qOS"
  publicly verifiable rather than "trust our key."
- **Independent transparency witness.** The registry stores signatures and
  attestations, but a registry operator can still hide a release from a new
  client. A separately administered append-only witness/checkpoint service is
  still required for public split-view and freeze detection.
