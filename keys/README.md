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
> `.gitignore`) and must stay that way. The release root's private key never
> enters this repository or any connected machine at all.
>
> You **MAY** read this README and the **public** files
> (`cosign.pub`, `tpm2-pcr-signing.public.pem`, `efi-signing.crt`, everything
> in `release/` and `operational/`) — public keys and certificates are not
> secret; verifiers need them. Use the keys only via tools that consume them by
> *path* (e.g. `cosign sign --key cosign.key`); never by printing them.
>
> If a task seems to require a private key's bytes, it does not — stop and ask a
> human.

---

## What's here

| File | Secret? | Purpose |
|---|---|---|
| `cosign.key` | **YES** (git-excluded) | Development-only image-signing key. Production CI refuses file-based keys. |
| `cosign.password` | **YES** (git-excluded) | Development-only passphrase for `cosign.key`. |
| `cosign.pub` | no (tracked) | Public key of the **operational release key**, AWS KMS `alias/tilo-release-signing` (ECDSA P-256). Every release signature verifies against it; CI's own checks pin it, and installed systems verify OS updates against it directly until the release root exists (below). |
| `release/root.crt` | no (tracked, **after the ceremony**) | The **release root** certificate (ECDSA P-384). Its private key is offline, ideally on a hardware token, and signs nothing but operational certificates. Once it exists, installed systems verify OS images and extension manifests through it. Release root DER SHA-256: none yet (the ceremony writes it here as `Release root DER SHA-256 \`<hex>\``). |
| `release/trust.env` | no (tracked) | The trust parameters installed systems apply: the revocation epoch, the identities operational certificates carry, their maximum validity. Installed as `/usr/share/tilo/release/trust.env`. |
| `release/rekor.pub` | no (tracked) | Public key of the public Sigstore transparency log (Rekor, `rekor.sigstore.dev`, log ID `c0d23d6a…801d`). Installed systems accept a release signature only with this log's signed entry timestamp. |
| `operational/os-images.crt` | no (tracked, **after the ceremony**) | Operational certificate binding `cosign.pub` (the KMS release key) to the `os-images` purpose for at most 90 days, issued by the root. |
| `operational/extension-manifests.crt` | no (tracked, **after the ceremony**) | The same for `extensions.pub` and the `extension-manifests` purpose. |
| `tpm2-pcr-signing.private.pem` | **YES** (git-excluded) | Development-only PCR-prediction key for local experiments. It does not match the tracked public key; production signs with AWS KMS. |
| `tpm2-pcr-signing.public.pem` | no (tracked) | PCR-prediction **public** key of AWS KMS `alias/tilo-pcr-prediction` (RSA-2048). Baked into the image and initramfs; release CI verifies every PCR 11 prediction and target-UKI signature against it. |
| `cosign-dev.pub` | no (tracked) | Development-signing **public** key of AWS KMS `alias/tilo-dev-signing` (ECDSA P-256). Development candidates published from main (`dev_signed` products in `tools/ci/products.toml`) carry its signature; only t's checks on main can sign with it (role `TiloTDevSigner`), and nothing that verifies a release accepts it. |
| `extensions.pub` | no (tracked) | Public key of the **operational extension-manifest key**, AWS KMS `alias/tilo-extension-manifests` (ECDSA P-256). Installed as `/usr/share/tilo/extensions.pub`; images without the release root accept a downloaded system extension's manifest only with a signature from this key, never from the OS key. Only t's `publish-extension-manifests` check on main can sign (role `TiloTExtensionManifestSigner`, identity-token audience `tilo-extension-manifests`). |
| `efi-signing.crt` | no (tracked) | Tilo release EFI certificate (RSA-2048, code signing), self-signed through AWS KMS `alias/tilo-efi-signing`. Release UKIs carry its Authenticode signature; owners enroll it in firmware `db`. DER SHA-256 `6ce12ddcd6da3a8b678868f3979a61834f7ee1402a49773485748d4442eb0496` (`TILO_EFI_SIGNING_CERT_SHA256`). |

## The release trust model

Two tiers, so that no key CI can use is a trust anchor on installed systems:

- The **release root** is held offline and never touches a build machine.
  Its only job is issuing **operational certificates**: short-lived (at most
  `TILO_RELEASE_OPERATIONAL_MAX_DAYS`, 90 days) X.509 certificates for one
  purpose (`os-images` or `extension-manifests`) and one non-exportable AWS KMS
  key. A certificate names its purpose as the e-mail identity
  `<purpose>@signing.tilo.so` and the current revocation epoch as its
  OIDC-issuer extension `https://signing.tilo.so/release/epoch-<N>`.
- **CI holds only operational keys**, inside KMS, reached through short-lived
  OIDC workload credentials. Each signature carries its operational
  certificate and is entered in the public transparency log (Rekor).
- **Installed systems verify release → operational certificate → root.**
  `bootc upgrade` goes through `/etc/containers/policy.json`, whose
  `sigstoreSigned` requirement uses containers/image's native `fulcio` mode with
  the release root standing in for a Fulcio CA (`caPath`), the purpose
  identity (`subjectEmail`) and epoch (`oidcIssuer`), and Rekor's key
  (`rekorPublicKeyPath`). The certificate must be valid at the time Rekor
  logged the signature, so releases stay verifiable after their certificate
  expires, while a stolen operational key stops being usable within at most
  90 days even if nobody notices. The extension helper checks manifests the same
  way (`tilo-crypto verify-release-bundle`).

Rotating or renewing an operational key is a new certificate in
`operational/`, committed like any change; installed systems need nothing new.
Revoking is raising `TILO_RELEASE_EPOCH`: the next image accepts only
certificates of the new epoch.

The policy is generated (`python3 tools/signing/release_trust.py policy`) and
has two modes, chosen by whether `release/root.crt` exists: before the ceremony
it pins `cosign.pub` plus the Rekor log (**key mode**); from the commit that adds
the root on, it requires the chain (**chain mode**). Its default is `reject`;
every other image reference it lists says why (`release_trust.py`).

**Why public Rekor, not a self-hosted log:** the point of the log is that
someone other than Tilo can see every signature Tilo's keys made, including
one made with a stolen key. A log Tilo runs itself would be operated by the
same party it is meant to watch, and would add a service to keep alive; the
public instance is independently operated and monitored, costs nothing and
already holds Tilo's release signatures. The trade-offs: installed systems
depend on its key (`release/rekor.pub`; a key rotation by Sigstore needs a Tilo
image that carries the new key), and containers/image verifies only the v1
log's signed entry timestamps, so if Sigstore stops accepting v1 entries
before containers/image supports its successor, releases cannot be logged and
the policy needs a self-hosted v1 log's key instead. Everything Tilo logs is
public anyway (images, their digests, the operational certificates).

**What this does not cover, and why:**

- **CI-internal artifacts** (the RPM, kernel and buildroot snapshots, the
  env-runtime, the builder, the relay, the imager, the SLSA and freshness
  attestations) are verified only by this repository's workflows and scripts,
  against `cosign.pub`. That pin lives here, so changing it is a commit, not a
  trust-anchor change on any installed system.
- **`cosign-dev.pub`** is development trust, deliberately outside the release
  root: a development candidate must never chain to what verifies releases.
- **`tpm2-pcr-signing.public.pem`**: the TPM's `PolicyAuthorize` binds one
  public key and cannot follow a certificate chain, so the PCR authority stays a
  single KMS key; rotating it re-binds LUKS on each machine.
- **`efi-signing.crt`**: firmware `db` could hold a root and accept
  operational leaves, but owners enroll this certificate by fingerprint today;
  moving Secure Boot to the same two tiers is a separate change.

## Owner steps

Commands below run from a checkout of this repository unless a step says
*offline*. Each step's outcome is a commit pushed to `main`; t enforces the
rest: its `release-trust` check (and `release-trust-expiry`, daily) fails for a
committed certificate that does not certify the KMS key's public half, has
under `TILO_RELEASE_OPERATIONAL_MIN_REMAINING_DAYS` (14) days left, or does not
chain to the root, and every certified signature is verified through the
chain before anything is published.

### 1. The root ceremony (once)

You need an air-gapped machine (a live USB with networking disabled is fine)
with OpenSSL 3 and a copy of `tools/signing/release-ca.sh`,
`keys/release/trust.env`, `keys/cosign.pub` and `keys/extensions.pub`. For a
hardware token (recommended): a YubiKey with the `yubikey-manager`,
`yubico-piv-tool` (for `libykcs11.so`) and `pkcs11-provider` packages, and two
encrypted offline media for the backup.

1. *Offline*, create the root key. A key generated inside a token cannot be
   backed up, and losing the root means re-anchoring every installed system, so
   generate it in memory, back it up encrypted twice, and import it into the
   token:

   ```sh
   umask 077
   openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-384 \
     -aes-256-cbc -out /media/backup-1/tilo-release-root.key
   cp /media/backup-1/tilo-release-root.key /media/backup-2/
   ykman piv keys import --pin-policy ALWAYS --touch-policy ALWAYS \
     9c /media/backup-1/tilo-release-root.key
   # libykcs11 exposes a slot's key once the slot holds a certificate.
   ykman piv certificates generate --subject 'CN=Tilo Release Root' 9c \
     <(openssl pkey -in /media/backup-1/tilo-release-root.key -pubout)
   export PKCS11_PROVIDER_MODULE=/usr/lib64/libykcs11.so
   ROOT_KEY='pkcs11:object=Private%20key%20for%20Digital%20Signature;type=private'
   ```

   Without a token, keep the encrypted file and use `ROOT_KEY=/media/backup-1/tilo-release-root.key`.
2. *Offline*, create the root certificate and note its fingerprint on paper:

   ```sh
   tools/signing/release-ca.sh init-root --key "$ROOT_KEY" --out root.crt
   ykman piv certificates import 9c root.crt   # token only
   ```
3. *Offline*, issue the operational certificates for the existing KMS keys.
   Their public halves are `keys/cosign.pub` and `keys/extensions.pub` (CI
   already checks that they match KMS), so no key leaves KMS and no secret
   changes:

   ```sh
   tools/signing/release-ca.sh issue --root root.crt --key "$ROOT_KEY" \
     --purpose os-images --public-key keys/cosign.pub --out os-images.crt
   tools/signing/release-ca.sh issue --root root.crt --key "$ROOT_KEY" \
     --purpose extension-manifests --public-key keys/extensions.pub \
     --out extension-manifests.crt
   ```
4. Carry the three **certificates** (never the key) to the connected machine
   and commit them, with the regenerated policy, in one commit:

   ```sh
   cp root.crt keys/release/root.crt
   mkdir -p keys/operational
   cp os-images.crt extension-manifests.crt keys/operational/
   tools/signing/release-ca.sh verify --root keys/release/root.crt \
     --purpose os-images keys/operational/os-images.crt
   python3 tools/signing/release_trust.py policy
   ```

   In the `release/root.crt` row above, replace the "none yet" sentence with
   ``Release root DER SHA-256 `<hex>` `` (`release-ca.sh inspect
   keys/release/root.crt` prints it; `tools/tests/test_release_trust_roots.py`
   checks it), compare it with your paper copy, run `t test`, and push.
5. Put the token and both backups in separate safes. Every image built from
   that commit on verifies updates through the root; images built before keep
   verifying `cosign.pub`, which is why OS images carry both signatures (the
   transition, step 5 below).

### 2. Renew an operational certificate (at least every 76 days)

Same key, new certificate, before the old one is within 14 days of expiry
(signing refuses it then). *Offline*:

```sh
tools/signing/release-ca.sh issue --root root.crt --key "$ROOT_KEY" \
  --purpose os-images --public-key keys/cosign.pub --out os-images.crt
```

Commit it over `keys/operational/os-images.crt` (likewise for
`extension-manifests` with `keys/extensions.pub`). Nothing else changes.

### 3. Rotate an operational key

Only once no supported installation runs a key-mode image (step 5): those
images verify `cosign.pub` itself and would refuse the new key.

1. In AWS KMS (account 125650147312, `us-east-1`), create an asymmetric
   `ECC_NIST_P256` `SIGN_VERIFY` key and give its sign permission to the roles
   that held the old one: `TiloTReleaseSigner` for `os-images`, `TiloTExtensionManifestSigner` for `extension-manifests`.
   Save its public key: `cosign public-key --key awskms:///<key ARN> > new.pub`.
2. *Offline*, issue its certificate (`issue ... --public-key new.pub`).
3. In one commit: the certificate in `keys/operational/`, `new.pub` as
   `keys/cosign.pub` (or `keys/extensions.pub`), and the key reference in
   `tools/ci/products.toml` (`signing_env`, or the extension check's `env`).
4. CI's own checks pin `keys/cosign.pub`, so artifacts still pinned under the
   old key (`os/kernel-snapshot.env`, `os/kernel-buildroot-snapshot.env`,
   `os/legacy-guest-snapshot.env`, the RPM and env-runtime snapshots) must be
   rebuilt and re-pinned after the swap. Then disable the old KMS key and
   schedule its deletion.

### 4. Revoke an operational key (compromise)

1. Disable the KMS key at once (`aws kms disable-key`) and remove its role
   grants: nothing can sign with it from then on.
2. Create a replacement key (step 3.1).
3. Raise the epoch: `tools/signing/release-ca.sh revoke`. Images built from
   here on accept only certificates of the new epoch.
4. *Offline*, issue the replacement key's certificate at the new epoch, and a
   **bridge** certificate for the same replacement key at the old epoch, so
   installed systems still on the old epoch can verify the update that moves
   them on:

   ```sh
   tools/signing/release-ca.sh issue ... --public-key new.pub --out os-images.crt
   tools/signing/release-ca.sh issue ... --public-key new.pub --epoch <old epoch> \
     --out os-images.bridge.crt
   ```
5. Commit both under `keys/operational/`, the public key and key reference as
   in step 3.3, plus `python3 tools/signing/release_trust.py policy`; swap the
   secret. CI signs each OS image once per certificate.
6. Search the transparency log for what the stolen key signed:
   `rekor-cli search --email os-images@signing.tilo.so`. Until a machine
   installs a new-epoch image it still accepts the stolen key's certificate,
   at most until that certificate expires.
7. Delete the bridge certificate once supported installations run new-epoch
   images.

### 5. Finish the transition from the key-pinned policy

Images built before the root commit verify updates against `cosign.pub` and
require a log entry naming that key, which a certificate entry never matches.
So `tools/ci/publish/oci-sign.sh` gives every release of a repository installed
systems verify (the OS and legacy-guest images, `device_verified` in
`tools/ci/publish/lib.sh`) both a bare signature by the release key and one
certified signature per operational certificate. Installed systems
cross over by installing any update built after the root commit. Once no
supported installation runs an older image, rotate the release key (step 3):
that drops the old key's trust everywhere, and the root is the only anchor.

### 6. Replacing the root

A different ceremony: images must trust the new root before anything is
issued under it, so the new root is introduced by an image that still accepts
the old one. Plan it when the root nears expiry (`release-ca.sh inspect`) or if
it is ever exposed.

## Use

- **Image signing:** release CI signs with non-exportable KMS keys; installed
  systems verify updates through `/etc/containers/policy.json` (above).
- **Measured boot:** each UKI publishes a signed PCR prediction. LUKS binds to
  `tpm2-pcr-signing.public.pem`, allowing TPM unlock across trusted updates.
- **Secure Boot:** release UKIs are signed for `efi-signing.crt`. Verify its
  fingerprint with `tilo-crypto certificate-sha256 keys/efi-signing.crt`
  before enrolling it; see `docs/security/boot-chain.md`.

## Custody

The file-based keys are a development-only compatibility path, git-excluded and
`chmod 600`. Production signing is AWS KMS-backed (account 125650147312,
`us-east-1`) for all release identities: `alias/tilo-release-signing`
(Cosign, the `os-images` operational key), `alias/tilo-extension-manifests`
(the `extension-manifests` operational key), `alias/tilo-pcr-prediction`
(PCR/floor authority) and `alias/tilo-efi-signing` (EFI). `tools/ci/products.toml`
names only key references, t's signing checks receive short-lived OIDC workload
credentials for their roles (`TiloTReleaseSigner`, `TiloTExtensionManifestSigner`),
and no private key can be exported. The EFI and PCR
operations run through the root-owned signer baked into the native runner image
(`TILO_PCR_PREDICTION_SIGNER`). The release root is offline and held by the
owner.

**Never commit the private keys. Never print them. Rotate if ever exposed.**
