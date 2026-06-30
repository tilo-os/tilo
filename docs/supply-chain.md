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
- **Reproducibility (partial).** The base image is **pinned by digest** (both
  build stages, same digest → glibc match) and the Rust resolve is pinned via
  `os/Cargo.lock.pinned`. Not yet bit-for-bit reproducible (see below).

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
- **Bit-reproducible builds.** Need `SOURCE_DATE_EPOCH`, deterministic file
  ordering/timestamps, and a pinned toolchain so a third party can rebuild and
  match the published digest. Today only the inputs (base digest + lockfile) are
  pinned.
- **Key custody.** Private keys are file-based + in the CI secret store. Move to
  a **KMS/HSM** (`cosign --key <kms-ref>`) or sigstore keyless so no copyable
  file can sign as qOS (plan §3 "no single root of trust").
