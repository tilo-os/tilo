# Development signing and production provisioning

Disposable test identities can exercise signature checks; they cannot establish
a production release or trust on physical hardware. The procedures here do not
install, partition, write USB media, change physical firmware, or enroll keys on
the host. A development image must remain visibly identified as such.

## Existing trust boundaries

| Identity | Current input and consumer | Development gap |
|---|---|---|
| OS image | `keys/cosign.pub` is copied by `os/Containerfile` to `/usr/share/tilo/cosign.pub`. `os/etc/containers/policy.json` requires its signature for `code.sandbox.example/qos/tilo`. Release verification also reads the tracked public key. | No public-key build-context override exists. A separate test repository needs an explicit signature policy and test-only update-origin binding. |
| PCR authorization | `keys/tpm2-pcr-signing.public.pem` is copied into the image and initramfs. `sign-target-prediction.sh` verifies signatures over the exact UKI and generation-bound PCR11 policy with the image's public key. | Test PCR trust must be injected before initramfs/UKI construction, not merely replaced in the final root filesystem. |
| EFI/UKI | `TILO_PCR_PREDICTION_SIGNER` exports its certificate; `TILO_EFI_SIGNING_CERT_SHA256` independently pins the SHA-256 of its DER bytes. Returned signatures and the unchanged Authenticode payload are checked. | A test certificate is trusted only in a disposable VM variable store. Its fingerprint alone does not make stock firmware trust it. |
| Marketplace registry owner | `TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY`, a nonzero 32-byte Ed25519 public key in hex, becomes `/usr/share/tilo/trust/tilo-main-registry-owner.pub`. | Branch CI can use the explicit RFC 8032 test-vector public key. It is not an independently held production owner identity. |
| Downloaded installer | Native host code signatures authenticate the embedded release catalog. Linux downloadable media/catalog signatures are checked against `TILO_LINUX_EXPECTED_FINGERPRINT`. | Disposable OpenPGP tests exercise these checks; their keys must never appear in a production catalog or published trust bundle. |

These identities have different purposes. An OpenPGP download key or marketplace
owner key does not authorize an OS image, PCR policy, or EFI executable. Existing
`COSIGN_PRIVATE_KEY`/`COSIGN_PASSWORD` secrets do not satisfy the production OS
workflow's `COSIGN_KEY_REF` requirement.

Some historical comments still describe PCR authorization as unused or
owner-enrollment as the general boot model. The executable checks in
`os/dev/sign-target-prediction.sh`, `os/usr/libexec/tilo-verified-boot-install-preflight`,
and the protected release workflows are the current interfaces.

## Tests available now

On the native Linux preparation/CI host, from the repository root:

```sh
bash os/tests/imager_linux_release_trust.sh
bash os/tests/authenticode_envelope.sh
bash os/tests/installer_target_invariants.sh
python3 apps/imager/packaging/test_release_catalog.py
```

The first test creates an ephemeral OpenPGP identity outside the checkout,
exercises real signatures, and removes its private keyring. The other tests
exercise catalog selection, synthetic PE-envelope mutations and installer
preflight fixtures. They do not establish firmware acceptance, measured boot,
an installable OS image, or hardware compatibility.

## Preparing public build inputs

`os/dev/development-trust.py` prepares and validates a public-only bundle from
existing development public keys and a certificate. It does not generate keys,
sign images, inject image trust, or enroll firmware certificates. Private key
generation and custody remain outside this helper and outside the checkout.

```sh
python3 os/dev/development-trust.py create \
  --cosign-public "$DEV_COSIGN_PUBLIC" \
  --pcr-public "$DEV_PCR_PUBLIC" \
  --efi-certificate "$DEV_EFI_CERTIFICATE" \
  --registry-owner "$DEV_REGISTRY_OWNER_PUBLIC" \
  --output "$NEW_PUBLIC_BUNDLE"
```

The destination must not exist. The helper accepts exactly the four public
files and a development manifest, parses keys/certificates with OpenSSL, requires
an RSA PCR key of at least 2048 bits, and rejects private PEM blocks, extra
files, links and junctions. The owner input is a lowercase 64-hex public key.
Certificate parsing does not establish firmware trust or certificate validity
for a particular signing service.

Record the returned `manifest_sha256` in the independently controlled build
invocation. Validate the transferred bundle against that value:

```sh
python3 os/dev/development-trust.py validate "$PUBLIC_BUNDLE" \
  --expected-manifest-sha256 "$EXPECTED_PUBLIC_BUNDLE_SHA256"
python3 os/tests/test_development_trust.py
```

Without the expected digest, validation checks structure and internal hashes
only; it cannot authenticate who selected those public identities. The
expected digest must not be obtained from the same untrusted transfer being
checked. No OS build consumes this bundle yet.

## Isolated development-image design — implementation still required

Do not overwrite tracked public keys or reuse production aliases to get a test
image through the current checks. Implement a dedicated development path with
these boundaries:

1. Keep a new private signing directory outside the checkout, image context,
   artifact staging, and CI upload paths. Only its owner can read it. Key
   operations consume paths; never print private bytes or shell-trace commands
   containing secrets. The image context receives public material only.
2. Add an explicit **public-only named build context** for the development
   Cosign key, PCR key and EFI certificate. Default/release builds continue to
   use their existing pinned inputs. Include a development marker and public
   fingerprints in image provenance. Reject private-key PEM blocks, unexpected
   files and redirected paths in the public bundle.
3. Apply PCR and image trust before dracut and UKI construction. Rebuild the
   initramfs, UKI and composefs seal together; verify that their embedded public
   identity matches the development bundle. Adding a public key after sealing
   does not preserve the existing measurement.
4. Use a dedicated loopback-only test registry and repository, with its own
   required-signature policy. Add a narrowly scoped development update-origin
   contract shared by image metadata and installer validation. The current
   `hardware_profile.py` and `tilo-install` deliberately reject that origin;
   no development-origin override exists today. Production policy must keep
   rejecting test repositories and test identities.
5. Install an independent test signer outside the checkout and runner temporary
   directory. Keep the same root-ownership, canonical-path, signature and
   payload-preservation checks as the existing interface. Do not add a bypass
   for those checks or let an untrusted artifact select its own verifier key.
6. Before any VM boot test, use a unique cycle with disposable QCOW2 disks,
   software TPM state and a private firmware variable store. Never pass through
   a host block device, physical TPM, or efivarfs. Enrollment is permitted only
   in that VM's variable store. This VM procedure is planned; the development
   build and enrollment integration described here is not implemented yet.

The planned path needs tests proving both acceptance of the correct development
identity and rejection of substituted keys, mismatched measurements, unsigned
images and production publication of development artifacts. There is no
end-to-end development-signing command yet; implement these boundaries before
presenting one as usable.

### Existing signer invocation, once the development image boundary exists

The wrapper must implement exactly:

```text
SIGNER export-efi-cert OUTPUT_CERTIFICATE
SIGNER sign-efi INPUT_UKI OUTPUT_UKI
SIGNER sign-sha256-rsa-pkcs1 INPUT OUTPUT_SIGNATURE
```

It must be an absolute, canonical, executable, non-symlink path; it and all
parents must be root-owned and not group/world writable. For a correctly
prepared development image, the existing invocation is:

```sh
# Variables below must identify the isolated test image and test signer.
export TILO_EFI_SIGNING_CERT_SHA256
bash os/dev/sign-target-prediction.sh "$TILO_IMAGE" "$TILO_PCR_PREDICTION_SIGNER"
```

`build-image.sh` and `build-custom-installer.sh` invoke that interface only when
`TILO_REQUIRE_PCR_PREDICTION=1` or `TILO_ALLOW_LOCAL_PCR_SIGNING=1`. The latter
is an invocation opt-in, **not** a key-injection mechanism or a firmware-trust
override. Do not set it on the current production-key image expecting a new
test key to work.

## Production provisioning later

1. Choose the KMS/HSM provider and release operator. Provision non-exportable OS
   image and PCR signing identities with narrowly scoped runner workload
   authorization. Reconcile their public keys with the tracked trust roots;
   an existing public key does not prove its private counterpart is held in
   the new service. Any replacement needs an explicit public-key transition.
2. Configure `COSIGN_KEY_REF` as the service URI consumed by the OS workflow:
   `gcpkms://`, `awskms://`, `azurekms://`, or `hashivault://`. The workflow rejects
   file-based keys. Verify returned signatures with the independently pinned
   `keys/cosign.pub` before release promotion.
3. Arrange the EFI signing path trusted by the intended stock firmware, and
   validate the actual chain on each supported hardware family. A newly
   generated self-signed certificate does not meet this requirement. Provision
   the PCR/EFI wrapper on both native Linux runner images and configure protected
   `TILO_PCR_PREDICTION_SIGNER` and `TILO_EFI_SIGNING_CERT_SHA256` variables.
4. Independently verify the EFI certificate fingerprint. Its calculation uses
   only the public certificate:

   ```sh
   openssl x509 -in release-efi.crt -outform DER | sha256sum
   ```

5. Establish and publish the independently held marketplace owner identity;
   configure `TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY` with its verified public key.
   Configure platform signing/notarization brokers and expected publisher
   identities as specified in [imager-release-operations.md](imager-release-operations.md).
6. Verify the protected workflow/ref/OIDC policies before issuing release
   credentials. Build signed native OS children and their index, with the real
   generation, provenance, firmware and board profile. Only then configure
   `TILO_INSTALLER_OS_IMAGE` to the verified published index digest and bind the
   board's reviewed update reference. Those values are build results, not
   placeholders to invent beforehand.
7. Complete installer/media, update/rollback and physical hardware acceptance.
   Confirm a recoverable backup before any destructive installation step.
   Development signature tests do not remove any of these release gates.
