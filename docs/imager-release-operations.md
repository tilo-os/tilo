# qOS Imager release operations

The release workflow builds only on native, repository-scoped runners and
publishes only from protected `imager-v*` tags. Configure these runner labels:

| Label | Host | Required tooling |
|---|---|---|
| `runner` | Linux x86-64 | Rust, GPG or KMS client, Podman, QEMU tools, zstd, `linuxdeploy`, `appimagetool`, Homebrew, Ruby, curl, jq, Node.js and npm |
| `runner-arm64` | Linux ARM64 | same Linux toolchain and release utilities, running natively |
| `windows` | Windows x86-64 | Rust MSVC, Windows SDK `signtool`, Inno Setup 6, WinGet client |
| `macos-x64` | Intel macOS | Xcode CLI tools, Rust, `hdiutil`, `notarytool`, Homebrew |
| `macos-arm64` | Apple silicon | same macOS toolchain, running natively, Homebrew |

Register release runners at repository scope and provision them as ephemeral
one-job machines. On Forgejo Runner that means registration with `--ephemeral`
and execution with `forgejo-runner one-job`. Do not expose native signing
runners to pull-request workflows; only protected tags may reach them.

## Short-lived credentials

The native release jobs already request `id-token: write`. Enable Forgejo
Actions OIDC on the instance and configure the external secret broker with
these minimum claim checks:

- issuer: `https://code.sandbox.example/api/actions`
- audience: a dedicated value such as `qos-release-signing`
- subject: the exact qOS repository and `refs/tags/imager-v*`
- `ref_protected == true`, plus the exact release workflow identity

Install one root-owned broker client at the same absolute path on each release
runner and set repository variable `IMAGER_CREDENTIAL_BROKER` to that path. The
release workflow invokes it before any build as
`BROKER <linux|windows|macos> OUTPUT_ENV`. The broker receives
`ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, requests the
configured audience, validates its server-side claims, and exchanges the JWT
for a 5–15 minute signing lease:

```sh
oidc_token="$(curl --fail --silent --show-error \
  -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=qos-release-signing" | jq -er .value)"
# Exchange this JWT with the signing service. Never write the JWT or returned
# credential to OUTPUT_ENV or an artifact.
```

The broker configures the native signer and writes only these non-secret handles
to `OUTPUT_ENV`:

- Linux: `QOS_LINUX_SIGNER=/absolute/root-owned/wrapper` and
  `QOS_LINUX_VERIFY_KEY=<public-key fingerprint>`; the broker imports only the
  public verification key into the job keyring. The wrapper receives input and
  output-signature paths and emits an armored detached OpenPGP signature.
- Windows: `QOS_WINDOWS_SIGNER=C:\absolute\root-owned\wrapper.exe`; it receives
  one artifact path and returns after Authenticode signing and timestamping.
- macOS: `QOS_MAC_SIGNING_IDENTITY=<Developer ID Application identity>` and
  `QOS_NOTARY_KEYCHAIN_PROFILE=<temporary notarytool profile>` after leasing the
  certificate into a temporary keychain and storing a scoped App Store Connect
  API credential in that profile.
- Homebrew: for the `homebrew` platform, write a 5–15 minute Forgejo token to
  the output path. Scope it to repository creation in `qos` and writes to
  `qos/homebrew-qos` only.
- WinGet: for the `winget` platform, use a GitHub App installation belonging to
  the submission bot and write its 5–15 minute installation token to the output
  path. Scope the installation to the bot's `winget-pkgs` fork; public-index PR
  creation additionally needs pull-request write access on
  `microsoft/winget-pkgs`.
- Site: for the `site` platform, write a 5–15 minute Forgejo token to the output
  path. Scope it to pushing `qos/website` only. After all
  native and package-index gates pass, the workflow updates only
  `src/data/imager-release.json`, runs the site's release contract and full
  production build, and pushes that exact-tag change to `main`.

The workflow rejects every other output key. The underlying code-signing
identities are not all ephemeral, but their private material never needs to be
a Forgejo secret:

- Linux: use a KMS/HSM-backed OpenPGP key, or have the broker issue a short
  signing lease. The packaging job verifies each returned signature against the
  configured public-key fingerprint before upload.
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
the broker is absent. Remove the switch after broker validation.

## Package indexes

`publish-homebrew-tap.sh` creates `qos/homebrew-qos` when needed, renders the
formula and cask from release digests, audits them, and pushes `main`. Configure
the broker's `homebrew` exchange to mint a short-lived repository-create/write
token limited to the `qos` organization. `HOMEBREW_TAP_TOKEN` is accepted only
when the explicit static-bootstrap switch is enabled.

WinGet's public consumer path is the Microsoft community index. A self-hosted
source is a separate HTTPS REST service that every user must add as
administrator, so qOS does not make it the default. `publish-winget.sh` creates
the release bot's GitHub fork when needed and opens the one-version PR expected
by `microsoft/winget-pkgs`. Configure:

- the broker's `winget` exchange with a GitHub App installation that can write
  the bot's `winget-pkgs` fork and open pull requests. The installation token is
  minted per release and expires within 15 minutes. `WINGET_GITHUB_TOKEN` is a
  static-bootstrap-only fallback;
- repository variable `WINGET_GITHUB_USER`: that bot account's login.

After first publication users install with:

```text
brew tap qos/qos https://code.sandbox.example/qos/homebrew-qos.git
brew install qos-imager
winget install qOS.Imager
```
