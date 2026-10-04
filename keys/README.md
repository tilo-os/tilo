# Tilo signing keys

> ## ⛔ AGENTS / AUTOMATED TOOLS — STOP
>
> **NEVER read, `cat`, `Read`, `head`, `tail`, `sed`, `grep`, `xxd`, `base64`,
> open, copy, upload, or otherwise emit the contents of the PRIVATE key files in
> this directory.** Doing so leaks the OS signing root into the transcript,
> logs, a commit, or an external service — a critical compromise.
>
> The private files are: **`cosign.key`**, **`cosign.password`**,
> **`tpm2-pcr-signing.private.pem`** (and anything matching `*.key`,
> `*.password`, `*.private.pem`). They are git-excluded (see the repo-root
> `.gitignore`) and must stay that way.
>
> You **MAY** read this README and the **public** files
> (`cosign.pub`, `tpm2-pcr-signing.public.pem`, `efi-signing.crt`) — public
> keys and certificates are not secret;
> verifiers need them. Use the keys only via tools that consume them by *path*
> (e.g. `cosign sign --key cosign.key`); never by printing them.
>
> If a task seems to require a private key's bytes, it does not — stop and ask a
> human.

---

## What's here

| File | Secret? | Purpose |
|---|---|---|
| `cosign.key` | **YES** (git-excluded) | Development-only image-signing key. Production CI refuses file-based keys. |
| `cosign.password` | **YES** (git-excluded) | Development-only passphrase for `cosign.key`. |
| `cosign.pub` | no (tracked) | Image-signing **public** key. Clients/`bootc` verify the image against this before applying an update. |
| `tpm2-pcr-signing.private.pem` | **YES** (git-excluded) | Development-only PCR-prediction key for local experiments. It does not match the tracked public key; production signs with AWS KMS. |
| `tpm2-pcr-signing.public.pem` | no (tracked) | PCR-prediction **public** key of AWS KMS `alias/tilo-pcr-prediction` (RSA-2048). Baked into the image and initramfs; release CI verifies every PCR 11 prediction and target-UKI signature against it. |
| `extensions.pub` | no (tracked) | Extension-manifest **public** key of AWS KMS `alias/tilo-extension-manifests` (ECDSA P-256). Installed as `/usr/share/tilo/extensions.pub`; devices accept a downloaded system extension's manifest only with a signature from this key, never from the OS key. Only `extension-manifests.yml` on main can sign (role `TiloForgejoExtensionManifestSigner`). |
| `efi-signing.crt` | no (tracked) | Tilo release EFI certificate (RSA-2048, code signing), self-signed through AWS KMS `alias/tilo-efi-signing`. Release UKIs carry its Authenticode signature; owners enroll it in firmware `db`. DER SHA-256 `6ce12ddcd6da3a8b678868f3979a61834f7ee1402a49773485748d4442eb0496` (`TILO_EFI_SIGNING_CERT_SHA256`). |

## Use

- **Image signing:** release CI uses a non-exportable KMS/HSM key. Updates
  verify against `cosign.pub` before applying; releases reject file-based keys.
- **Measured boot:** each UKI publishes a signed PCR prediction. LUKS binds to
  `tpm2-pcr-signing.public.pem`, allowing TPM unlock across trusted updates.
- **Secure Boot:** release UKIs are signed for `efi-signing.crt`. Verify its
  fingerprint with `tilo-crypto certificate-sha256 keys/efi-signing.crt`
  before enrolling it; see `docs/security/boot-chain.md`.

## Custody

The file-based keys are a development-only compatibility path, git-excluded and
`chmod 600`. Production signing is AWS KMS-backed (account 125650147312,
`us-east-1`) for all three release identities: `alias/tilo-release-signing`
(Cosign), `alias/tilo-pcr-prediction` (PCR/floor authority) and
`alias/tilo-efi-signing` (EFI). Forgejo stores only key references, the runner
receives short-lived OIDC workload credentials for `TiloForgejoReleaseSigner`,
and no private key can be exported. The EFI and PCR operations run through the
root-owned signer baked into the native runner image
(`TILO_PCR_PREDICTION_SIGNER`).

**Never commit the private keys. Never print them. Rotate if ever exposed.**
