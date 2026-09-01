# qOS Imager host packaging

`package-host.sh` builds Linux or macOS on that native host.
`package-host.ps1` builds Windows. Every package receives a SHA-256 sidecar.
Unsigned builds are suitable for CI validation only.

Release hosts produce this matrix (architecture is part of every filename):

| Host | Native downloads | Package-manager publication |
|---|---|---|
| Linux x86-64 + ARM64 | AppImage, Flatpak bundle, portable tarball | Homebrew tap formula |
| Windows x86-64 | signed standalone EXE, modern Inno Setup EXE, portable zip | WinGet manifest |
| macOS x86-64 + Apple silicon | signed/notarized DMG and app zip | Homebrew cask |

The Flatpak deliberately has a conspicuous `device=all` permission and uses a
host-side polkit hand-off: an image writer cannot both access arbitrary raw USB
block devices and pretend to be conventionally sandboxed. The AppImage is the
preferred Linux download; Flatpak exists for people who want a verifiable
single-file Flatpak bundle and accept that permission.

Native release packages are fail-closed with `QOS_REQUIRE_NATIVE_PACKAGES=1`.
Linux release builders need `linuxdeploy`, `appimagetool`, `flatpak-builder`
and the Freedesktop 25.08 SDK/runtime. Windows builders need Inno Setup 6.
macOS uses the platform `hdiutil`.

An `imager-v*` release tag sets `QOS_REQUIRE_SIGNING=1`, so packaging fails
closed unless the platform release credentials are configured:

- Linux: `IMAGER_LINUX_SIGNING_KEY_B64` (armored private key, base64 encoded)
  and `IMAGER_LINUX_SIGNING_KEY_ID`, imported by CI as
  `QOS_LINUX_SIGNING_KEY`. The portable tarball includes an armored detached
  signature.
- Windows: `IMAGER_WINDOWS_CERT_PFX_B64` and
  `IMAGER_WINDOWS_CERT_PASSWORD`. The executable is Authenticode-signed and
  verified before it is zipped.
- macOS: `IMAGER_MAC_CERT_P12_B64`, `IMAGER_MAC_CERT_PASSWORD`,
  `IMAGER_MAC_SIGNING_IDENTITY`, `IMAGER_APPLE_ID`,
  `IMAGER_APPLE_TEAM_ID`, and `IMAGER_APPLE_APP_PASSWORD`. The app is signed
  with the hardened runtime, notarized, stapled, and then zipped.

All are protected Forgejo secrets. Never place signing material in the
repository or in an ordinary workflow artifact.

Publishing to Homebrew and WinGet is intentionally a separate credentialed
promotion after the signed release exists. Packaging emits the WinGet
installer manifest when `QOS_VERSION` and `QOS_RELEASE_BASE_URL` are set; the
promotion job submits that immutable URL and digest to the package indexes.
The `homebrew/*.rb.in` files are the tap templates: promotion replaces only the
version, immutable base URL and four release SHA-256 placeholders, runs
`brew audit --strict`, and commits the resulting formula/cask to the qOS tap.
