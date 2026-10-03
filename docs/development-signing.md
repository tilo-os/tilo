# Development signing and production provisioning

Disposable test identities can exercise signature checks; they cannot establish
a production release or trust on physical hardware. The procedures here do not
install, partition, write USB media, change physical firmware, or enroll keys on
the host. A development image must remain visibly identified as such.

## Existing trust boundaries

| Identity | Current input and consumer | Development gap |
|---|---|---|
| OS image | `keys/cosign.pub` is copied by `os/Containerfile` to `/usr/share/tilo/cosign.pub`. `os/etc/containers/policy.json` requires its signature for `registry.tilo.so/tilo/tilo`. Release verification also reads the tracked public key. | The development named context supplies independent public trust, signature policy and a bound update origin. Signed disk/VM integration remains required. |
| PCR authorization | `keys/tpm2-pcr-signing.public.pem` is copied into the image and initramfs. `sign-target-prediction.sh` verifies signatures over the exact UKI and generation-bound PCR11 policy with the image's public key. | Test PCR trust must be injected before initramfs/UKI construction, not merely replaced in the final root filesystem. |
| EFI/UKI | `TILO_PCR_PREDICTION_SIGNER` exports its certificate; `TILO_EFI_SIGNING_CERT_SHA256` independently pins the SHA-256 of its DER bytes. Returned signatures and the unchanged Authenticode payload are checked. Production uses `keys/efi-signing.crt`, the KMS-held Tilo release certificate owners enroll in firmware `db`. | A test certificate is trusted only in a disposable VM variable store and must never be pinned for publication. |
| Marketplace registry owner | `TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY`, a nonzero 32-byte Ed25519 public key in hex, becomes `/usr/share/tilo/trust/tilo-main-registry-owner.pub`. | Branch CI can use the explicit RFC 8032 test-vector public key. It is not an independently held production owner identity. |
| Downloaded installer | Native host code signatures authenticate the embedded release catalog. Linux downloadable media/catalog signatures are checked against `TILO_LINUX_EXPECTED_FINGERPRINT`. | Disposable OpenPGP tests exercise these checks; their keys must never appear in a production catalog or published trust bundle. |

These identities have different purposes. An OpenPGP download key or marketplace
owner key does not authorize an OS image, PCR policy, or EFI executable. Existing
`COSIGN_PRIVATE_KEY`/`COSIGN_PASSWORD` secrets do not satisfy the production OS
workflow's `COSIGN_KEY_REF` requirement.

Development signer provisioning uses `tilo-crypto generate-tpm-authority-key`
for the PCR/floor authority and the separate
`generate-secure-boot-cert` command for EFI signing. The former deliberately
emits RSA-2048, matching the production authority and the key size portable
across the TPM implementations used by Tilo; a 4096-bit Secure Boot key is not
an interchangeable TPM authority.

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
files and a development manifest, parses keys and certificates with the
Rust-native `tilo-crypto` helper, requires an RSA PCR key of at least 2048 bits,
and rejects private PEM blocks, extra files, links and junctions. The owner
input is a lowercase 64-hex public key.
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
checked.

### Container-only development staging

`build-image.sh` accepts the public bundle through an explicit named build
context. It validates the bundle on the host and again inside the rootfs before
dracut. The image records public fingerprints and the pinned manifest under
`/usr/share/tilo/development-trust/`. The registry owner is derived from the
bundle; a conflicting supplied owner is rejected. EFI material is public
provenance only; this does not enroll a certificate or sign a UKI.

```sh
TILO_PUBLIC_TRUST_MODE=development \
TILO_PUBLIC_TRUST_BUNDLE="$PUBLIC_BUNDLE" \
TILO_PUBLIC_TRUST_MANIFEST_SHA256="$EXPECTED_PUBLIC_BUNDLE_SHA256" \
TILO_PCR_PREDICTION_SIGNER="$ROOTFUL_DEVELOPMENT_SIGNER" \
TILO_IMAGE=localhost/tilo-development:trust-test \
TILO_TARGET_IMGREF=registry.vm.test:5443/tilo-development/os:trust-test \
  bash os/dev/build-image.sh
```

Run this command as root using the local rootful Podman store. Provision
`ROOTFUL_DEVELOPMENT_SIGNER` with the helper described below, and include its
public certificate and PCR key in the selected bundle. The rootfs loader is
signed before the sealing stages; the final UKIs still require the separate
sealing/signing integration.

All normal pinned build inputs are still required. Development mode rejects
skip-build and production aliases, and returns after container creation,
before disk imaging. This is a prerequisite staging interface, **not an
installable or boot-validated development OS**. Registry provisioning and the
end-to-end signed disk/VM acceptance below remain incomplete. A separate
creation-only disk command is described below.
Production defaults compare the selected public bytes with the unchanged
tracked keys and reject development bundles.

The tiny `bash os/tests/named_public_trust_context.sh` fixture proves named
context precedence using scratch images without network access or downloaded
base images. It passed on Podman 5.8.1; BuildKit execution is still required
before claiming that backend is verified.

## Isolated development-image boundaries and remaining integration

Do not overwrite tracked public keys or reuse production aliases to get a test
image through the current checks. Implement a dedicated development path with
these boundaries:

1. Keep a new private signing directory outside the checkout, image context,
   artifact staging, and CI upload paths. Only its owner can read it. Key
   operations consume paths; never print private bytes or shell-trace commands
   containing secrets. The image context receives public material only.
2. Container staging provides an explicit **public-only named build context**
   for the development Cosign key, PCR key, EFI certificate and registry owner.
   Default/release builds continue to use their existing pinned inputs. The
   image includes a development marker and public fingerprints in provenance.
   Validation rejects private-key PEM blocks, unexpected
   files and redirected paths in the public bundle.
3. Apply PCR and image trust before dracut and UKI construction. Rebuild the
   initramfs, UKI and composefs seal together; verify that their embedded public
   identity matches the development bundle. Adding a public key after sealing
   does not preserve the existing measurement.
4. The development origin must explicitly name the retained HTTPS repository
   `registry.tilo.so/tilo/tilo-development`, or an isolated
   `HOST[:PORT]/tilo-development/os` repository, with a tag or immutable SHA-256
   digest. Retained-repository tags identify the native architecture, for example
   `development-x86_64` or `development-aarch64`; known build/profile/source
   architectures must match that suffix. Digest references bind architecture
   through native image verification. Alternate hosts or explicit ports cannot
   alias the retained repository. The production `registry.tilo.so/tilo/tilo`
   repository, credentials, URL schemes and other repository paths are
   rejected. The image binds this origin to its pinned public manifest. Runtime
   origin resolution requires root-owned, non-writable metadata, matching public
   keys, exact signature policy and matching registry configuration. Environment
   overrides cannot change that origin. Development policy rejects all other
   repositories and requires a matching-repository Sigstore signature for the
   selected one; the production policy remains unchanged.

   Keep the selected registry reachable from the builder, future VM and eventual
   development installation. The retained development repository reuses the
   existing HTTPS registry without sharing production signing keys or policy.
   HTTPS is required for every endpoint except literal `127.0.0.1`, which permits
   explicitly configured local HTTP staging. `localhost` is rejected to avoid
   ambiguous resolution. A loopback endpoint inside a VM refers to that VM, so
   it is suitable for disposable fixtures, not a persistent remote update origin. An isolated
   VM-network HTTPS endpoint can use a valid certificate trusted by the guest;
   custom CA provisioning is not implemented here. Signature attachment lookup
   is configured for the exact development repository. No registry is started,
   published to or trusted merely by running the public-bundle helper.
5. Install an independent test signer outside the checkout and runner temporary
   directory. Keep the same root-ownership, canonical-path, signature and
   payload-preservation checks as the existing interface. Do not add a bypass
   for those checks or let an untrusted artifact select its own verifier key.
   The rootful provider below implements this interface; connecting final image
   sealing to the signed disk/VM pipeline remains separate work.
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

The production disk path in `build-image.sh` and the path in
`build-custom-installer.sh` invoke final UKI/PCR signing when
`TILO_REQUIRE_PCR_PREDICTION=1` or `TILO_ALLOW_LOCAL_PCR_SIGNING=1`. The latter
is an invocation opt-in, **not** a key-injection mechanism or a firmware-trust
override. Do not set it on the current production-key image expecting a new
test key to work. Development mode signs the rootfs loader unconditionally and
stops after provisional container creation, before either of these final
UKI/PCR signing flags can trigger disk preparation.

### Provisioning the rootful development signer

On an isolated Linux preparation host, provision a fresh directory from the
reviewed source revision:

```sh
sudo python3 os/dev/setup-development-signer.py /var/lib/tilo-development-signer
```

The parent must already exist, be root-owned and not group/world writable. The
new directory must be outside the checkout, runner temporary directory and
artifact upload paths. Provisioning refuses an existing destination. It creates
independent 3072-bit RSA EFI and PCR keys with owner-only permissions, a visibly
named development certificate, a purpose marker and the standalone signer.
It returns only public input paths, the signer path and the EFI certificate's
SHA-256 over its DER bytes. A failed provision leaves an incomplete directory;
it never silently replaces an identity on retry.

The provider requires an already-root caller. It grants no privileges and is
not a network signing service. Use the same rootful Podman store throughout this
development build/seal/verify path. Do not mix a rootless build store with this
rootful provider. Cosign and marketplace owner identities remain separate;
copy only the exported certificate and PCR public key into the independently
pinned public bundle using `development-trust.py`.

Set `TILO_PCR_PREDICTION_SIGNER` to the installed `signer` path and independently
record its returned `efi_certificate_sha256` as
`TILO_EFI_SIGNING_CERT_SHA256`. The provider accepts the three existing signer
operations, rejects redirected inputs and unsafe private-key permissions, and
creates outputs exclusively in a root-owned, owner-only directory (such as the
existing caller's private `mktemp` work directory). Output traversal rejects
links; failed operations remove only their own output inode through the held
directory descriptor. The existing image-sealing caller must still
verify the signatures, certificate fingerprint and unchanged Authenticode
payload. No operation enrolls firmware keys or makes the development certificate
trusted by physical hardware.

The native CI fixture `os/tests/development_signer.py` provisions disposable keys,
verifies real PCR signatures and signs the native systemd-boot EFI executable.
It checks certificate identity and preserved executable contents, plus rejection
of changed messages, redirected inputs, unsafe key permissions and reused output
paths. It removes its private fixture directory afterward.

## Independent development source verification

`os/dev/verify-development-source.py` verifies a native OCI child before any
image code is executed. Supply an independently pinned public bundle, its
manifest digest, the expected source commit and architecture, and an immutable
source digest in the exact development repository selected at build time.
The embedded update reference may be a tag: embedding the image's own digest
would create a circular dependency. Development builds require a clean committed
checkout and record that commit in `org.opencontainers.image.revision`.

```sh
podman unshare python3 os/dev/verify-development-source.py \
  --source "$SIGNED_NATIVE_DEVELOPMENT_SOURCE" \
  --target "$DEVELOPMENT_UPDATE_REFERENCE" \
  --architecture aarch64 --revision "$EXPECTED_SOURCE_COMMIT" \
  --public-bundle "$PUBLIC_BUNDLE" \
  --manifest-sha256 "$EXPECTED_PUBLIC_BUNDLE_SHA256"
```

The host needs local Podman, Cosign, and `tilo-crypto`; it does not need
OpenSSL. Rootful storage can run the same helper as root. The helper verifies
the signature payload's exact repository and digest before pulling, validates
the native OCI identity, and inspects a mounted filesystem with host code. It
checks embedded trust, update policy, registry owner and any hardware profile.
It never starts the image's entrypoint.
Remote Podman and redirected signature repositories are rejected. If explicitly
provided, `REGISTRY_AUTH_FILE` is copied privately for both clients and removed
afterward. Only literal `127.0.0.1` permits HTTP; TLS certificate verification is
required elsewhere.

Disposable offline signing fixtures may explicitly select
`--offline-key-signature`; this still requires the pinned key's valid signature
but does not require a transparency-log entry. The receipt records that choice.
Normal verification requires the transparency log. The revision label is the
signer's statement, not independent proof of a reproducible build.

The JSON output is an audit receipt, never an installation authorization token.
A disk consumer must repeat verification and enforce container signature policy.
The receipt includes the full local immutable `image_id`. In-process consumers
can use `verified_source(args)` to repeat authentication and hold that exact
image mount and an independently validated public-bundle snapshot throughout
their checks. The context unmounts and removes its snapshot on success or
failure. Consumers must not re-resolve the update tag, reuse an old receipt as
authorization, or treat source identity as proof of valid boot artifacts,
generation, native execution, disk contents or firmware acceptance.
The existing container-only development stop remains in place. Unit tests cover
identity rejection and failure cleanup. Run
`podman unshare python3 os/tests/development_source_registry.py` with the pinned
Cosign tool available to exercise a real loopback registry, temporary signing
keys, accepted native image and rejected unsigned/wrong-identity inputs. The
`CI / development signing tools` workflow runs this fixture
on both native architectures. Final EFI/PCR sealing, disk creation and VM
acceptance remain required integration work.

## Verify the boot loader before sealing

### Compute the installation seal without a discovery install

After constructing the provisional final image from the signed rootfs, use
the pinned native bootc 1.16.4 storage command through:

```sh
python3 os/dev/compute-development-composefs.py \
  --image-id "$PROVISIONAL_FINAL_IMAGE_ID" --architecture aarch64 \
  > composefs-before.json
```

The host must be already-root native Linux, with bootc and Podman addressing
the same local rootful store. The helper rejects remote engines, mutable tags,
foreign architectures and other bootc versions. It records the tool hash,
immutable image ID and exact composefs digest. Temporary composefs data uses
`/var/tmp`; no disk is installed or partitioned.

bootc v1.16.4's `compute-composefs-digest-from-storage` applies the same OCI
filesystem and boot transformation as its fresh-install composefs pipeline,
using the default EROFS format. Use the returned `composefs_digest` with
`reseal-image.sh`'s immutable mode, then sign the target UKI/PCR prediction.
`sign-target-prediction.sh` projects the selected verified signed UKI into
`/boot/tilo-target/tilo-target.efi`; without an explicit target, more than one
normal UKI is an error.

Recompute the storage digest after those `/boot`-only layers and require exact
equality before publishing and signing the final image. The final source,
prediction and disk verifiers still run; this computation is not installed-byte
or boot acceptance.

`os/dev/verify-development-loader.py` validates a signed systemd-boot loader
against its original executable, native architecture, and independently pinned
EFI certificate fingerprint. It checks the Authenticode envelope and actual
signature using private snapshots of the input files. Its hash receipt describes
those snapshots; consumers must preserve or recheck the exact bytes they install.

The native development fixture invokes the packaged `bootctl` with an isolated
tmpfs as its ESP, image-only loader selection, and firmware-variable writes
disabled. It checks that the signed companion supplies both EFI loader paths,
records package versions and hashes, and rejects missing, unsigned, wrong-key,
wrong-architecture and tampered inputs. This tests loader selection and copying;
it does not prove a bootable disk or firmware acceptance.

Development builds require an already-root Linux caller and
`TILO_PCR_PREDICTION_SIGNER` pointing to the independently provisioned signer.
They build the unsealed `rootfs` stage first, then use
`os/dev/sign-development-rootfs.py` to validate its immutable local ID, embedded
trust and revision, sign the original loader externally, and add its verified
signed companion through `os/Containerfile.development-loader`. Private keys
never enter those build contexts. The existing sealing stages consume the
resulting immutable image through a named `rootfs` context with pulling disabled.
The helper requires an explicit `--source-date-epoch`; `build-image.sh` supplies
its resolved `SOURCE_DATE_EPOCH`. The loader layer uses the same image and file
timestamp normalization as the raw and final builds, avoiding wall-clock times
in the inserted layer. Native fixtures check image creation time and the signed
companion's installed modification time, then repeat production with the same
authenticated signing output and require the same immutable image ID. This
isolates layer reproducibility from any timestamps a signing provider embeds.

This integration still requires native end-to-end acceptance, followed by signed
disk and VM validation. The development container-only stop remains enabled.

## Reusing compilation when development trust changes

Keep completed compilation separate from public trust replacement and signing.
The reuse boundaries are:

| Change | Work required |
| --- | --- |
| Unchanged WPE inputs | Import the authenticated completed WPE stage; do not compile WebKit again. |
| Development public keys, owner, or update origin | Authenticate the retained raw rootfs, replace public trust, regenerate and verify its initramfs, then add the changed files as one image layer. |
| Signing or final image assembly | Consume immutable compiled rootfs and tooling inputs. |
| WPE recipe, toolchain, architecture, or dependency inputs | Produce a matching WPE artifact before the full OS consumer runs. |

`os/dev/import-development-rootfs.py` verifies the retained digest and signed
handoff against independently selected source identity. The rebind path keeps
the original kernel and generation. `complete-initramfs-rebind.py` clears the
pending trust-replacement marker only after checking the actual initramfs key
bytes against the selected public bundle. These checks do not prove bootability.

The completed WPE artifact consumer rejects missing or mismatched receipts;
it does not silently start an expensive fallback build. Compiler-cache hits,
completed-stage reuse, and a successful OS build are separate evidence: a small
cache fixture proves neither a completed WebKit build nor an installable image.
Measure cold compilation and warm reuse separately before setting build-time
expectations. Native rebind execution, final sealing, and disk/VM acceptance
remain required before using a rebound image for installation.

## Private disk-file allocation boundary

`os/dev/private_disk_image.py` supplies the file-allocation boundary for the
development disk consumer. It requires an already-root Linux caller and a canonical,
root-owned private output directory. It creates a new sparse regular file with
exclusive creation, holds the directory and file descriptors, and verifies the
same inode, owner, permissions, link count and size before handoff and retention.
Existing paths are rejected, including links and special files. Failure cleanup
removes only the file it created, even if the output directory was renamed.

The consumer must call `validate()` immediately before handing the path to its
trusted installer. Retaining an unaccepted test output is not disk verification;
only independent installed-content and boot checks can establish acceptance.
This helper does not invoke bootc, allocate a physical disk, prove free-space
sufficiency, or protect against a malicious root process. Its root Linux tests
use private sparse temporary files; signed disk and VM acceptance remain separate.

## Creation-only development test disks

`os/dev/create-development-disk.py` composes authenticated immutable source
verification, EFI/PCR verification and exclusive disk-file allocation. It
executes the verified local image ID with implicit pulling disabled. The
packaged `bootc` must advertise all required installation flags; source and
target signature policy and fetch checks remain enabled.

The test disk explicitly uses Btrfs, matching the development disk layout.
It does not inherit a root filesystem default from the Fedora base image;
composefs installation requires a filesystem supporting fs-verity.

When `REGISTRY_AUTH_FILE` is supplied, the source verifier's private temporary
snapshot is also mounted read-only into the installation container. The mount
and environment setting apply only to that invocation; credential bytes are
not included in the public receipt. Inspect the real installed image for secret
persistence before accepting it. This handoff does not provision credentials
for future updates or prove that the eventual update context can fetch them.

This command is for an operator-provisioned disposable native QEMU/KVM guest
with no device passthrough. It rejects WSL, user namespaces, remote container
configuration, exposed TPM devices and EFI variables. Those checks cannot prove
that a VM is disposable. Privileged Podman can access that VM's devices, so the
runner's isolation remains a prerequisite. Do not run it on the laptop or an
ordinary persistent VM. No physical disks are valid output arguments.

The caller supplies the independently pinned public bundle, source digest,
update origin, source revision, architecture, generation, composefs seal and
exact systemd measurement version. Output must be a new regular file in an
existing private root-owned directory. Failed installation or source teardown
removes the newly allocated output.

A successful receipt says `tilo-development-unaccepted-test-disk`, with
`installed_bytes_verified`, `vm_boot_verified` and `installable` all false.
It establishes command completion and output-file identity only. The actual
installed manifest, origin, partitions, EFI/UKI/PCR bytes, signature-policy
negative cases and boot behavior still need verification against the completed
candidate. The development build's container-only stop remains enabled.

Command/lifecycle tests do not establish runtime compatibility with the packaged
bootc, sufficient disk capacity, correct installed content or bootability.

## Production provisioning

Production identities live in AWS KMS (account 125650147312, `us-east-1`) and
are used only through the Forgejo OIDC role `TiloForgejoReleaseSigner`, which
trusts tilo/tilo `main` and `v*` tags:

| Identity | KMS alias | Tracked public identity |
|---|---|---|
| OS image (Cosign, ECDSA P-256) | `alias/tilo-release-signing` | `keys/cosign.pub` |
| PCR 11 predictions and target-UKI signatures (RSA-2048) | `alias/tilo-pcr-prediction` | `keys/tpm2-pcr-signing.public.pem` |
| EFI/UKI Authenticode (RSA-2048) | `alias/tilo-efi-signing` | `keys/efi-signing.crt` |

1. Non-exportable OS image and PCR signing identities are provisioned with
   narrowly scoped runner workload authorization, and their public keys are the
   tracked trust roots. An existing public key does not prove its private
   counterpart is held in the service; any replacement needs an explicit
   public-key transition.
2. `COSIGN_KEY_REF` is the service URI consumed by the OS workflow:
   `gcpkms://`, `awskms://`, `azurekms://`, or `hashivault://`. The workflow rejects
   file-based keys. Returned signatures are verified with the independently
   pinned `keys/cosign.pub` before release promotion.
3. The EFI trust anchor is `keys/efi-signing.crt`, a Tilo-owned code-signing
   certificate self-signed through its KMS key. It is not in any factory `db`:
   owners enroll it themselves (see the release-certificate section of
   [security/boot-chain.md](security/boot-chain.md)). This user-enrolled
   certificate is the supported production path; a factory-trusted chain may
   replace it later. Validate enrollment and boot on each supported hardware
   family. The PCR/EFI signer is baked root-owned into the native runner image
   (`code.tilo.so/public/runner-image`, `tilo-release-signer/`) at
   `/usr/local/libexec/tilo-release-signer/tilo-release-signer`, which the
   protected `TILO_PCR_PREDICTION_SIGNER` variable names;
   `TILO_EFI_SIGNING_CERT_SHA256` pins the certificate.
4. Independently verify the EFI certificate fingerprint. Its calculation uses
   only the public certificate and must equal `TILO_EFI_SIGNING_CERT_SHA256`:

   ```sh
   tilo-crypto certificate-sha256 keys/efi-signing.crt
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
