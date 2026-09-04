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
and publishes a digest-bound catalog. Both installer images and the catalog
receive detached OpenPGP signatures from the independently pinned Linux release
identity, so direct downloads can be authenticated as well as checksum-checked.
The catalog is also embedded into every signed Imager package. Selecting its qOS
release downloads the compressed image into a per-user cache and verifies both
the transport SHA-256 and expanded-image SHA-256/size before the raw image is
offered to the writer.

Every package launches the wizard as the signed-in user. Only after the exact
external device name is typed does it request polkit, macOS administrator, or
Windows UAC approval for a short-lived helper mode. The helper receives a
digest-bound, secret-free request, repeats the device identity and safety
checks, writes and verifies the medium, then exits. Account and disk-encryption
secrets never cross that privilege boundary.

Flatpak is deliberately not offered. A sandboxed application cannot safely gain
raw block-device access without a separately installed, narrowly scoped host
helper, and elevating a second copy of the whole Flatpak application would both
defeat the sandbox and fail for normal per-user installs. The signed AppImage is
the preferred self-contained Linux download.

Native release packages are fail-closed with `QOS_REQUIRE_NATIVE_PACKAGES=1`.
Linux release builders need GnuPG; CI materializes checksum-pinned
`linuxdeploy`, `appimagetool`, and type-2 runtime binaries from its per-arch
cache. A manual release build must provide those tools plus
`QOS_APPIMAGE_RUNTIME`; implicit “latest” runtime downloads fail closed. Windows
builders need Inno Setup 6 and the WinGet client for manifest validation.
macOS uses the platform `hdiutil`.

An `imager-v*` release tag sets `QOS_REQUIRE_SIGNING=1`, so packaging fails
closed unless the platform release credentials are configured:

- Linux steady state: `IMAGER_LINUX_CREDENTIAL_BROKER` names the native OIDC
  credential broker. It installs the public verification
  key and exports `QOS_LINUX_SIGNER` plus `QOS_LINUX_VERIFY_KEY`; every tarball
  and AppImage receives an armored detached signature that is immediately
  verified. The protected repository variable
  `IMAGER_LINUX_SIGNING_FINGERPRINT` must contain that key's full OpenPGP
  fingerprint. Packaging refuses a different broker/imported key, exports the
  exact public key as `qos-release-key.asc`, and publishes its fingerprint and
  SHA-256 sidecars beside the artifacts. The final publication job imports only
  that exported key into a fresh keyring and re-verifies every Linux signature
  against the protected fingerprint. Bootstrap-only fallback:
  `IMAGER_LINUX_SIGNING_KEY_B64` (armored
  private key, base64 encoded) and `IMAGER_LINUX_SIGNING_KEY_ID`, imported by CI
  as `QOS_LINUX_SIGNING_KEY`.
- Windows steady state: `IMAGER_WINDOWS_CREDENTIAL_BROKER` configures a signer
  wrapper, or repository variable `IMAGER_WINDOWS_SIGNER` directly names a
  preinstalled wrapper which performs its own OIDC exchange. It receives the
  artifact path, uses the job's federated short-lived signing lease, and returns
  only after timestamping.
  `signtool verify` independently checks its result. Bootstrap-only fallback:
  `IMAGER_WINDOWS_CERT_PFX_B64` and `IMAGER_WINDOWS_CERT_PASSWORD`.
- macOS steady state: `IMAGER_MACOS_CREDENTIAL_BROKER` leases the Developer ID
  signing identity and a notarytool keychain profile. Bootstrap-only fallback:
  `IMAGER_MAC_CERT_P12_B64`, `IMAGER_MAC_CERT_PASSWORD`,
  `IMAGER_MAC_SIGNING_IDENTITY`, `IMAGER_APPLE_ID`,
  `IMAGER_APPLE_TEAM_ID`, and `IMAGER_APPLE_APP_PASSWORD`. The app is signed
  with the hardened runtime; both the app and signed DMG are notarized and
  stapled before publication.

The bootstrap credentials are protected Forgejo secrets. Native and promotion
broker paths are separate protected repository variables because absolute paths
are platform-specific; public-key identities are protected variables as well.
Private signing material and OIDC leases must never enter the repository,
workflow environment files, or ordinary artifacts.

Linux users should download `qos-release-key.asc` and
`qos-release-key.fingerprint`, compare the complete fingerprint through an
independent qOS-controlled channel, and then verify a download, for example:

```sh
gpg --import qos-release-key.asc
gpg --fingerprint "$(tr -d '[:space:]' < qos-release-key.fingerprint)"
sha256sum --check qos-imager-linux-x86_64.AppImage.sha256
gpg --verify qos-imager-linux-x86_64.AppImage.asc \
  qos-imager-linux-x86_64.AppImage
```

The release attachment is the convenient key transport, not by itself the
trust anchor: the full fingerprint must also be pinned on the qOS website or
another independently authenticated channel before the first release.

Publishing to Homebrew and WinGet is a separate credentialed promotion after
the signed release exists. `publish-homebrew-tap.sh` creates/updates the
first-party `qos/homebrew-qos` tap, replaces the version, immutable base URL and
release SHA-256 values, and runs strict audits. `publish-winget.sh` submits the
generated three-file manifest to Microsoft's public community index. See
`docs/imager-release-operations.md` for runner labels, protected variables and
the Forgejo OIDC design for short-lived signing access.

The landing-page manifest records package-channel state separately from signed
download availability. Homebrew is marked available only after its native
install matrix passes. A newly opened WinGet submission is marked `submitted`,
so the site recommends the signed setup EXE instead of showing a command that
cannot resolve yet. Re-running the idempotent site promotion after Microsoft
accepts the exact version detects the public manifest and changes WinGet to
`available`.

Forgejo releases remain drafts while artifacts are uploaded. Publication reads
every attachment back and compares its exact bytes, then makes the complete
release public in one final API update. A failed upload therefore leaves a
recoverable private draft; an already-public release is immutable and cannot be
filled in or changed by a rerun.
