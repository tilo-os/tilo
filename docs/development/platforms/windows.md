# Windows development

Native Windows work targets hosted apps and portable libraries, not Tilo's Linux
login desktop or complete OS. A workspace-wide build includes services that are
not Windows applications. Start with [shared setup](../setup.md) and select the
Cargo package you are changing.

## Native prerequisites and focused checks

Use the pinned Rust **MSVC** toolchain, a Windows SDK, and Visual Studio C++
Build Tools for the target architecture. Windows ARM64 needs
`Microsoft.VisualStudio.Component.VC.Tools.ARM64`; the x64 tools alone do not
supply ARM64 target libraries. Use a native-architecture developer shell. SDK
builds also need Python 3.11 or newer for the verified font catalog; see
[font preparation](../../../crates/sdk/ui-core/assets/fonts/ARTIFACTS.md).

From PowerShell at the repository root, after installing the selected package's
dependencies:

```powershell
cargo check --locked -p tilo-ui --tests
cargo run --locked -p calculator
```

`tilo-ui` is the Cargo package name; `ui` is the dependency alias used in Rust
source, not the package to pass to `-p`. A successful check or headless test is
not evidence that native windowing, clipboard, accessibility or GPU behavior
works on your Windows machine.

For Imager host-media-writer work on a native ARM64 host, the focused recipes
are:

```powershell
cargo test --locked -p imager --lib --features host-media-writer --target aarch64-pc-windows-msvc
cargo test --locked -p tilo-ui --test hosted-platform --target aarch64-pc-windows-msvc
```

On native x64, substitute `x86_64-pc-windows-msvc`. Keep the target explicit so
an emulated shell does not silently select the release architecture. The
`hosted-platform` test checks refusal of unsupported local authority; it does
not qualify the Linux authority or compositor. Signed installer requirements
belong in [Imager release operations](../../imager-release-operations.md), not
ordinary app setup.

## Optional per-user installation

Use the maintained [Windows tools](../../../tools/windows/README.md) when you
need installed executables, runtime dependencies and Start entries. Read
`tools/windows/install-apps.ps1`'s parameter block for the current app
selections; that list is not a claim that every app or integration is qualified.

The installer requires PowerShell 7, the build prerequisites above, and LLVM's
`llvm-readobj.exe` on `PATH` for executable-import inspection. Run from an
ordinary, unpackaged PowerShell session: package-private AppData redirection is
rejected rather than treated as a successful public installation.

```powershell
./tools/windows/install-apps.ps1 -Apps calculator,notes
```

This **builds and installs** the selected apps; it is not a diagnostic. Release
is the default profile, with two build jobs. `-Profile dev` selects debug
iteration and `-Jobs 1` lowers concurrency. Close affected apps before updating
executables or changed shared fonts. Documents stay in their existing profiles.
The installer stages fonts and DXC, adds PDFium when needed, inspects media
imports and verifies Start visibility. Do not work around a refused install by
restarting Explorer or copying files from a packaged process's private cache.

Media apps additionally need architecture-matched GStreamer development
libraries and runtime plugins. Use the
[shared Windows media runtime guide](../../../tools/windows/media-runtime.md)
for MSYS2 prefixes, runtime closure and verification instead of maintaining
app-specific DLL-copy or global environment recipes here. Its headless decoder
checks do not establish audible playback, visible presentation or hardware
decoding.

## Linux work, development VMs and Android

For Linux checks in WSL, use a separate checkout on its Linux filesystem; see
[WSL checkout guidance](linux.md#wsl-checkouts). Do not share `target/` with
native Windows builds or set `CARGO_TARGET_DIR`. WSL alone does not qualify a
Tilo login session or physical device.

For the complete OS, use `tilo device guide quickstart` and
`tilo device guide platforms` (also available as `tilo-vm guide ...`). The
[canonical platform guide](../../../crates/dev/tilo-vm/guide/platforms.md)
covers Hyper-V and the Windows ARM64 Home QEMU/WHPX adapter, including their
actual prerequisites. Keep lifecycle, component swaps and rollback under the
device tool; do not invent an SSH/SCP deployment loop.

Android is a separate app-host target; see [Android setup](android.md).
