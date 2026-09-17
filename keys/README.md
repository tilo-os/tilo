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
> (`cosign.pub`, `tpm2-pcr-signing.public.pem`) — public keys are not secret;
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
| `tpm2-pcr-signing.private.pem` | **YES** (git-excluded) | Measured-boot PCR-prediction signing key (RSA-2048). Signs the *expected* PCR set so TPM disk-unlock survives updates. |
| `tpm2-pcr-signing.public.pem` | no (tracked) | PCR-prediction **public** key. Baked into the image; `systemd-cryptenroll --tpm2-public-key=` binds the LUKS keyslot to predictions signed by the matching private key. |

## Use

- **Image signing:** release CI uses a non-exportable KMS/HSM key. Updates
  verify against `cosign.pub` before applying; releases reject file-based keys.
- **Measured boot:** each UKI publishes a signed PCR prediction. LUKS binds to
  `tpm2-pcr-signing.public.pem`, allowing TPM unlock across trusted updates.

## Custody

The file-based keys are a development-only compatibility path, git-excluded and
`chmod 600`. Production image signing is KMS/HSM-backed: Forgejo stores only the
key URI, the runner receives short-lived workload credentials, and the private
key cannot be exported. The PCR/floor authority key must follow the same custody
model before TPM short-PIN enrollment is enabled for general releases.

**Never commit the private keys. Never print them. Rotate if ever exposed.**
