# qOS signing keys

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
| `cosign.key` | **YES** (git-excluded) | Image-signing private key (cosign, ECDSA P-256). Signs the OS container image in CI. |
| `cosign.password` | **YES** (git-excluded) | Passphrase that decrypts `cosign.key`. Needed alongside the key to sign. |
| `cosign.pub` | no (tracked) | Image-signing **public** key. Clients/`bootc` verify the image against this before applying an update. |
| `tpm2-pcr-signing.private.pem` | **YES** (git-excluded) | Measured-boot PCR-prediction signing key (RSA-2048). Signs the *expected* PCR set so TPM disk-unlock survives updates. |
| `tpm2-pcr-signing.public.pem` | no (tracked) | PCR-prediction **public** key. Baked into the image; `systemd-cryptenroll --tpm2-public-key=` binds the LUKS keyslot to predictions signed by the matching private key. |

## How they're used (per the security plan)

- **Image signing (security plan §9):** CI runs `cosign sign --key keys/cosign.key code.podesta.ai/qos/qos@<digest>` (password via `COSIGN_PASSWORD`, sourced from `cosign.password`). The A3 auto-update path verifies the signature against `cosign.pub` **before** applying — refuse unsigned/untrusted images.
- **Measured boot (security plan §4):** the build/update pipeline runs `systemd-measure sign --private-key tpm2-pcr-signing.private.pem …` to publish a **signed PCR prediction** for each new UKI, so the TPM keeps auto-releasing the disk key across updates (no recovery-key prompt on every update). LUKS is enrolled with `--tpm2-public-key=tpm2-pcr-signing.public.pem`.

## Custody (this is the *dev* custody model — harden for production)

These are **file-based** keys, git-excluded, `chmod 600`. That is the minimum.
Per the security plan's "no single root of trust": for production, move the
private keys out of files and into a **CI secret store / KMS / HSM** (or
sigstore keyless for image signing), so a stray file copy can't sign as qOS.
The `cosign.password` separation means a leaked `cosign.key` alone is not
immediately usable.

**Never commit the private keys. Never print them. Rotate if ever exposed.**
