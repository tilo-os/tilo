# qOS supply-chain security (operations)

Status of security-plan §9 (signing / reproducibility / transparency) and how to
operate it. Companion to `docs/superpowers/specs/2026-06-30-qos-security-architecture-plan.md`.

## What's in place

- **Image signing.** CI signs every pushed image by digest with cosign
  (`--tlog-upload=false`, own key). Keys: `keys/cosign.{key,pub}` +
  `keys/cosign.password` (private = git-excluded; CI uses the `COSIGN_PRIVATE_KEY`
  / `COSIGN_PASSWORD` fgj secrets). See `keys/README.md`.
- **Verify-before-update.** The image ships `/usr/share/qos/cosign.pub` + an
  `/etc/containers/policy.json` that **requires** a valid signature for
  `code.podesta.ai/qos/qos` (+ `registries.d` sigstore-attachment lookup). So
  `bootc upgrade` refuses an unsigned/tampered OS image.
- **SBOM.** CI generates an SPDX SBOM (syft) and attaches it as a **signed**
  in-toto attestation to the image (`cosign attest`). Audit with
  `cosign verify-attestation --key keys/cosign.pub --type spdxjson <image>`.
- **Reproducible builds (verified).** Two independent builds of the same commit
  produce a **byte-identical image** (confirmed: built twice, same image ID).
  Achieved by: base pinned by **digest** (both stages), Rust resolve pinned
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
  pushed by the `snapshot-rpms` CI job to `code.podesta.ai/qos/rpms`), pinned in
  `os/rpms-snapshot.env` (`RPMS_REF=…@sha256`). Every `os/Containerfile` `dnf
  install` mounts that repo and installs from it only
  (`--setopt=reposdir=/repo`), never live Fedora mirrors.
  **When you add/remove/rename a package in `os/Containerfile`,** re-run the
  `snapshot-rpms` workflow (`workflow_dispatch`); it prints the new `RPMS_REF` —
  commit that into `os/rpms-snapshot.env`. Otherwise the build fails
  `No match for argument: <pkg>`. The package list is auto-derived from
  `os/Containerfile`, so there is no separate list to maintain.

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

1. Generate a new pair (`cosign generate-key-pair`), keep the password separate
   (mirror `keys/README.md`).
2. Update the fgj secrets (`COSIGN_PRIVATE_KEY`, `COSIGN_PASSWORD`) — piped from
   files, never echoed.
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
- **Key custody.** Private keys are file-based + in the CI secret store. Move to
  a **KMS/HSM** (`cosign --key <kms-ref>`) or sigstore keyless so no copyable
  file can sign as qOS (plan §3 "no single root of trust").
