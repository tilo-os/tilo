# Tilo supply-chain security (operations)

Status of security-plan §9 (signing / reproducibility / transparency) and how to
operate it. Companion to `docs/superpowers/specs/2026-06-30-tilo-security-architecture-plan.md`.

## What's in place

- **Image signing and public transparency.** CI signs every promoted image by
  digest with cosign and requires the default Rekor publication path for the
  image signature and provenance/freshness attestations. The release invariant rejects both
  `--tlog-upload=false` and `--insecure-ignore-tlog`. The tracked
  `keys/cosign.pub` is the
  verifier; signing requires `COSIGN_KEY_REF` to name a non-exportable
  KMS/HSM key. Exportable private-key material is rejected by the workflow.
- **Verify-before-update.** The image ships `/usr/share/tilo/cosign.pub` + an
  `/etc/containers/policy.json` that **requires** a valid signature for
  `registry.tilo.so/tilo/tilo` (+ `registries.d` sigstore-attachment lookup). So
  `bootc upgrade` refuses an unsigned/tampered OS image.
- **Owner identity endorsements.** `tilo-user-identity::ImageEndorsement` gives
  locally built images a separate human-root → explicitly authorized device →
  immutable OCI digest chain. A short-lived root-signed image-signing grant is
  distinct from device-enrollment authority; its canonical Ed25519 preimage
  includes both manifest SHA-256 and monotonic Tilo generation.
  This does not weaken the release policy above: official-channel updates still
  require their Cosign signature and provenance.

  The installed `tilo-image-trust` tool creates and checks the canonical
  artifacts. Private key files must already satisfy the identity store's strict
  mode checks. Times are Unix-epoch nanoseconds; the verifier deliberately
  requires the expected digest and generation so a caller cannot validate a
  good signature and then accidentally install a different image:

  ```sh
  tilo-image-trust grant ROOT_KEY DEVICE_CERT ISSUED_NS EXPIRES_NS image.grant
  tilo-image-trust endorse DEVICE_KEY DEVICE_CERT image.grant \
    sha256:MANIFEST_DIGEST GENERATION ISSUED_NS image.endorsement
  tilo-image-trust revocations ROOT_PUBLIC_KEY revocations.bin REVOCATION_ENTRY...
  tilo-image-trust verify ROOT_PUBLIC_KEY revocations.bin image.endorsement \
    sha256:MANIFEST_DIGEST GENERATION NOW_NS
  tilo-image-trust verify-and-record ROOT_PUBLIC_KEY revocations.bin \
    image.endorsement sha256:MANIFEST_DIGEST GENERATION NOW_NS \
    generation.state time.state
  ```

  A root or device key held by a TPM, HSM, or signing broker never needs a key
  file. The two-phase commands emit the exact 32-byte, domain-separated digest
  for that capability to sign, then accept a raw 64-byte Ed25519 signature and
  verify the complete artifact before publishing it:

  ```sh
  tilo-image-trust prepare-grant DEVICE_CERT ISSUED_NS EXPIRES_NS \
    unsigned.grant grant.digest
  hardware-signer grant.digest > grant.signature
  tilo-image-trust attach-grant unsigned.grant grant.signature image.grant

  tilo-image-trust prepare-endorsement DEVICE_CERT image.grant \
    sha256:MANIFEST_DIGEST GENERATION ISSUED_NS \
    unsigned.endorsement endorsement.digest
  hardware-signer endorsement.digest > endorsement.signature
  tilo-image-trust attach-endorsement unsigned.endorsement endorsement.signature \
    image.endorsement
  ```

  The attach steps reject already-signed templates, wrong-sized signatures,
  mismatched device grants, expired certificates, and signatures made over any
  altered digest, generation, timestamp, certificate, or authorization.

  Grants should be issued offline; both signer and verifier enforce a maximum
  lifetime of 30 days, so an unattended device can never receive effectively
  permanent image-signing authority. Each individual endorsement also expires
  for verification after seven days, independently of the grant, so a frozen
  registry cannot replay one release for the entire authorization window. The revocation bundle
  may be empty but is mandatory and every entry must chain to the same root.
  Update/install paths should use `verify-and-record`: it serializes concurrent
  checks and durably records both the highest accepted generation and trusted
  wall-clock time. It rejects a valid older image and rejects clock rollback
  that could otherwise resurrect expired signing authority. Plain `verify` is
  for stateless offline inspection.
  The installed `/etc/chrony.conf` permits only NTS-authenticated servers from
  four independent operators and requires at least two selectable sources.
  It imports no DHCP, pool, peer, or configuration-directory sources, so an
  unauthenticated network time response cannot advance this persistent floor.
  Immediately before verification, the staged-image gate revalidates that
  owner-controlled configuration and requires `chronyc waitsync` to report
  less than 100 ms remaining correction. An unsynchronized future RTC therefore
  cannot become the durable trusted-time floor during early boot.
  Revoked signers fail regardless of their claimed issuance time, because a
  compromised device controls that timestamp. Neither a normal device
  certificate nor device-enrollment authority implicitly permits image signing.

  To require this additional owner chain for OS updates, arm it atomically:

  ```sh
  sudo tilo-image-trust arm root.pub revocations.bin image.endorsement \
    "$(date +%s%N)" /var/lib/tilo/image-trust
  ```

  `arm` validates the complete chain, serializes against updates, writes the
  canonical root/revocation/endorsement artifacts as mode `0600`, seeds both
  the trusted-time floor and the rollback floor from the arming endorsement,
  and publishes the `required` marker last. This prevents the first update from
  establishing a lower generation floor with another still-valid endorsement.
  It refuses to modify an already-armed store;
  use `tilo-image-trust rotate` with the same arguments for replacement. Rotation
  validates the complete new set and the existing rollback floor while holding
  the update transaction lock. It preserves the pinned human root and requires
  every installed device revocation with an equal or earlier cutoff; an empty,
  omitted, or later-dated replacement cannot silently un-revoke a device. Root
  recovery is deliberately a separate ceremony. An interrupted replacement remains fail-closed
  rather than allowing an update against a mixed trust set.
  Both automatic updates and `tilo-deployment update` verify
  the exact downloaded OCI digest and the `tilo.generation` measured by its UKI
  before activating it for boot. A rejection leaves the running deployment
  selected. Removing the marker returns to
  the mandatory official Cosign policy alone.
  Bounded `bootc status` JSON is parsed by the same non-dumpable Rust
  `tilo-image-trust` binary; the update authority path does not invoke Python or
  a general-purpose JSON command-line tool. Deployment and TPM rollback
  enrollment use only its fixed `booted-image`, `staged-image`,
  `rollback-image`, `booted-composefs`, and `staged-composefs` selectors; no
  caller-supplied expression is evaluated.

  Automatic updates never pass bootc's `--apply` flag: they download, verify,
  and stage in the background, then become active on the user's next ordinary
  restart. This prevents an update timer from rebooting an active game or work
  session.
- **SBOM.** CI generates a complete SPDX SBOM with Syft and attaches it to
  the image in the OCI registry. Rekor's public service limits attestation
  uploads to 100 KB, so the full OS inventory cannot be logged directly. The
  CI KMS signs its SHA-256 in the Rekor-logged SLSA provenance. Audit with
  `os/dev/verify-release-sbom.sh <image@sha256:digest>`; it verifies that
  provenance and hashes the downloaded inventory before accepting it.
- **Reproducible builds (release-blocking).** Every release is built twice. The
  second build bypasses the first build's layer cache, and the workflow refuses
  publication unless both OCI manifests are byte-identical.
  Achieved by: base pinned by **digest** against our mirror (below), Rust resolve pinned
  (the tracked `Cargo.lock`), a deterministic compile (`CARGO_INCREMENTAL=0`,
  `--remap-path-prefix=/build=.`, `LC_ALL=C.UTF-8`, `TZ=UTC`), and a
  `SOURCE_DATE_EPOCH` with layer/image timestamps clamped to it
  (`podman --source-date-epoch --rewrite-timestamp`; buildkit honours
  `SOURCE_DATE_EPOCH` + `provenance: false` in CI). CI derives the epoch from
  the pinned inputs, not the commit: `os/dev/image-epoch.sh "$BASE_REF" "$RPMS_REF"`,
  the newest creation time of the base and the RPM snapshot. Content embeds it
  (the UKI's initramfs, the rpmdb, the fontconfig caches), so with the commit
  time every release re-sent those layers unchanged; the commit time stays in
  `org.opencontainers.image.created`.
  Reproduce: `SOURCE_DATE_EPOCH=$(os/dev/image-epoch.sh "$BASE_REF" "$RPMS_REF") ./os/dev/build-image.sh`
  twice → identical `podman image inspect --format '{{.Id}}'`.
- **Cross-time reproducibility (frozen RPM repo).** Package versions are frozen
  too, so a rebuild *weeks later* is still byte-identical. The exact resolved RPM
  set is published as a digest-pinned OCI image (`os/Containerfile.rpms`, built +
  pushed by the `build-rpms.yml` CI job to `registry.tilo.so/tilo/rpms`) and
  addressed as `set-<fingerprint>`, where the fingerprint hashes both
  architectures' package sets, `BASE_REF`, and the files that shape the snapshot
  (`os/dev/rpm-snapshot-fingerprint.py`). Builds call
  `os/dev/resolve-rpm-snapshot.sh`, which looks up that tag and verifies
  signature, provenance and both architectures before printing the digest. Every
  `os/Containerfile` `dnf install` mounts that repo and installs from it only
  (`--setopt=reposdir=/repo`), never live Fedora mirrors.
  The reference is an OCI index containing exactly native AMD64 and ARM64
  repositories. Snapshot CI builds both transactions and verifies the index
  platforms before tagging; its privileged binfmt helper is itself commit- and
  digest-pinned. The index is signed through the non-exportable release key and
  carries verified SLSA provenance binding its exact digest, base image,
  architecture set, source, and canonical package-set hash. CI and local image
  builders reject it before any package transaction unless all of those claims
  match the current build inputs.
  **When you add/remove/rename a package in `os/Containerfile`,** merging it to
  `main` re-runs `build-rpms.yml` (or image builds call it as a reusable
  workflow). If `set-<fingerprint>` is already published the job exits early;
  otherwise it builds, signs, attests and tags. There is no hand-kept pin file
  and no pin pull request. The kernel does not build against this snapshot.
  Until the matching tag exists the build fails `No match for argument: <pkg>`
  (or resolve fails closed). The package list is auto-derived from
  `os/Containerfile`, so there is no separate list to maintain.
  Before publishing, the snapshot replays the image's erase of base packages it
  later needs again (the pkgconf family) and re-resolves the whole package set
  offline, so an incomplete snapshot fails in CI rather than in an image build.
- **Kernel buildroots.** The kernel's build dependencies and the Fedora kernel
  SRPM come from their own snapshot, `os/Containerfile.kernel-buildroot`,
  instead of the OS frozen RPM snapshot. `build-buildroots.yml` resolves
  it natively on x86-64 and ARM64 against the mirrored base, checks the SRPM's
  Fedora signature, records its checksum, the base and the package manifest,
  deletes every repository file so a kernel build cannot fall back to the
  network, and signs each digest through the non-exportable release key. Both
  pins land in `os/kernel-buildroot-snapshot.env` through an
  `automation/kernel-buildroot-snapshot-<digest16>` pull request. A base
  refresh re-runs it; an OS package change does not.
- **Native custom kernels.** Kernel CI rebuilds the hardened kernel separately
  on native x86-64 and ARM64 runners, checks the runner architecture, signs
  each digest through the non-exportable release key, and advances both pins
  in `os/kernel-snapshot.env` atomically only after both native jobs verify.
  A kernel pin is consistent on its own, so it lands whenever it comes from a
  later commit than main's pin (read from the pinned kernels' signed
  provenance), even if kernel inputs moved meanwhile; it closes older kernel
  pin pull requests, and the push runs those moves triggered follow it.
  Promotion creates a deterministic,
  snapshot-bound automation branch and pull request rather than bypassing
  protected `main`; normal required checks and merge policy remain
  authoritative. Each kernel builds FROM its signature-verified native
  buildroot and its provenance binds that buildroot (and the base the
  buildroot records), not `RPMS_REF`. Kernel CI therefore rebuilds only for
  kernel inputs: the kernel sources, config and patches, the buildroot pin,
  the Rust module signer and the kernel build scripts.
- **Pin promotion credentials.** Pin promotion pushes its automation branch
  with the job token but opens the pull request through
  `os/dev/open-snapshot-pr.py` with protected secret `RELEASE_TOKEN`. Forgejo
  answers the pull-request API with 404 for the push credential, while
  `RELEASE_TOKEN` has repository scope. If the script reports that the token
  cannot read pull requests, check that `RELEASE_TOKEN` exists and still grants
  repository access; the branch is already pushed, so a PR can be opened from
  it by hand in the meantime. OS CI selects the matching pin, verifies
  its Cosign signature, OCI architecture, and SLSA source, architecture and
  digest provenance, then verifies the signature of the kernel buildroot
  that provenance binds (`os/dev/verify-kernel-provenance.sh`), before
  BuildKit can consume it. The kernel is not compared with the OS image's own
  RPM snapshot or base: the kernel packages carry no userspace, and their
  runtime dependencies resolve from the OS snapshot during the image's
  kernel swap,
  and binds both kernel digests into the signed multi-architecture release
  provenance. ARM64 additionally requires kernel pointer authentication, BTI,
  KASLR, and full module-region randomization.
- **Cross-time reproducibility (mirrored base image).** Pinning the base by
  digest is not enough on its own: Fedora rebuilds the `fedora-bootc` tag daily
  and garbage-collects the manifest it replaces almost immediately, so a
  `FROM quay.io/fedora/fedora-bootc@sha256:…` pin stops resolving after roughly
  a day (`manifest unknown`) and every build fails at its first `FROM` — the
  `build-rpms.yml` job included, which made the package set unrefreshable too.
  So the base is **mirrored into `registry.tilo.so/tilo/fedora-bootc`** and every
  Containerfile takes it as `--build-arg BASE_REF`, pinned in
  `os/base-snapshot.env`. Our registry collects nothing, so the pin keeps
  resolving. The copy uses `skopeo copy --all --preserve-digests`, so the
  multi-arch index survives and the mirrored digest is the digest Fedora
  published — the mirror adds durability, not a second artifact to trust.
  **To refresh the base:** edit `BASE_SOURCE` and `BASE_REF` in
  `os/base-snapshot.env` (one-line digest bump). Builds and `build-rpms.yml`
  call `os/dev/ensure-base-mirror.sh`, which copies `BASE_SOURCE` into the
  mirror when the pinned digest is missing. Changing `BASE_REF` changes the
  RPM fingerprint, so the next snapshot publishes a new `set-<fingerprint>`
  tag; then merge the kernel buildroot pin PR that follows and the kernel pin
  PR that the new buildroot produces. The ensure step refuses a `BASE_SOURCE`
  whose digest no longer matches the pin, so a silent major bump fails closed
  instead of shipping.
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
- **Pinned workflow machinery.** Every external action executed by every CI
  workflow is referenced by its full commit hash. Moving an upstream `v3`/`v5`
  tag cannot alter a validation, build, signing, or promotion job;
  `os/tests/workflow_dependency_invariants.sh` enforces that repository-wide.

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

- **Installed-client transparency enforcement.** Release CI publishes Rekor
  evidence, but the installed update path currently enforces the Tilo signature
  through containers-policy; it does not independently require a fresh Rekor
  inclusion proof and checkpoint before booting the image.
- **Independent transparency witness.** Rekor provides public append-only
  evidence, but the repository has no separately administered witness key and
  checkpoint/freshness service yet. That independent operator is still needed
  for robust split-view and freeze detection; the existing release-signed
  freshness attestation is only the migration anchor, not an independent vote.
