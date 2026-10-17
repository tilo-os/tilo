# Tilo Installer release operations

The release workflow builds only on native, repository-scoped runners. Relevant
protected `main` pushes automatically publish immutable signed **Tilo Installer**
prereleases; protected `imager-v*` tags publish stable releases. Existing package
IDs, filenames and tag prefixes retain `Imager` for compatibility. Configure
these runner labels:

| Label | Host | Required tooling |
|---|---|---|
| `imager-linux-x86_64` | Linux x86-64 | Rust, Cap'n Proto compiler, Docker + Buildx, GPG or KMS client, Podman, QEMU/4 MiB measured OVMF, swtpm, `virt-fw-vars`, zstd, Homebrew, Ruby, curl, jq, Node.js and npm, UDisks2, Flatpak + flatpak-builder with the 25.08 SDK |
| `imager-linux-aarch64` | Linux ARM64 | same Linux toolchain, release utilities, Docker/Buildx, ARM64 QEMU/UEFI, and UDisks2 service, running natively |
| `imager-windows-x86_64` | Windows x86-64 | Rust MSVC, Cap'n Proto compiler, Windows SDK `signtool`, Inno Setup 6, WinGet client |
| `imager-windows-aarch64` | Windows ARM64 | Native ARM64 Rust MSVC and C++ build tools, Cap'n Proto compiler, Windows SDK `signtool`, Inno Setup 6.3 or newer, WinGet client |
| `imager-macos-x86_64` | Intel macOS | Xcode CLI tools, Rust, Cap'n Proto compiler, `hdiutil`, `notarytool`, Homebrew |
| `imager-macos-aarch64` | Apple silicon | same macOS toolchain, running natively, Homebrew |

Manual installer-only rehearsals do not need those release labels: dispatch
`release-imager.yml` with `installer_only=true` and `full_native=true`. The
workflow uses the native `runner-disk-100g` and `runner-disk-100g-arm64`
large-disk lanes for erase, prepared-alongside, and guided-alongside cycles. Their isolated
job containers use QEMU's slower TCG accelerator and capturable software
framebuffer because host KVM/DRI devices are intentionally not exposed. Tagged
releases still require hardware KVM/DRI and the dedicated six-platform fleet
above; installer-only rehearsals skip host packaging and every publication job.

Use Forgejo 15 or newer and Forgejo Runner 12.5 or newer. Register every host at
**repository scope** (`tilo/tilo`), select ephemeral mode, expose exactly one of
the labels above as `<label>:host`, and start it with `forgejo-runner one-job`.
The registration is invalidated after that one job; the VM must be destroyed,
not returned to a warm pool. The practical autoscaling sequence is:

1. Clone a patched, read-only golden image into a new VM with no signing key.
2. Ask the Forgejo administrator/API for a new repository-scoped ephemeral
   runner UUID and token. Deliver them through instance metadata with a TTL,
   never in the image or Terraform state.
3. Start `forgejo-runner one-job` with the one native `:host` label, wait for it
   to exit, revoke the registration if the job never arrived, and destroy the
   VM and attached disk in all cases.

Golden Linux and macOS images should install
`tools/ci/runners/imager-ephemeral-linux.sh` as a root-owned, non-writable executable
outside the checkout. Invoke that installed copy as
`/usr/local/sbin/run-imager-ephemeral-runner LABEL UUID_FILE TOKEN_FILE`.
Windows images should likewise install
`tools/ci/runners/imager-ephemeral-windows.ps1`
outside the checkout with an ACL writable only by Administrators, then invoke
it as `powershell.exe -NoLogo -NoProfile -NonInteractive -File
C:\ProgramData\Tilo\Run-ImagerEphemeralRunner.ps1 -Label imager-windows-x86_64 -UuidFile
C:\run\tilo-runner\uuid -TokenFile C:\run\tilo-runner\token`. Both launchers
validate the native OS/architecture-to-label binding, require
Forgejo Runner 12.5 or newer, accept only private runner-owned metadata files
inside a private runner-owned directory, pin the Forgejo origin, run exactly
`one-job` with a `:host` label, bound the wait to six hours, and erase both
registration values on every exit after their paths pass safety validation. Set
`TILO_RUNNER_MAX_WAIT_SECONDS` to a smaller orchestration deadline when useful;
the accepted range is 1 second through 24 hours. The final success message is a
destruction instruction, not permission to return the VM to a pool. Run the
PowerShell launcher directly on Windows; do not use a Unix compatibility layer
as the release host.

The repository runner page is `/tilo/tilo/settings/actions/runners`. Forgejo's
[runner-registration guide](https://forgejo.org/docs/latest/admin/actions/registration/)
documents both the UI/API registration response and offline administrator CLI;
its [Actions security guide](https://forgejo.org/docs/latest/admin/actions/security/)
documents why repository scope, ephemeral registration, and `one-job` belong
together. Set an orchestration timeout because an ephemeral registration which
never receives a job does not expire on its own.

Pull requests and unrelated `main` changes retain the affected-package tests
and release invariants in `check.yml`. Installer, profile-tool, workspace manifest,
lockfile and release-workflow changes on `main` additionally enter the existing
native release queue, without reserving runners or dispatching OS builds.
Manual rehearsals receive no signing lease. Do not put a persistent private key
or cloud credential on the native fleet.

## Automatic main downloads

`Release / Tilo Installer` builds all six native Linux, Windows and macOS lanes
on relevant `main` pushes, after the existing exact-source Desktop catalog gate.
Linux retains signed capture qualification, AppImage runtime and installed
Flatpak checks; Windows requires Authenticode-signed native packages; macOS
requires signed and notarized packages. Missing packages or unavailable subjects
fail rather than becoming successful platform skips.

The public release identity is `imager-main-<full source SHA>`, while native
package versions come from that source's `apps/imager/Cargo.toml`. No synthetic
semantic version or source tag is created. The existing GitHub publisher still
starts with a draft, authenticates the Linux trust bundle, verifies uploaded
bytes, and refuses replacement of immutable assets. Continuous releases are
prereleases, never GitHub's stable latest release. Homebrew, WinGet and website
promotion remain tag-only.

Main builds do not build installer media or OS images. They reuse the exact
published media recorded in the reviewed `apps/imager/release-catalog.json`,
fetch its catalog, checksums and signatures, compare the published catalog with
the reviewed bytes, and verify the complete set with the independently protected
Linux key fingerprint before publication. An empty `unreleased` catalog fails.
The exact-source Desktop catalog, published installer catalog, native ephemeral
fleet, capture prerequisites and broker policy below must be provisioned before
a main run can publish. Enabling the trigger is not a publication receipt.

The workflow protection preflight queries Forgejo's repository and main-branch
API, checks the immutable source repository ID and requires a boolean protected
branch response. It does not use `github.ref_protected`: Forgejo 16 can report
that workflow context as false for protected branches. This API preflight is
not a credential or JWT claim override. The signing broker must independently
verify protected-main authorization from the issuer's authenticated claims;
if its `ref_protected` claim is false, repair the issuer's protected-ref
reporting before enabling publication. Never waive that broker check based on
the workflow's metadata result.

Installer media never allocate an OS generation. Set the protected
`TILO_INSTALLER_OS_IMAGE` variable to one already-published, cosign-signed Tilo
multi-architecture manifest-list reference of the form
`registry.tilo.so/tilo/tilo@sha256:<64 lowercase hex>`. Both native Linux lanes
verify that same digest and its SLSA provenance, require its immutable `genN`
tag to bind the exact digest, pull their native manifest,
and require the generation embedded in the image to equal the single generation
bound by provenance before layering the live installer. Provenance must also
bind the exact checkout commit, preventing installer-owned OS files from being
mixed with an older published root while retaining its generation. A manual rehearsal may
override the variable with the `published_os_image` input, subject to all the
same repository, digest, signature, provenance, and embedded-generation checks.

The workflow materializes checksum-pinned `linuxdeploy`, `appimagetool`, and
type-2 runtime binaries from the Forgejo cache separately for each Linux
architecture. They do not need to be baked into the runner, and AppImage
creation is forbidden from downloading an implicit “latest” runtime. CI runs
the completed AppImage through both its extraction compatibility path and its
normal embedded FUSE runtime. An extract-only result is accepted solely when
the kernel exposes neither the FUSE filesystem nor `/dev/fuse`; a missing or
inaccessible device on a FUSE-capable runner is a release failure, and the
fallback reason is retained in the job summary.
The same golden images preinstall `org.freedesktop.Sdk//25.08` and
`org.freedesktop.Platform//25.08` system-wide and run UDisks2
with at least the VM's system disk visible. The Flatpak build forbids downloads
during bundle assembly, installs the emitted bundle into a clean per-job user
installation, launches that installed application, and reconciles its sandbox
inventory with that host UDisks2 view before it can become an artifact.
End-user Linux hosts need UDisks2 (standard on major desktop
distributions); its polkit prompt mediates the one exclusive removable-drive
descriptor used by the sandboxed writer.

## Short-lived credentials

The eight credentialed jobs set Forgejo's native `enable-openid-connect: true`.
Do not add GitHub's `permissions: id-token: write`: Forgejo ignores that field
and emits a workflow warning. Repository capabilities come from Authorized
Integrations; the native key is what causes Forgejo to inject
`ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`. The exact
Forgejo behavior and token fields are documented in its
[OIDC guide](https://forgejo.org/docs/latest/user/actions/security-openid-connect/).

Configure the external secret broker with all of these claim checks:

- issuer: `https://code.tilo.so/api/actions`
- audience: a dedicated value such as `tilo-release-signing`
- `repository == tilo/tilo` and the pinned immutable `repository_id`
- `event_name == push`, `ref_type == tag`, `ref` matches only
  `refs/tags/imager-vMAJOR.MINOR.PATCH`, and `ref_protected == "true"`
- `workflow == release-imager.yml`, and `workflow_ref` belongs to `tilo/tilo`
- `sub` equals the instance's actual protected-tag subject. Forgejo 16 may add
  immutable numeric owner/repository IDs to this string, so capture one denied
  bootstrap token and pin the exact format instead of assuming the older
  `repo:tilo/tilo:ref:...` form.

For automatic main publication, configure a **separate protected-main claim
profile** for only the Linux, Windows, macOS and public-release brokers. Keep
the protected-tag profile above unchanged for stable releases and all media,
Secure Boot, Homebrew, WinGet, internal-bundle and website credentials. The main
profile must require the same issuer, audience, immutable repository ID,
workflow identity, JWT validation and replay prevention, but require
`event_name == push`, `ref_type == branch`, `ref == refs/heads/main` and
`ref_protected == "true"`. Pin the instance's actual protected-main subject and
bind the lease to the JWT's exact source revision, run and platform. The public
release token remains scoped solely to the public downloads repository. A
manual dispatch on main is not eligible. Existing tag-only brokers will deny
continuous publication until this bounded profile is installed; the workflow
must fail on that denial, never fall back to unsigned downloads.

Validate JWT signature, `exp`, `nbf`, and `iat` against the issuer's discovery
document/JWKS. Reject replay by `run_id` + `run_attempt` + platform, and never
accept a caller-supplied repository, ref, workflow, or audience outside the
signed JWT.

Install a root/Administrator-owned, non-workspace broker client on each golden
image. Paths are deliberately separate because one absolute path cannot be
valid on Linux, Windows and macOS. Configure these protected repository
variables:

| Variable | Invoked on | Output contract |
|---|---|---|
| `IMAGER_LINUX_CREDENTIAL_BROKER` | both Linux architectures | environment file described below |
| `IMAGER_WINDOWS_CREDENTIAL_BROKER` | Windows x86-64 and ARM64 | environment file described below |
| `IMAGER_MACOS_CREDENTIAL_BROKER` | both macOS architectures | environment file described below |
| `IMAGER_RELEASE_CREDENTIAL_BROKER` | Linux x86-64 promotion runner | raw GitHub App installation token file |
| `IMAGER_HOMEBREW_CREDENTIAL_BROKER` | Linux ARM64 | raw GitHub App installation token file |
| `IMAGER_WINGET_CREDENTIAL_BROKER` | Linux x86-64 | raw GitHub token file |
| `IMAGER_SITE_CREDENTIAL_BROKER` | Linux x86-64 | raw Forgejo token file |
| `TILO_PCR_PREDICTION_SIGNER` | both Linux architectures | direct KMS/HSM signing interface described below |
| `TILO_EFI_SIGNING_CERT_SHA256` | both Linux architectures | lowercase SHA-256 of the release EFI certificate's DER bytes |

Each value is an absolute executable path appropriate to that host. The
workflow invokes it as `BROKER PLATFORM OUTPUT`, where `PLATFORM` is `linux`,
`windows`, `macos`, `release`, `homebrew`, `winget`, `site`, or `internal-bundle`. The broker receives
`ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, requests the
configured audience, validates its server-side claims, and exchanges the JWT
for a narrowly scoped lease. It must create `OUTPUT` with mode 0600 (or a
Windows ACL granting only the runner account) and fail if that path already
exists:

```sh
oidc_token="$(curl --fail --silent --show-error \
  -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=tilo-release-signing" | jq -er .value)"
# Exchange this JWT with the signing service. Never write the JWT or returned
# credential to OUTPUT_ENV or an artifact.
```

`TILO_PCR_PREDICTION_SIGNER` is a deliberately narrower direct interface. After
the installer UKI's composefs digest has converged, the medium builder invokes:

```text
SIGNER sign-sha256-rsa-pkcs1 INPUT OUTPUT_SIGNATURE
SIGNER sign-efi INPUT_UKI OUTPUT_UKI
SIGNER export-efi-cert OUTPUT_CERTIFICATE
```

It receives the Forgejo OIDC request environment, validates the protected
workflow/ref and native architecture server-side, and asks the KMS/HSM to make
an RSA PKCS#1 v1.5 SHA-256 signature without exporting the private key. The EFI
operation obtains the Authenticode signature of the Tilo release EFI
certificate (`keys/efi-signing.crt`, which owners enroll in firmware `db`; see
[security/boot-chain.md](security/boot-chain.md)) and must omit
nondeterministic timestamps. The
export operation returns its public X.509 signing certificate; CI verifies its
DER SHA-256 fingerprint against the independently protected
`TILO_EFI_SIGNING_CERT_SHA256` repository variable and verifies every returned
UKI against that exact certificate. The two
inputs are the exact target UKI and `PolicyPCR || generation_be64`; neither is
secret. The builder verifies both returned signatures against the independently
pinned identities before adding the sidecars: the fingerprint above for EFI,
and `/usr/share/tilo/tpm2-pcr-signing.public.pem` for PCR authorization.
The signer and every parent directory must be root-owned and not group/world
writable, and it must not live in the checkout or runner temporary directory.
Tagged releases fail closed when this protected variable is absent.
The same direct signer is also used by both native `Release / OS` publication
lanes before their child manifests can enter the multi-architecture index. Its
server-side OIDC policy must therefore accept either the protected
`release-imager.yml` semantic-tag identity above or `build-image.yml` on a
protected `vMAJOR.MINOR.PATCH` tag, an authorized publishing dispatch from
`main`, the nightly scheduled run on protected `main`, or an enabled continuous
publication push to protected `main`. Reject all pull requests, branch
rehearsals, non-main dispatches, and other workflows. Tag lanes run on the
`imager-linux-*` hosts; `main` publication lanes run on `runner-disk-100g` and
`runner-disk-100g-arm64`, so the wrapper must be installed at the same
canonical path on those runner images too.

For `linux`, `windows`, and `macos`, the broker configures the native signer and
writes only these non-secret handles to the environment-file output, one
newline-terminated `NAME=value` per line:

- Linux: `TILO_LINUX_SIGNER=/absolute/root-owned/wrapper` and
  `TILO_LINUX_VERIFY_KEY=/absolute/root-owned/release-public-key.asc`; packaging
  imports that public key into a fresh isolated job keyring and verifies its
  complete fingerprint against the independent protected policy. The wrapper
  receives input and output-signature paths and emits an armored detached
  OpenPGP signature. Set
  protected repository variable `IMAGER_LINUX_SIGNING_FINGERPRINT` to the same
  full fingerprint. This policy value is deliberately supplied independently
  of the broker: a broker returning a different public key cannot silently
  rotate the release identity.
- Windows: `TILO_WINDOWS_SIGNER=C:\absolute\root-owned\wrapper.exe`; it receives
  one artifact path and returns after Authenticode signing and timestamping.
- macOS: `TILO_MAC_SIGNING_IDENTITY=<Developer ID Application identity>` and
  `TILO_NOTARY_KEYCHAIN_PROFILE=<temporary notarytool profile>` after leasing the
  certificate into a temporary keychain and storing a scoped App Store Connect
  API credential in that profile.
- GitHub releases and Homebrew: create a GitHub App owned by `tilo-os`, with
  repository permission **Contents: read and write**, no organization/account
  permissions, and no webhook. Install it only on the existing public
  `tilo-os/releases` and `tilo-os/homebrew-tilo` repositories. Keep the App ID,
  installation ID, and private key in the external broker (prefer an HSM/KMS
  signer); never store the private key or a PAT in Forgejo.

  After validating Forgejo OIDC, the broker exchanges `release` for a GitHub
  App installation token narrowed to repository `releases`, or `homebrew` for
  one narrowed to `homebrew-tilo`. Request only `contents: write`. GitHub App
  installation tokens expire after one hour; the jobs mint them immediately
  before promotion and revoke them when the step exits. The repository helper
  calls `/installation/repositories` and rejects a token which can see zero,
  two, or a different repository. This prevents accidentally returning the
  App's installation-wide authority.

  Both repositories must already have a `main` branch and remain public and
  non-archived. If branch protection covers the tap's `main`, allow only this
  GitHub App to bypass it, or change the tap publisher to a reviewed-PR flow.
  The releases repository must permit the App to create releases and tags.
  GitHub limits each release asset to 2 GiB; CI rejects an oversized installer
  before creating a draft. If a compressed installer exceeds that limit, move
  binaries to an object store with OIDC-issued upload leases instead of
  splitting an image the Imager expects to verify and stream as one object.

  The publisher checks the target's exact full name and public/non-archived
  state, reads every attachment back byte-for-byte through GitHub's API, and
  publishes only a complete draft. Set `IMAGER_PUBLIC_RELEASE_REPOSITORY` or
  `IMAGER_HOMEBREW_TAP_REPOSITORY` only when intentionally using another
  pre-created public target governed by an equivalently narrow App install.

  A minimal broker implementation mints a GitHub App JWT (ten-minute maximum),
  then calls `POST /app/installations/INSTALLATION_ID/access_tokens` with one of
  these bodies:

  ```json
  {"repositories":["releases"],"permissions":{"contents":"write"}}
  {"repositories":["homebrew-tilo"],"permissions":{"contents":"write"}}
  ```

  Return only the response's `token` bytes in `OUTPUT`, with mode `0600` and no
  trailing newline. Do not return the expiry, App JWT, private key, or JSON.
  The common helper rejects multiline/noisy output. The broker should keep a
  replay cache for the validated Forgejo `run_id`, `run_attempt`, tag, and
  platform; `release` and `homebrew` are distinct audiences/uses even when one
  GitHub App installation covers both repositories. The App private key is necessarily a
  longer-lived root credential, but it remains outside CI. Back it with a
  non-exportable KMS/HSM key when possible and rotate it independently of the
  one-hour installation tokens visible to jobs.
- WinGet: for the `winget` platform, write the complete raw GitHub token. A
  GitHub App **installation** token scoped only to the bot's fork can push the
  branch but cannot be relied on to create a pull request in
  `microsoft/winget-pkgs`, where the app is not installed. Use a dedicated bot
  account which has completed Microsoft's CLA and authorize an expiring GitHub
  App **user access token** with Contents and Pull requests read/write. GitHub
  fixes these tokens at eight hours (with a rotating refresh token), so this is
  the one promotion credential that cannot be reduced to 5–15 minutes. Keep the
  app client secret and rotating refresh token in the broker, return an access
  token only after Forgejo OIDC validation, and revoke it after submission when
  supported.
- Site: for the `site` platform, write a 5–15 minute Forgejo token as the
  complete raw output. Give its bot write access only to `tilo/site`. After all
  native and package-index gates pass, the workflow updates only
  `src/data/imager-release.json`, including its exact public `releaseRoot`, runs
  the site's release contract and full production build, and pushes that
  exact-tag change to `main`. Promotion also inspects the generated site and
  refuses to push unless its signed setup link uses that manifest root and no
  private-source download URL remains.

The workflow rejects every other output key. The underlying code-signing
identities are not all ephemeral, but their private material never needs to be
a Forgejo secret:

- Linux: use a KMS/HSM-backed OpenPGP key, or have the broker issue a short
  signing lease. The packaging job verifies each returned signature against the
  configured public-key fingerprint before upload. It exports that exact public
  key and a machine-readable fingerprint as release attachments. The promotion
  job uses a fresh keyring to verify both architecture's tarballs and AppImages
  before it creates or changes a Forgejo release. Publish the same full
  fingerprint independently on the HTTPS website (and preferably a second
  authenticated channel); the release-hosted key alone cannot bootstrap trust
  in the server that delivered it.
- Windows: keep the EV/OV Authenticode certificate in Azure Trusted Signing or
  another PKCS#11/KMS service. Federate Forgejo OIDC into that service and
  install a small signer wrapper on the ephemeral runner image. The
  `IMAGER_WINDOWS_CREDENTIAL_BROKER` validates the job identity and returns that
  trusted wrapper path for the lease. The packaging script passes it one
  artifact path at a time and then runs `signtool verify`; the wrapper signs by
  key reference and timestamps the file without exporting a PFX.
- macOS: the Developer ID certificate itself is long-lived. Keep it in a
  signing service or secrets manager and release a short lease to the
  ephemeral macOS runner. Prefer an App Store Connect API key for notarization;
  scope it to the Tilo team and retrieve it through the same OIDC lease.

There is no static signing or publication bootstrap. A release tag fails closed
when any required OIDC broker is absent or rejects the job. Initial bring-up
uses the unsigned, non-publishing `full_native` workflow dispatch until all
broker-backed release paths are ready; private signing keys, PFX/P12 archives,
Apple app passwords, and publication tokens must not be stored in Forgejo.

## Bring-up checklist

Before creating a release tag, verify all of the following:

- the protected tag rule matches `imager-v*`, forbids deletion/force-push, and
  restricts creation to release maintainers;
- six native labels are online at least once: `imager-linux-x86_64`,
  `imager-linux-aarch64`, `imager-windows-x86_64`, `imager-windows-aarch64`,
  `imager-macos-x86_64`, and `imager-macos-aarch64`; each reports its expected
  native architecture and `:host` execution;
- each golden image can build this repository from a clean checkout and has the
  tooling in the table above; Linux hosts can use rootful Podman for live-media
  assembly, and Windows/macOS hosts have access to their native GUI session for
  the packaged smoke launch;
- all seven broker-path variables plus the direct PCR signer path above are
  protected; the Linux full fingerprint is independently set in protected variable
  `IMAGER_LINUX_SIGNING_FINGERPRINT`; `IMAGER_WINDOWS_PUBLISHER` pins the full
  Authenticode certificate subject; `IMAGER_MAC_TEAM_ID` and
  `IMAGER_MAC_CERT_SHA256` pin the Apple team and leaf signing certificate;
  `WINGET_GITHUB_USER` names the CLA-enabled bot; and `SITE_RELEASE_USER` names
  the Forgejo website bot when it is not `oauth2`;
- a denied dry run proves that a main-branch, pull-request, wrong workflow,
  unprotected tag, expired token, replayed token, and wrong platform receive no
  lease;
- an allowed dry run returns only the documented output shape, the lease expiry
  is bounded, and destroying the VM removes its temporary keychain/key handles;
- Homebrew tap and website repositories already exist with branch protection
  permitting the release bot, the GitHub fork exists, the bot has signed the
  Microsoft CLA, and no PR already exists for the package version.

First publish and promote the multi-architecture Tilo image from the exact commit
to be tagged, then record its manifest-list digest in `TILO_INSTALLER_OS_IMAGE`.
Protected manual `main` publication uses the native large-disk x86-64 and ARM64
fleet with QEMU's TCG fallback; version tags remain reserved for the dedicated
KVM/DRI release runners. Both routes keep the same reproducibility, encrypted
install/boot, signature, SBOM, SLSA provenance, and two-architecture promotion
gates.
Dispatch `Release / Tilo Installer` manually on that commit with `full_native` enabled.
That run does not sign or publish, but proves every native label and packaged UI.
Only after it is green should a maintainer create a protected semantic tag such
as `imager-v1.2.3` on the same commit. A successful tag must finish the native matrix, release trust
verification, Homebrew installation on four hosts, WinGet PR creation, and the
landing-page production build in that order.

The Forgejo release is created as a draft. CI uploads and reads back every
required attachment, including signed installer images, the signed installer
catalog, Windows portable zip, and both macOS app zips, before making that draft
public. Reruns may resume a draft, but never add to or replace files in an
already-public release.

### Forgejo configuration inventory

Steady state has no exported signing-key secret in Forgejo. Create these
repository variables (broker paths should only be changeable by release
administrators):

| Variable | Required value |
|---|---|
| the seven `IMAGER_*_CREDENTIAL_BROKER` variables listed above | absolute path on each corresponding golden image |
| `IMAGER_PUBLIC_RELEASE_REPOSITORY` | optional GitHub binary repository in `OWNER/REPOSITORY` form; omit for `tilo-os/releases` |
| `IMAGER_HOMEBREW_TAP_REPOSITORY` | optional GitHub tap repository; omit for `tilo-os/homebrew-tilo` |
| `TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY` | independently verified lowercase 64-hex Tilo-main registry-owner Ed25519 public key |
| `TILO_INSTALLER_OS_IMAGE` | signed, published multi-architecture `registry.tilo.so/tilo/tilo@sha256:...` manifest-list reference used by installer media |
| `TILO_PCR_PREDICTION_SIGNER` | absolute root-owned KMS/HSM wrapper path on both native Linux runner images |
| `TILO_EFI_SIGNING_CERT_SHA256` | independently verified lowercase 64-hex fingerprint of the Tilo release EFI certificate (`keys/efi-signing.crt`) |
| `IMAGER_LINUX_SIGNING_FINGERPRINT` | uppercase 40- or 64-hex full primary-key fingerprint, independently verified |
| `IMAGER_WINDOWS_PUBLISHER` | complete expected Authenticode certificate subject, independently verified |
| `IMAGER_MAC_TEAM_ID` | 10-character Apple Developer team identifier, independently verified |
| `IMAGER_MAC_CERT_SHA256` | uppercase 64-hex SHA-256 fingerprint of the expected Developer ID leaf certificate |
| `WINGET_GITHUB_USER` | dedicated GitHub submission bot login |
| `SITE_RELEASE_USER` | Forgejo website bot login; omit to use `oauth2` |
| `IMAGER_INTERNAL_BUNDLE` | optional: unset (default) skips the internal installer bundle; `export` tests and exports it on every run; `publish` also publishes it for tags and requires it in the catalog. See [Internal installer bundle](#internal-installer-bundle) |
| `IMAGER_INTERNAL_BUNDLE_CREDENTIAL_BROKER` | with `publish`: absolute broker path on both native Linux runner images, platform `internal-bundle` |
| `IMAGER_INTERNAL_BUNDLE_OWNER` | optional Forgejo owner of the `tilo-internal-installer` generic package; omit for `tilo` |

The live-medium builder separately needs protected secret `REGISTRY_TOKEN` for
`code.tilo.so`, where board (Lenovo) target images stay private; the generic
Tilo images and their signature/attestation attachments are anonymously
readable on `registry.tilo.so`. Give its service account read-only access to
the private board images. The workflow copies its
short-lived Podman auth file into a private Docker config for Cosign, then
destroys both after use; installer-media CI never
pushes an OS image or allocates a generation. Rotate it independently of
signing. The kernel buildroot snapshot workflow uses the same token and signing
secrets as the other snapshot workflows; see below.
The source job's `github.token` is deliberately never used for binary
publication. `IMAGER_RELEASE_CREDENTIAL_BROKER` exchanges the protected-tag
OIDC identity for the short-lived, target-only token described above.

Delete and do not configure these retired legacy secrets and variables; the
release and monitor workflows do not read them:
`IMAGER_ALLOW_STATIC_SIGNING_BOOTSTRAP`, `IMAGER_LINUX_SIGNING_KEY_B64`,
`IMAGER_LINUX_SIGNING_KEY_ID`, `IMAGER_WINDOWS_CERT_PFX_B64`,
`IMAGER_WINDOWS_CERT_PASSWORD`, `IMAGER_MAC_CERT_P12_B64`,
`IMAGER_MAC_CERT_PASSWORD`, `IMAGER_MAC_SIGNING_IDENTITY`, `IMAGER_APPLE_ID`,
`IMAGER_APPLE_TEAM_ID`, `IMAGER_APPLE_APP_PASSWORD`, `HOMEBREW_TAP_TOKEN`,
`WINGET_GITHUB_TOKEN`, `SITE_RELEASE_TOKEN`, or `IMAGER_WINDOWS_SIGNER`.

### Internal installer bundle

Full Tilo without a USB stick (design spec
`docs/architecture/usb-free-install.md` §3) downloads a
schema-2 internal installer bundle: a manifest plus two raw component images
the Imager writes to staging partitions on the internal disk. Each native
`medium` lane derives it from the live disk that has just passed the
installation cycles:

1. **Test** (`export` or `publish`): installs `btrfs-progs`/`dosfstools`/
   `qemu-img`, runs `os/tests/internal_bundle_test.py`,
   `os/tests/internal_bundle_catalog.sh` and, as root,
   `os/tests/internal_bundle_export.sh` against loop devices.
2. **Export** (`export` or `publish`): `os/dev/export-internal-bundle.sh`
   runs `os/dev/internal-bundle.py export` through `sudo -n` on
   `os/output/installer/qcow2/disk.qcow2`. It copies the one live UKI, unchanged
   and still signed, to `/EFI/BOOT/BOOT{X64,AA64}.EFI` (the UKI finds the
   staging root through `root=tilo-auto` and the Tilo staging root partition
   type), reserves the handoff files, shrinks
   the Btrfs root to its minimum plus 1 GiB, and writes
   `tilo-internal-installer-ARCH{.json,-esp.img,-root.img}` plus the catalog
   entry `internal-catalog-ARCH.json`. Tag builds record the tag as
   `release_id`; rehearsals record `rehearsal-<sha>`.
3. **Publish** (`publish`, tags only): `publish-internal-bundle.sh` uploads
   components first and the manifest last to the Forgejo generic package
   `https://code.tilo.so/api/packages/OWNER/generic/tilo-internal-installer/TAG/`,
   then downloads each file anonymously and compares digests. Existing files
   are never replaced; a rerun only verifies them.
4. **Catalog** (`publish`, tags only): the lane uploads only the manifest and
   its entry (artifact `tilo-internal-catalog-ARCH`). `build-release-catalog.sh`
   checks every entry against the exact manifest bytes beside it, parses the
   manifest with the exporter's own validator, requires the package URL
   above, and with `TILO_REQUIRE_INTERNAL_BUNDLE=1` requires both
   architectures. The entries appear as `CatalogRelease::Internal`
   (`purpose: tilo-internal-installer`, `layout: internal-bundle-v2`). The
   GitHub and Forgejo release publishers skip these catalog inputs.

Why a Forgejo package on `code.tilo.so` rather than a release attachment:
the Imager accepts internal manifests from that origin only
(`trusted_catalog_url` in `apps/imager/src/release.rs`); each architecture
publishes independently without racing one draft release; the root image
routinely exceeds GitHub's 2 GiB asset limit; and the multi-GiB images never
travel through workflow artifacts. Configure Forgejo accordingly:

- the package owner is public, so anonymous downloads work;
- package blobs are served by Forgejo itself: the Imager refuses cross-origin
  redirects, so object-storage `SERVE_DIRECT` must stay off for packages;
- `[packages] LIMIT_SIZE_GENERIC` (and any reverse-proxy body limit) admits
  the root image, typically several GiB;
- the `internal-bundle` broker returns a 5–15 minute token with
  `write:package` for that owner only, after the usual protected-tag OIDC
  validation.

Publishing a bundle does not enable Full Tilo: builds offer it only when
compiled with `TILO_IMAGER_INTERNAL_INSTALL=1` after the VM round trip
passes. What export proves offline, and what remains for that round trip, is
listed in the header of `os/dev/internal-bundle.py`.

## Kernel buildroot snapshot credentials

`build-buildroots.yml` authenticates, signs and lands its pins exactly
like `build-rpms.yml` and `build-kernel.yml`; it needs no extra secret or
variable beyond theirs and `TILO_NATIVE_ARM64_RUNNER=true`:

- The job token (`secrets.GITHUB_TOKEN`, logged in with
  `docker/login-action`) pushes `tilo/tilo-kernel-buildroot` to
  `registry.tilo.so`; the mirrored base is anonymously readable there.
- `COSIGN_KEY_REF` (a KMS/HSM URI) and `AWS_SIGNING_ROLE_ARN` sign each native
  buildroot through `os/dev/configure-cosign-kms.sh` and Forgejo OIDC. A
  preflight job refuses to start the multi-hour builds unless the KMS public
  key equals `keys/cosign.pub`.
- The job token pushes an `automation/kernel-buildroot-snapshot-<digest>` branch
  and `RELEASE_TOKEN` opens the protected-main pull request through
  `os/dev/open-snapshot-pr.py`.

Each architecture builds on its native runner with `no-cache`, publishes by
digest, and must carry a matching `/usr/lib/tilo/kernel-buildroot-arch` marker,
a recorded package list, and no remaining yum repositories. Both signatures
are verified with `keys/cosign.pub` before either reference is recorded, and
promotion refuses to proceed if any snapshot input changed on main while the
builds ran. Do not configure `COSIGN_PRIVATE_KEY` or `COSIGN_PASSWORD`.

## Package indexes

`publish-homebrew-tap.sh` updates the pre-created `tilo-os/homebrew-tilo`, renders
the formula and cask from release digests, audits them, and pushes `main`. Configure
the broker's `homebrew` exchange as described above. The workflow accepts no
repository token fallback.

WinGet's public consumer path is the Microsoft community index. A self-hosted
source is a separate HTTPS REST service that every user must add as
administrator, so Tilo does not make it the default. `publish-winget.sh` creates
the release bot's GitHub fork when needed and opens the one-version PR expected
by `microsoft/winget-pkgs`. Configure:

- the broker's `winget` exchange with the dedicated bot's expiring GitHub App
  user token described above; the workflow accepts no repository token fallback;
- repository variable `WINGET_GITHUB_USER`: that bot account's login.

The `Release / imager WinGet acceptance` workflow runs every six hours and can also be
dispatched manually. It reads the release already published by the landing
page, refuses to select another version, and uses
`IMAGER_SITE_CREDENTIAL_BROKER` to advance `submitted` to `available` only
after Microsoft's public index serves the exact identifier and version.

After first publication users install with:

```text
brew tap tilo/tilo https://github.com/tilo-os/homebrew-tilo.git
brew install tilo-imager
winget install Tilo.Imager
```
