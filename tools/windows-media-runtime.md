# Shared native Windows media runtime

`install-windows-apps.ps1` inspects the actual PE imports of the binaries it
builds. When any uses GStreamer, it invokes `install-windows-gstreamer.ps1`
once. All apps use the same DLLs beside their executables, plugins under
`lib/gstreamer-1.0`, and scanner under `libexec/gstreamer-1.0`. Start shortcuts
still point directly to ordinary application `.exe` files. No wrapper, global
PATH change, registry setting or app-specific GStreamer environment is needed.

The bundler requires LLVM `llvm-readobj` and a trusted MSYS2 source installation.
On native ARM64 its default is `%USERPROFILE%/msys64/clangarm64`; x64 defaults
to `ucrt64`. Pass `-GStreamerPrefix` to the app installer or `-SourcePrefix` to
the bundler for another location. The tested source set is core/base/good
1.28.6-1 and bad/libav 1.28.6-2, including FFmpeg 9.0.1-3, on ARM64.

Install missing development/runtime packages using the matching MSYS2 shell:

```sh
pacman -S --needed mingw-w64-clang-aarch64-gstreamer \
  mingw-w64-clang-aarch64-gst-plugins-base \
  mingw-w64-clang-aarch64-gst-plugins-good \
  mingw-w64-clang-aarch64-gst-plugins-bad \
  mingw-w64-clang-aarch64-gst-libav
```

The installer selects playback plugins and recursively follows PE imports,
including delay imports reported by LLVM. System DLLs are recorded but never
copied. GIO's GnuTLS backend is the sole explicit dynamic TLS backend. Every shipped
binary's SHA-256 and owning package version/license are recorded in
`gstreamer-runtime.json`. Missing dependencies, mixed PE architectures and
missing version-specific copyright notices fail installation. Existing identical
DLLs are left untouched so installing another app does not overwrite mapped
shared files unnecessarily.

The crypto boundary follows `os/tests/no_openssl_runtime.sh` and
`os/scripts/verify-runtime-crypto-packages.sh`: Tilo-owned TLS/signing and
optional OpenSSL adapters stay excluded, while named distribution-library
residuals are reviewed explicitly. The Windows bundle contains no GIO OpenSSL
backend or `libssl` DLL. It is **not entirely OpenSSL-free**: this MSYS2
`libgstlibav` imports FFmpeg's `avformat-63.dll`, which imports `libsrt.dll`
(package `srt` 1.5.7-1) and `libssh.dll` (package `libssh` 0.12.2-1); both
require `libcrypto-3-arm64.dll`. FFmpeg's own TLS uses GnuTLS. These are the
only permitted direct OpenSSL consumers, with exact package owners/versions
checked by `windows-gstreamer-crypto.ps1` and recorded under
`ReviewedCryptoResiduals` in the manifest. Unknown consumers and changed
versions fail before copying. The verifier also scans unmanifested DLLs and
executables for unexpected OpenSSL imports.

An existing destination containing the formerly redundant `libgioopenssl` or
`libssl` is rejected without deleting or modifying it; use a separately reviewed
clean destination. `-PlanOnly` calculates and validates the complete new closure
and manifest without writing a runtime, which is useful when disk space is low.

The current closure is about 202 MiB. See `gstreamer-licenses/README.md` for
license provenance and the distinction between a local installation and
redistributing the bundled GPL/LGPL components.

Stage and verify without building or installing apps:

```powershell
./tools/install-windows-gstreamer.ps1 -Destination "$env:TEMP/Tilo media runtime"
./tools/test-windows-gstreamer.ps1 -RuntimeDirectory "$env:TEMP/Tilo media runtime" `
  -MediaPath ./apps/video-player/tests/fixtures/h264-aac.mp4
```

Verification launches staged tools with only Windows system directories on
PATH, clears GStreamer/GIO environment overrides and uses a fresh registry.
It requires all common media factories to resolve inside the stage, rejects
blacklisted plugins, and runs raw audio/video pipelines. With the regression
fixture it also requires both H.264 and AAC decoder branches to reach EOS.
These are headless decoder checks; audible device output and visible app
playback require the separate app tests. The GnuTLS backend is packaged,
but this relocation test does not claim live HTTPS playback coverage.
