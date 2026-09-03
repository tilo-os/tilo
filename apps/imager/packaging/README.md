# qOS Imager host packaging

`package-host.sh` builds Linux or macOS on that native host.
`package-host.ps1` builds Windows. Every package receives a SHA-256 sidecar.
Unsigned builds are suitable for CI validation only.

Release hosts produce this matrix (architecture is part of every filename):

| Host | Native downloads | Package-manager publication |
|---|---|---|
| Linux x86-64 + ARM64 | signed AppImage, signed portable tarball | Homebrew tap formula |
| Windows x86-64 | signed standalone EXE, modern Inno Setup EXE, portable zip | WinGet manifest |
| macOS x86-64 + Apple silicon | signed/notarized DMG and app zip | Homebrew cask |

The protected tag workflow also builds the live installer on native x86-64 and
ARM64 runners, converts the tested qcow2 to `qos-installer-linux-ARCH.img.zst`,
and publishes a digest-bound catalog. The catalog is embedded into every signed
Imager package. Selecting its qOS release downloads the compressed image into a
per-user cache and verifies both the transport SHA-256 and expanded-image
SHA-256/size before the raw image is offered to the writer.

Flatpak is deliberately not offered. A sandboxed application cannot safely gain
raw block-device access without a separately installed, narrowly scoped host
helper, and elevating a second copy of the whole Flatpak application would both
defeat the sandbox and fail for normal per-user installs. The signed AppImage is
the preferred self-contained Linux download.

Native release packages are fail-closed with `QOS_REQUIRE_NATIVE_PACKAGES=1`.
Linux release builders need `linuxdeploy`, `appimagetool`, and GnuPG. Windows
builders need Inno Setup 6 and the WinGet client for manifest validation.
macOS uses the platform `hdiutil`.

An `imager-v*` release tag sets `QOS_REQUIRE_SIGNING=1`, so packaging fails
closed unless the platform release credentials are configured:

- Linux steady state: the OIDC credential broker installs the public verification
  key and exports `QOS_LINUX_SIGNER` plus `QOS_LINUX_VERIFY_KEY`; every tarball
  and AppImage receives an armored detached signature that is immediately
  verified. Bootstrap-only fallback: `IMAGER_LINUX_SIGNING_KEY_B64` (armored
  private key, base64 encoded) and `IMAGER_LINUX_SIGNING_KEY_ID`, imported by CI
  as `QOS_LINUX_SIGNING_KEY`.
- Windows steady state: repository variable `IMAGER_WINDOWS_SIGNER` names a
  preinstalled signer wrapper. It receives the artifact path, uses the job's
  federated short-lived signing lease, and returns only after timestamping.
  `signtool verify` independently checks its result. Bootstrap-only fallback:
  `IMAGER_WINDOWS_CERT_PFX_B64` and `IMAGER_WINDOWS_CERT_PASSWORD`.
- macOS steady state: the OIDC credential broker leases the Developer ID signing
  identity and a notarytool keychain profile. Bootstrap-only fallback:
  `IMAGER_MAC_CERT_P12_B64`, `IMAGER_MAC_CERT_PASSWORD`,
  `IMAGER_MAC_SIGNING_IDENTITY`, `IMAGER_APPLE_ID`,
  `IMAGER_APPLE_TEAM_ID`, and `IMAGER_APPLE_APP_PASSWORD`. The app is signed
  with the hardened runtime; both the app and signed DMG are notarized and
  stapled before publication.

The bootstrap credentials are protected Forgejo secrets. Broker paths and
public-key identities are repository variables; private signing material and
OIDC leases must never enter the repository, workflow environment files, or
ordinary artifacts.

Publishing to Homebrew and WinGet is a separate credentialed promotion after
the signed release exists. `publish-homebrew-tap.sh` creates/updates the
first-party `qos/homebrew-qos` tap, replaces the version, immutable base URL and
release SHA-256 values, and runs strict audits. `publish-winget.sh` submits the
generated three-file manifest to Microsoft's public community index. See
`docs/imager-release-operations.md` for runner labels, protected variables and
the Forgejo OIDC design for short-lived signing access.
