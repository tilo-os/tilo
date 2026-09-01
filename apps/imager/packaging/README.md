# qOS Imager host packaging

`package-host.sh` builds Linux or macOS on that native host.
`package-host.ps1` builds Windows. Every package receives a SHA-256 sidecar.
Unsigned builds are suitable for CI validation only.

An `imager-v*` release tag sets `QOS_REQUIRE_SIGNING=1`, so packaging fails
closed unless the platform release credentials are configured:

- Linux: `IMAGER_LINUX_SIGNING_KEY_B64` (armored private key, base64 encoded)
  and `IMAGER_LINUX_SIGNING_KEY_ID`. The output includes an armored detached
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
