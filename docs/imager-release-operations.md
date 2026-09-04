# qOS Imager release operations

The release workflow builds only on native, repository-scoped runners and
publishes only from protected `imager-v*` tags. Configure these runner labels:

| Label | Host | Required tooling |
|---|---|---|
| `runner` | Linux x86-64 | Rust, Cap'n Proto compiler, GPG or KMS client, Podman, QEMU tools, zstd, Homebrew, Ruby, curl, jq, Node.js and npm, UDisks2, Flatpak + flatpak-builder with the 25.08 SDK |
| `runner-arm64` | Linux ARM64 | same Linux toolchain, release utilities, and UDisks2 service, running natively |
| `windows` | Windows x86-64 | Rust MSVC, Cap'n Proto compiler, Windows SDK `signtool`, Inno Setup 6, WinGet client |
| `macos-x64` | Intel macOS | Xcode CLI tools, Rust, Cap'n Proto compiler, `hdiutil`, `notarytool`, Homebrew |
| `macos-arm64` | Apple silicon | same macOS toolchain, running natively, Homebrew |

Use Forgejo 15 or newer and Forgejo Runner 12.5 or newer. Register every host at
**repository scope** (`qos/qos`), select ephemeral mode, expose exactly one of
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
`tools/run-imager-ephemeral-runner.sh` as a root-owned, non-writable executable
outside the checkout. Invoke that installed copy as
`/usr/local/sbin/run-imager-ephemeral-runner LABEL UUID_FILE TOKEN_FILE`.
Windows images should likewise install `tools/Run-ImagerEphemeralRunner.ps1`
outside the checkout with an ACL writable only by Administrators, then invoke
it as `powershell.exe -NoLogo -NoProfile -NonInteractive -File
C:\ProgramData\qOS\Run-ImagerEphemeralRunner.ps1 -Label windows -UuidFile
C:\run\qos-runner\uuid -TokenFile C:\run\qos-runner\token`. Both launchers
validate the native OS/architecture-to-label binding, require
Forgejo Runner 12.5 or newer, accept only private runner-owned metadata files
inside a private runner-owned directory, pin the Forgejo origin, run exactly
`one-job` with a `:host` label, bound the wait to six hours, and erase both
registration values on every exit after their paths pass safety validation. Set
`QOS_RUNNER_MAX_WAIT_SECONDS` to a smaller orchestration deadline when useful;
the accepted range is 1 second through 24 hours. The final success message is a
destruction instruction, not permission to return the VM to a pool. Run the
PowerShell launcher directly on Windows; do not use a Unix compatibility layer
as the release host.

The repository runner page is `/qos/qos/settings/actions/runners`. Forgejo's
[runner-registration guide](https://forgejo.org/docs/latest/admin/actions/registration/)
documents both the UI/API registration response and offline administrator CLI;
its [Actions security guide](https://forgejo.org/docs/latest/admin/actions/security/)
documents why repository scope, ephemeral registration, and `one-job` belong
together. Set an orchestration timeout because an ephemeral registration which
never receives a job does not expire on its own.

The native jobs also validate ordinary `main`/pull-request builds, but those
runs receive neither release secrets nor a signing lease. The broker must reject
every non-protected, non-tag OIDC identity. If stronger physical separation is
required, dedicate a second no-broker validation fleet; do not put a persistent
private key or cloud credential on either fleet.

The workflow materializes checksum-pinned `linuxdeploy`, `appimagetool`, and
type-2 runtime binaries from the Forgejo cache separately for each Linux
architecture. They do not need to be baked into the runner, and AppImage
creation is forbidden from downloading an implicit “latest” runtime.
The same golden images preinstall `org.freedesktop.Sdk//25.08` and run UDisks2
with at least the VM's system disk visible. The Flatpak build forbids downloads
during bundle assembly, smoke-loads the exported app inside the pinned runtime,
and reconciles its sandbox inventory with that host UDisks2 view before it can
become an artifact.
End-user Linux hosts need UDisks2 (standard on major desktop
distributions); its polkit prompt mediates the one exclusive removable-drive
descriptor used by the sandboxed writer.

## Short-lived credentials

The six credentialed jobs set Forgejo's native `enable-openid-connect: true`.
Do not add GitHub's `permissions: id-token: write`: Forgejo ignores that field
and emits a workflow warning. Repository capabilities come from Authorized
Integrations; the native key is what causes Forgejo to inject
`ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`. The exact
Forgejo behavior and token fields are documented in its
[OIDC guide](https://forgejo.org/docs/latest/user/actions/security-openid-connect/).

Configure the external secret broker with all of these claim checks:

- issuer: `https://code.sandbox.example/api/actions`
- audience: a dedicated value such as `qos-release-signing`
- `repository == qos/qos` and the pinned immutable `repository_id`
- `event_name == push`, `ref_type == tag`, `ref` matches only
  `refs/tags/imager-vMAJOR.MINOR.PATCH`, and `ref_protected == "true"`
- `workflow == release-imager.yml`, and `workflow_ref` belongs to `qos/qos`
- `sub` equals the instance's actual protected-tag subject. Forgejo 16 may add
  immutable numeric owner/repository IDs to this string, so capture one denied
  bootstrap token and pin the exact format instead of assuming the older
  `repo:qos/qos:ref:...` form.

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
| `IMAGER_WINDOWS_CREDENTIAL_BROKER` | Windows x86-64 | environment file described below |
| `IMAGER_MACOS_CREDENTIAL_BROKER` | both macOS architectures | environment file described below |
| `IMAGER_HOMEBREW_CREDENTIAL_BROKER` | macOS ARM64 | raw Forgejo token file |
| `IMAGER_WINGET_CREDENTIAL_BROKER` | Linux x86-64 | raw GitHub token file |
| `IMAGER_SITE_CREDENTIAL_BROKER` | Linux x86-64 | raw Forgejo token file |

Each value is an absolute executable path appropriate to that host. The
workflow invokes it as `BROKER PLATFORM OUTPUT`, where `PLATFORM` is `linux`,
`windows`, `macos`, `homebrew`, `winget`, or `site`. The broker receives
`ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, requests the
configured audience, validates its server-side claims, and exchanges the JWT
for a narrowly scoped lease. It must create `OUTPUT` with mode 0600 (or a
Windows ACL granting only the runner account) and fail if that path already
exists:

```sh
oidc_token="$(curl --fail --silent --show-error \
  -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=qos-release-signing" | jq -er .value)"
# Exchange this JWT with the signing service. Never write the JWT or returned
# credential to OUTPUT_ENV or an artifact.
```

For `linux`, `windows`, and `macos`, the broker configures the native signer and
writes only these non-secret handles to the environment-file output, one
newline-terminated `NAME=value` per line:

- Linux: `QOS_LINUX_SIGNER=/absolute/root-owned/wrapper` and
  `QOS_LINUX_VERIFY_KEY=<public-key fingerprint>`; the broker imports only the
  public verification key into the job keyring. The wrapper receives input and
  output-signature paths and emits an armored detached OpenPGP signature. Set
  protected repository variable `IMAGER_LINUX_SIGNING_FINGERPRINT` to the same
  full fingerprint. This policy value is deliberately supplied independently
  of the broker: a broker returning a different public key cannot silently
  rotate the release identity.
- Windows: `QOS_WINDOWS_SIGNER=C:\absolute\root-owned\wrapper.exe`; it receives
  one artifact path and returns after Authenticode signing and timestamping.
- macOS: `QOS_MAC_SIGNING_IDENTITY=<Developer ID Application identity>` and
  `QOS_NOTARY_KEYCHAIN_PROFILE=<temporary notarytool profile>` after leasing the
  certificate into a temporary keychain and storing a scoped App Store Connect
  API credential in that profile.
- Homebrew: for the `homebrew` platform, write a 5–15 minute Forgejo token as
  the complete raw output. Pre-create public repository `qos/homebrew-qos`, give
  the release bot write access to only that repository, and mint the token for
  that bot. Repository creation is supported by the script for bootstrap, but
  should not be part of the steady-state lease.
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
  complete raw output. Give its bot write access only to `qos/website`. After all
  native and package-index gates pass, the workflow updates only
  `src/data/imager-release.json`, runs the site's release contract and full
  production build, and pushes that exact-tag change to `main`.

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
  install a small signer wrapper on the ephemeral runner image. Set repository
  variable `IMAGER_WINDOWS_SIGNER` to its absolute path. The packaging script
  passes it one artifact path at a time and then runs `signtool verify`; the
  wrapper exchanges the OIDC token, signs by key reference and timestamps the
  file without exporting a PFX.
- macOS: the Developer ID certificate itself is long-lived. Keep it in a
  signing service or secrets manager and release a short lease to the
  ephemeral macOS runner. Prefer an App Store Connect API key for notarization;
  scope it to the qOS team and retrieve it through the same OIDC lease.

For initial bring-up only, set `IMAGER_ALLOW_STATIC_SIGNING_BOOTSTRAP=1`; the
workflow then accepts the protected `IMAGER_*` secrets documented in the
packaging README. Without that explicit switch, a release tag fails closed when
the broker is absent. Remove the switch and every bootstrap secret after one
broker-backed release succeeds. `IMAGER_WINDOWS_SIGNER` is a separate supported
steady-state path for a preinstalled Windows wrapper which performs its own
OIDC exchange; it is not a path to a PFX.

## Bring-up checklist

Before creating a release tag, verify all of the following:

- the protected tag rule matches `imager-v*`, forbids deletion/force-push, and
  restricts creation to release maintainers;
- five native labels are online at least once: `runner`, `runner-arm64`,
  `windows`, `macos-x64`, and `macos-arm64`; each reports its expected native
  architecture and `:host` execution;
- each golden image can build this repository from a clean checkout and has the
  tooling in the table above; Linux hosts can use rootful Podman for live-media
  assembly, and Windows/macOS hosts have access to their native GUI session for
  the packaged smoke launch;
- all six broker-path variables above are protected; the Linux full fingerprint
  is independently set in protected variable
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

Dispatch `Release / imager` manually on `main` with `full_native` enabled first. That run
does not sign or publish, but proves every native label and packaged UI. Only
after it is green should a maintainer create a protected semantic tag such as
`imager-v1.2.3`. A successful tag must finish the native matrix, release trust
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
| the six `IMAGER_*_CREDENTIAL_BROKER` variables listed above | absolute path on each corresponding golden image |
| `IMAGER_LINUX_SIGNING_FINGERPRINT` | uppercase 40- or 64-hex full primary-key fingerprint, independently verified |
| `IMAGER_WINDOWS_PUBLISHER` | complete expected Authenticode certificate subject, independently verified |
| `IMAGER_MAC_TEAM_ID` | 10-character Apple Developer team identifier, independently verified |
| `IMAGER_MAC_CERT_SHA256` | uppercase 64-hex SHA-256 fingerprint of the expected Developer ID leaf certificate |
| `WINGET_GITHUB_USER` | dedicated GitHub submission bot login |
| `SITE_RELEASE_USER` | Forgejo website bot login; omit to use `oauth2` |
| `IMAGER_WINDOWS_SIGNER` | optional alternative to the Windows broker; absolute OIDC-aware wrapper path |

The live-medium builder separately needs protected secret `REGISTRY_TOKEN` for
`code.sandbox.example`. Give its service account pull access to frozen inputs and
only the minimum image-write access required by `build-image.sh`; rotate it
independently of signing. The WPE buildroot snapshot workflow does **not** use
that persistent token in steady state; its broker setup is described below.
Forgejo provides the tag-release upload token as `github.token` and no
additional release-upload secret is required.

Do not configure any of the following in steady state:
`IMAGER_ALLOW_STATIC_SIGNING_BOOTSTRAP`, `IMAGER_LINUX_SIGNING_KEY_B64`,
`IMAGER_LINUX_SIGNING_KEY_ID`, `IMAGER_WINDOWS_CERT_PFX_B64`,
`IMAGER_WINDOWS_CERT_PASSWORD`, `IMAGER_MAC_CERT_P12_B64`,
`IMAGER_MAC_CERT_PASSWORD`, `IMAGER_MAC_SIGNING_IDENTITY`, `IMAGER_APPLE_ID`,
`IMAGER_APPLE_TEAM_ID`, `IMAGER_APPLE_APP_PASSWORD`, `HOMEBREW_TAP_TOKEN`,
`WINGET_GITHUB_TOKEN`, or `SITE_RELEASE_TOKEN`. They exist solely to unblock a
time-bounded first bootstrap and should be deleted immediately afterward.

## WPE buildroot snapshot credentials

`snapshot-wpe-buildroot.yml` is a separate main-branch trust boundary. Set the
protected repository variable `WPE_SNAPSHOT_CREDENTIAL_BROKER` to an absolute,
root-owned executable on both native Linux golden images. Both snapshot jobs
enable Forgejo OIDC. They invoke the broker as:

```text
BROKER resolve-x86_64 OUTPUT
BROKER resolve-aarch64 OUTPUT
BROKER publish-x86_64 OUTPUT
BROKER publish-aarch64 OUTPUT
BROKER promote OUTPUT
```

The broker must fail if `OUTPUT` exists, create it as a regular mode-0600 file
owned by the runner account, and write exactly one newline-terminated
`NAME=value` per line. Resolve and promotion leases contain
`QOS_REGISTRY_USERNAME`, `QOS_REGISTRY_PASSWORD`,
and `QOS_LEASE_EXPIRES_AT`. Publish leases also contain `QOS_WPE_SIGNER`. The
expiry is a Unix timestamp no more than 15 minutes in the future. CI acquires a
read lease to resolve the base image, builds without pushing, logs out, then
acquires a fresh write/signing lease immediately before push and signing;
a signing handle is never left to expire during the long Fedora transaction.
The helper rejects duplicate, malformed, or additional settings, uses the
registry password only for `docker login`, removes the response, and logs out
after the job. No registry password is copied to `GITHUB_ENV` or an artifact.

The helper deliberately never handles the JWT itself: it verifies that
Forgejo injected `ACTIONS_ID_TOKEN_REQUEST_URL` and
`ACTIONS_ID_TOKEN_REQUEST_TOKEN`, then invokes the trusted broker with both
still present in its environment. The broker must request the audience
`qos-wpe-buildroot-snapshot` from that URL, validate and exchange the returned
JWT server-side, and must never copy either OIDC value into `OUTPUT`, logs, or
an artifact. A broker which merely trusts the requested mode or other process
environment without validating the signed JWT is not conforming.

`QOS_WPE_SIGNER` must resolve to an executable, non-symlink, root-owned path
outside the checkout and job temporary directory. It receives one argument:
the immutable
`code.sandbox.example/qos/qos-wpe-buildroot@sha256:...` reference. The wrapper must
hold or acquire a non-exportable KMS/HSM signing lease, reject every other
repository/reference shape, sign at most once for its build lease, upload the
transparency-log entry, and return only after signing succeeds. CI immediately
verifies the result with `keys/cosign.pub`; the broker cannot rotate the trusted
identity by returning another public key.

Configure this broker with a dedicated audience such as
`qos-wpe-buildroot-snapshot` and the same JWT signature/time/replay checks
described above, but with a policy specific to this workflow:

- `repository == qos/qos` and the pinned immutable repository ID;
- `event_name` is `push` or an authorized `workflow_dispatch`, `ref_type` is
  `branch`, `ref == refs/heads/main`, and main is protected;
- `workflow == snapshot-wpe-buildroot.yml`, with `workflow_ref` in `qos/qos`;
- the requested mode is exactly one of the two `resolve-*`, two `publish-*`, or
  `promote` modes above, and `(run_id, run_attempt, mode)` has not already
  received a lease.

Give resolve-mode registry leases pull access, publish-mode leases push access,
and promotion-mode leases pull access only to `qos/qos-wpe-buildroot`. The
promotion job explicitly gives `actions/checkout` Forgejo's job-scoped
`github.token` for its atomic main-branch pin commit, so the broker does not
mint a repository PAT.
Its `contents: write` permission exists only on that promotion job; native build
jobs have `contents: read`.

For initial bring-up only, set `WPE_ALLOW_STATIC_CREDENTIAL_BOOTSTRAP=1` and
set `WPE_STATIC_BOOTSTRAP_EXPIRES_AT` to an absolute Unix timestamp no more than
24 hours away. Only while both conditions hold does CI expose protected
`REGISTRY_TOKEN`, `COSIGN_PRIVATE_KEY`, and `COSIGN_PASSWORD` to the isolated
bootstrap steps. An absent, expired, or overlong window fails closed. After the
first broker-backed snapshot, delete the flag, expiry, `COSIGN_PRIVATE_KEY`, and
`COSIGN_PASSWORD`; remove WPE snapshot write scope from the shared
`REGISTRY_TOKEN` (which other live-medium jobs may still require). Do not
refresh the expiry as a steady-state operating procedure.

## Package indexes

`publish-homebrew-tap.sh` creates `qos/homebrew-qos` when needed, renders the
formula and cask from release digests, audits them, and pushes `main`. Configure
the broker's `homebrew` exchange as described above. `HOMEBREW_TAP_TOKEN` is
accepted only when the explicit static-bootstrap switch is enabled.

WinGet's public consumer path is the Microsoft community index. A self-hosted
source is a separate HTTPS REST service that every user must add as
administrator, so qOS does not make it the default. `publish-winget.sh` creates
the release bot's GitHub fork when needed and opens the one-version PR expected
by `microsoft/winget-pkgs`. Configure:

- the broker's `winget` exchange with the dedicated bot's expiring GitHub App
  user token described above. `WINGET_GITHUB_TOKEN` is a
  static-bootstrap-only fallback;
- repository variable `WINGET_GITHUB_USER`: that bot account's login.

After first publication users install with:

```text
brew tap qos/qos https://code.sandbox.example/qos/homebrew-qos.git
brew install qos-imager
winget install qOS.Imager
```
