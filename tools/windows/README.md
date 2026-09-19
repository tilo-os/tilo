# Windows tools

This directory owns the native Windows installation and runtime-verification
toolchain. Run commands from the repository root so their relative application
and fixture paths resolve consistently.

- `install-apps.ps1` builds and installs one or more applications, their shared
  runtime dependencies, and Start menu entries.
- `check-start-apps.ps1` verifies that installed applications are visible to
  Windows rather than redirected into a packaged process cache.
- `install-dxc.ps1`, `install-pdfium.ps1`, and `install-gstreamer.ps1` stage the
  native runtime closures used by the applications.
- `test-installed-media.ps1` and `test-gstreamer.ps1` verify the installed media
  stack without inheriting a development environment.

See [media-runtime.md](media-runtime.md) for the GStreamer closure, integrity,
and licensing contract.
