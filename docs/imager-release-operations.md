# qOS Imager release operations

The release workflow builds only on native, repository-scoped runners and
publishes only from protected `imager-v*` tags. Configure these runner labels:

| Label | Host | Required tooling |
|---|---|---|
| `runner` | Linux x86-64 | Rust, GPG or KMS client, AppImage tools, Flatpak 1.16+, Ruby, curl |
| `runner-arm64` | Linux ARM64 | same Linux toolchain, running natively |
| `windows` | Windows x86-64 | Rust MSVC, Windows SDK `signtool`, Inno Setup 6 |
| `macos-x64` | Intel macOS | Xcode CLI tools, Rust, `hdiutil`, `notarytool` |
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

The job receives `ACTIONS_ID_TOKEN_REQUEST_URL` and
`ACTIONS_ID_TOKEN_REQUEST_TOKEN`. Its first release step should request the
configured audience and exchange the returned JWT for a 5–15 minute
broker/KMS lease:

```sh
oidc_token="$(curl --fail --silent --show-error \
  -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=qos-release-signing" | jq -er .value)"
# Pass this JWT to the AWS/GCP/Azure federation or signing-broker
# exchange. Never write the JWT or returned credential to an artifact.
```

The underlying code-signing identities are not all ephemeral, but their
private material never needs to be a Forgejo secret:

- Linux: use a KMS/HSM-backed OpenPGP key, or have the broker issue a short
  signing lease. A Sigstore `sign-blob` bundle can be added when the Fulcio
  deployment trusts the Forgejo OIDC issuer.
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

Until the broker integration is enabled, the workflow accepts the protected
`IMAGER_*` secrets documented in the packaging README. Treat that as a
bootstrap configuration, not the desired steady state.

## Package indexes

`publish-homebrew-tap.sh` creates `qos/homebrew-qos` when needed, renders the
formula and cask from release digests, audits them, and pushes `main`. Give its
protected `HOMEBREW_TAP_TOKEN` repository-create/write access only within the
`qos` organization.

WinGet's public consumer path is the Microsoft community index. A self-hosted
source is a separate HTTPS REST service that every user must add as
administrator, so qOS does not make it the default. `publish-winget.sh` creates
the release bot's GitHub fork when needed and opens the one-version PR expected
by `microsoft/winget-pkgs`. Configure:

- protected secret `WINGET_GITHUB_TOKEN`: fine-grained GitHub token able to
  write the bot's `winget-pkgs` fork and open pull requests;
- repository variable `WINGET_GITHUB_USER`: that bot account's login.

After first publication users install with:

```text
brew tap qos/qos https://code.sandbox.example/qos/homebrew-qos.git
brew install qos-imager
winget install qOS.Imager
```
