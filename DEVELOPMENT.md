# Development Guide

Build dependencies, local development and testing. See `AGENTS.md` for repository
conventions.

## What the build needs

Install Rust through rustup and the native dependencies below, either on the host
or in your build container or Nix shell. Rustup reads `rust-toolchain.toml`.
Check dependencies with:

```bash
tools/check-build-env.sh
```

Modules, and the Fedora packages providing them. Note `xkbcommon-devel`,
`libudev-devel` and `ffmpeg-devel` do not exist:

| pkg-config | Fedora package | for |
|---|---|---|
| `xkbcommon` | `libxkbcommon-devel` | every build |
| `libseat` `libinput` `libudev` | `libseat-devel` `libinput-devel` `systemd-devel` | compositor DRM backend, input, device enumeration |
| `libdrm` `gbm` `gl` `egl` | `libdrm-devel` `mesa-libgbm-devel` `mesa-libGL-devel` `mesa-libEGL-devel` | compositor DRM and paint |
| `wayland-client` | `wayland-devel` | |
| `libpipewire-0.3` | `pipewire-devel` | portal ScreenCast |
| `fuse3` | `fuse3-devel` | `fs` |
| `dbus-1` | `dbus-devel` | portals |
| `libavcodec` `libavformat` `libavutil` | `libavcodec-free-devel` `libavformat-free-devel` | shell's default hdr-decode feature |
| `libswscale` | `libswscale-free-devel` | FFmpeg software scaling used by the shell |
| `gstreamer-1.0` `gstreamer-app-1.0` | `gstreamer1-devel` `gstreamer1-plugins-base-devel` | media apps |
| `capnp` (binary) | `capnproto` | runtime-collab schema |
| `libclang` (shared library) | `clang-libs` | bindgen for PipeWire and FFmpeg bindings; Python 3 checks that it can load |
| `bwrap` (binary) | `bubblewrap` | the launcher's confinement tests drive the real sandbox |
| `rust-analyzer` (binary) | `rustup component add rust-analyzer` | the code editor's language-server test drives a real server |

The sandbox and language-server tests require `bwrap` and `rust-analyzer`.
Install them where Cargo runs, including inside a build container.

### Native Windows Imager builds

Install the pinned Rust toolchain, a Windows SDK, and the Visual Studio C++
tools for the target architecture. Windows ARM64 needs the Build Tools
component `Microsoft.VisualStudio.Component.VC.Tools.ARM64`; installing only
the x64 C++ tools does not provide ARM64 target libraries. Put a working
`capnp` schema compiler on PATH. For a fully native ARM64 toolchain, build it
from the [Cap'n Proto source](https://capnproto.org/install.html) with CMake's
Visual Studio generator and `-A ARM64`.

From an ARM64 host, run:

```powershell
cargo test -p imager --lib --features host-media-writer --target aarch64-pc-windows-msvc --locked
cargo test -p ui --test hosted-platform --target aarch64-pc-windows-msvc --locked
```

Use `x86_64-pc-windows-msvc` on an x64 host. Keep the target explicit: an
emulated shell or tool must not determine the release architecture. These
tests cover the hosted client; the compositor and local identity authority
remain Linux/Unix services. Signed installer prerequisites and both native
release lanes are documented in [Imager release operations](docs/imager-release-operations.md).

### Video editor on macOS

Install the native media dependencies with Homebrew, then launch from the
workspace root:

```bash
brew install pkgconf gstreamer
cargo run -p video_editor
```

The editor enables GStreamer playback and export by default. Homebrew's
`gstreamer` formula supplies the plugins and GLib dependency as well. Check
that Cargo can discover the libraries with:

```bash
pkg-config --modversion glib-2.0 gstreamer-1.0 gstreamer-app-1.0
```

### Native Windows development

Use the pinned Rust MSVC toolchain, Visual Studio C++ build tools for the host
architecture, and the Cap'n Proto compiler (`capnp.exe` on `PATH`). The native
ARM64 toolkit, Calculator, Notes, Calendar, Contacts, Sheets, Slides, Weather and
Scratchpad, Files, Word Processor, Terminal, PDF Viewer, Books, Clocks, Image Editor,
Photos, Hex Editor, Code Editor, API Tester, Diff and Merge, Snippets, Database
Client, Git Client and Feeds have been built and run on Windows.
Other apps are still being ported; a workspace-wide build currently includes
Linux services and is not a Windows
application build command.

From PowerShell at the workspace root:

```powershell
capnp --version
cargo check --locked -p ui --tests --features portal
cargo run --locked -p calculator
cargo run --locked -p notes
```

Windows rendering defaults to Direct3D 12. `WGPU_BACKEND` remains an explicit
developer override. Apps use the shared `portal` feature for native Open, Save,
multiple-file and folder dialogs, with results delivered on the existing picker
channel. To exercise the host picker without opening or modifying a document:

```powershell
cargo run --locked -p ui --example native-file-picker --features portal -- open
# Also accepts: multiple, directory, save
cargo test --locked -p ui --lib --features portal portal::
```

The example reports the selected path; even its Save mode does not write a file.
The Windows installer also copies the versioned font catalog and licenses beside
the executables. Native renderers load and validate that catalog once per process,
then share its font data. Cargo-run binaries use the checked-in catalog instead.

Passwords is available as an explicit Windows preview with
`./tools/install-windows-apps.ps1 -Apps passwords`. It is not yet included in the
default verified-app set: native backup/import/export and failure reporting are
verified, but successful Windows clipboard copying still needs verification.
The automated session currently receives an access-denied error from Windows.
Mail is available as an explicit preview with
`./tools/install-windows-apps.ps1 -Apps mail`. Native empty-state setup, protected
password fields, responsive provider discovery, and cancellation of stalled
credential verification are verified. Configured-account reading and sending
still need native end-to-end verification, so Mail remains outside the default set.
Typst is also available as an explicit preview with
`./tools/install-windows-apps.ps1 -Apps typst-editor`; native source editing,
Save/Open, and PDF export have been verified, including reduced-motion settings
and emoji export through the shared font catalog. It remains outside the
default set while startup measurements and Start integration checks finish. The
installed optimized binary has passed those native file and PDF workflows.

The installer verifies the real Windows Start catalog and reports an error if
Windows has not indexed the shortcut, even when the executable was copied.

Install the currently verified native apps into your Windows user profile, with
Start menu entries under **Tilo**:

```powershell
./tools/install-windows-apps.ps1
# Or select applications (optimized release builds are the default):
./tools/install-windows-apps.ps1 -Apps calculator,notes
# Use dev explicitly when debugging, rather than for everyday installed apps:
./tools/install-windows-apps.ps1 -Apps calculator -Profile dev
```

This builds before copying executables to `%LOCALAPPDATA%\Tilo\Applications`.
The installer limits Cargo to two concurrent jobs to bound memory use during
release linking. Use `-Jobs 1` on a memory-constrained machine; `-Jobs` also
applies to the Windows notification helper build.
Unoptimized builds spend substantially longer validating and loading fonts at
startup. The installer includes Microsoft's pinned DXC 1.8.2505.1 compiler and
its MIT/LLVM notices beside the apps. Without DXC, wgpu's FXC fallback spends
seconds compiling UI shaders on every launch. `install-windows-dxc.ps1` can
update this shared runtime independently of app builds; it verifies the
release archive against Microsoft's published SHA-256 digest. The separate
DXIL validator and command-line compiler are not installed.

The standard Windows app launcher prepares the GPU and its shaders in parallel with
app state and font construction, then attaches the real window after checking
the selected adapter supports its surface. The primary window appears only
once the renderer is ready. Linux keeps its existing first-redraw initialization.
`TILO_SERIAL_RENDER_INIT=1` disables the Windows overlap for an A/B comparison;
`WGPU_DX12_COMPILER=fxc` explicitly selects the old compiler for diagnostics.
Both controls are optional: installed apps use DXC and parallel initialization
without environment configuration. To measure startup,
set `TILO_STARTUP_METRICS_PATH` to a writable JSON filename before launching an
app; it records font setup, GPU initialization, and the first presented frame.
Close an installed app before updating its executable. Application documents
remain in their normal user data locations; the installer does not move them.
The installer also checks both Windows' application catalog and `Get-StartApps`
against the installed executable paths and intended app names. A shortcut on disk alone is insufficient:
Explorer can retain a stale catalog. If this check fails after copying succeeds,
restart Windows Explorer when no file operations are running, or sign out and
back in, then run `./tools/check-windows-start-apps.ps1 -Apps hex-editor` (substitute
the affected package names). The installer does not restart the desktop shell.

Private identity files and local account-vault keys use the shared `private-fs`
crate. On Windows, files are created with a protected, current-user-only DACL;
reads validate the opened file's owner, access rules and reparse status. Existing
keys with public access are rejected rather than silently replaced. Concurrent
vault creators publish one complete key without overwriting the winner. The
shared Accounts vault records failed loads and refuses writes until access is
restored and the vault reopened. It also revalidates existing ciphertext before
saving and refuses to generate a replacement key for an existing vault. The
Passwords UI exposes recovery errors to both sighted and assistive users.
Passwords explicitly enables the shared native file-picker feature. Backup and
restore passphrase fields use protected semantic input, and transfer polling
keeps a stable wake deadline until the next sync. File-dialog completion and
revealed-password expiry therefore do not need another input event. Passwords
uses `try_clipboard_copy` for copy actions: an installed host backend's refusal
is final, and a failed system write reports an accessible error without keeping
a secret in the editor-local fallback buffer. Native
transfer verification covers Unicode paths, acknowledged CSV export/import,
encrypted backup, wrong-passphrase rejection, and fresh-profile restoration.

```powershell
cargo test --locked -p private-fs -p tilo-user-identity -p accounts --lib
```

Apps that need persistent content-addressed blobs without mounting a filesystem
can depend on `fs` with `default-features = false`, as Scratchpad does. This uses
the same content store, durable protection tags and legacy-store migration as
the OS. The default `filesystem` feature adds the namespace, FUSE projection,
service protocols and their integration tests.

```powershell
cargo test --locked -p fs --no-default-features --lib
cargo test --locked -p scratchpad --all-targets
```

Files supports native local-file browsing and operations, including drive and UNC
paths. Its Start menu entry is **Files** and its Cargo package is `file-explorer`.
The Tilo filesystem projection and received-share service transport are not yet
available on native Windows; their control operations return errors. Delete uses
the Windows recycle bin, but in-app recycle-bin browsing and per-item restoration
are not implemented. Projection-dependent screenshot scenarios also fail explicitly.

Run File Explorer tests serially because its fixture guard observes process-wide
filesystem access:

```powershell
cargo test --locked -p file-explorer --lib -- --test-threads=1
cargo test --locked -p file-explorer --test it -- --test-threads=1
```

Word Processor uses `%LOCALAPPDATA%/tilo/word-processor/recovery` for crash recovery
unless `TILO_WP_RECOVERY_DIR` is explicitly set. Its recovery key stays stable
when a new file is first saved and Windows starts returning a canonical verbatim
path. Native tests cover editing, DOCX/ODT/HTML round trips, PDF/RTF/EPUB export,
recovery cleanup and merging independently edited documents:

```powershell
cargo test --locked -p word-processor --all-targets -- --test-threads=1
```

Terminal uses Windows ConPTY and starts `COMSPEC` (normally `cmd.exe`). It supports
the same terminal action API, live output, resizing and exit-state reporting as
the Unix implementation. Closing the window terminates its native shell. Hosted
Tilo process-service grants are still unavailable on Windows and fail closed.
Windows drag selection stays in an app-local primary buffer; explicit Copy/Paste
uses the native system clipboard. `--shots --scenario shell` executes native
commands and fails if its completion output is not observed.

```powershell
cargo test --locked -p terminal --all-targets -- --test-threads=1
```

PDF Viewer and Files use a shared PDFium DLL for page rendering and PDF thumbnails.
The app installer downloads the pinned `chromium/8057` Windows runtime, verifies
its published SHA-256 digest, and installs `pdfium.dll` beside the executables in
`%LOCALAPPDATA%/Tilo/Applications`, including its licenses. To install the runtime
separately or reuse an already downloaded archive:

```powershell
./tools/install-windows-pdfium.ps1
./tools/install-windows-pdfium.ps1 -ArchivePath C:/Downloads/pdfium-win-arm64.tgz
$env:TILO_PDFIUM_LIB = "$env:LOCALAPPDATA/Tilo/Applications/pdfium.dll"
$env:TILO_REQUIRE_PDFIUM = '1'
cargo test --locked -p pdf-viewer --all-targets -- --test-threads=1
```

The library override is for development binaries outside the installation directory;
installed apps discover their adjacent DLL automatically. Requiring PDFium during
tests prevents a missing renderer from leaving backend tests unverified.
PDF Viewer captures wait for the production worker's visible pages and thumbnails;
unfinished rendering fails after 15 seconds. The explicit `loading` scenario keeps
its intentional placeholder. `cargo test --locked -p pdf-viewer --test capture`
exercises this worker path separately from the synchronous library tests.

Books opens EPUBs through its background importer and wakes idle windows when an
import completes. Device-local source paths and watched folders stay outside the
collaborative document. `EBOOK_LIBRARY_DIR` uses Windows `;` path-list separators;
drive letters are preserved. Windows imports remain local unless an absolute
`TILO_WORKSPACE_DIR` explicitly identifies a configured collaborative workspace.
The installer does not provide that workspace service. Books captures accept an
EPUB positional argument and wait for its import to finish.

Image Editor is installed as **Image Editor** (`image-editor.exe`). It opens
PNG, JPEG and WebP through the shared Windows file picker or `--open PATH`.
Adjustments remain editable in the persisted project; Export writes
`<source-name>-edited.png` beside the source without changing the original.
The native verification opens a Unicode path, invokes semantic rotate,
monochrome and export actions, checks the exported dimensions and verifies
that the project survives a new process. Its offscreen scenarios support
`TILO_SHOTS_THEME=dark|light|high-contrast`.

```powershell
cargo test --locked -p image-editor --all-targets
./tools/install-windows-apps.ps1 -Apps image-editor
```

Photos uses the native file picker and keeps imported image bytes in the shared
filesystem blob store. On Windows the default is `%LOCALAPPDATA%/tilo/blobs`,
with `USERPROFILE/AppData/Local` as the native fallback; `TILO_BLOB_STORE` and
`XDG_CACHE_HOME` retain their explicit override behavior. The document stores
content hashes and edits, so both its document and this store must survive a
restart. Live startup and headless automation install the durable store before
opening images; screenshot fixtures stay in memory. Completed thumbnail jobs
wake the host so the library finishes loading without pointer or keyboard input.

```powershell
cargo test --locked -p photos --all-targets
cargo test --locked -p qdoc --lib blobstore::
./tools/install-windows-apps.ps1 -Apps photos
```

Hex Editor is installed as **Hex Editor** (`hex-editor.exe`). Open a file through
the native picker or pass its path as the first positional argument. `Ctrl+S`
saves to the opened file. Bytes, paths and undo history stay device-local rather
than entering the collaboration document. The executable regression test opens a
Unicode path, edits a byte through the real headless input path, saves it and
checks both the resulting file and the collaboration document.

```powershell
cargo test --locked -p hex-editor --all-targets
./tools/install-windows-apps.ps1 -Apps hex-editor
```

Code Editor is installed as **Code Editor** (`code-editor.exe`). Pass a file or
project directory as a positional argument or with `--path`; its embedded
terminal uses Windows ConPTY. Language servers must be installed on `PATH`
(for Rust, `rustup component add rust-analyzer`). Server processes use pipes
without opening console windows, and incoming events wake an idle editor.
Windows drive letters are normalized in LSP document URIs so rust-analyzer's
diagnostics and definition locations match the opened buffers.

```powershell
cargo test --locked -p code-editor --all-targets
./tools/install-windows-apps.ps1 -Apps code-editor
```

API Tester is installed as **API Tester** (`api-tester.exe`). Requests run on a
background worker. Completed request history is saved in the app's CRDT document
and restored on restart; the recorded URL and method describe the request sent,
even if the builder is edited while that request is in flight. The native test
suite exercises the resource Send operation against a real loopback HTTP server.

```powershell
cargo test --locked -p api-tester --all-targets
./tools/install-windows-apps.ps1 -Apps api-tester
```

Snippets uses the shared multiline code editor for snippet bodies. Editing,
IME input and the resource's `edit-body` action update the same CRDT text;
indentation and trailing newlines survive restart. Ctrl+Z uses the host's
document undo. The variable-fill dialog exposes each field and its Copy/Cancel
actions through the same semantic interface used by Windows accessibility.

```powershell
cargo test --locked -p snippets --all-targets
./tools/install-windows-apps.ps1 -Apps snippets
```

Database Client supports SQLite files, including Unicode Windows paths, and the
PostgreSQL and MySQL drivers. Saved connection profiles and SQL editor text are
restored on restart; reopening a profile explicitly reconnects it. Passwords stay
in the running session and are not saved in the collaborative document. The
network drivers currently use unencrypted connections, so use a local server or
an existing trusted tunnel.

```powershell
cargo test --locked -p db-client --lib
./tools/install-windows-apps.ps1 -Apps db-client
```

The live database tests require the server flags and connection settings described
in `apps/db-client/tests/live_roundtrip.rs`. An ordinary test run without those
flags does not verify PostgreSQL or MySQL connectivity.

Git Client opens local repositories, including Unicode Windows paths, and supports
staging, commits, history, branches and local filesystem remotes. Text fields and
repository opening work through native Windows accessibility. Its toolbar wraps
to retain every control in narrow windows. Discard operations respect the
repository's LF/CRLF checkout policy. The Git backend uses Rustls HTTPS and russh
SSH transports without OpenSSL, configured credential helpers and SSH-agent credentials, and
stops retrying rejected credentials. Pull refuses to overwrite conflicting local
edits, and Push reports server-side ref rejection.

HTTPS validates server certificates, supports explicit `http.sslCAInfo` roots,
and rejects redirects and disabled certificate verification. SSH requires an
already trusted host in `~/.ssh/known_hosts` and an agent-held key; custom SSH
commands, host aliases, proxy jumps and password authentication are unsupported.

Remote integration tests require Git and Python 3 (`python` on Windows, `python3`
elsewhere) on PATH. They use disposable loopback servers and
fake credentials to verify authenticated HTTP fetch/push/pull, rejection handling,
trusted HTTPS and SSH transfers, plus rejection of untrusted certificates and
SSH host keys. SSH fixtures use an isolated test agent. Remote operations run on a worker,
leaving navigation and text editing responsive. Repository mutations and switching
repositories wait until the transfer finishes. Resource actions report acceptance,
progress, and the actual completion result. The visible status is also native
live text for accessibility. Cancellation requests do not claim to
undo a remote change. Cooperative cancellation is not yet implemented.

```powershell
cargo test --locked -p git-client --all-targets
./tools/install-windows-apps.ps1 -Apps git-client
```

Authenticator (`totp`) supports native account entry and QR-image import, including
Unicode Windows paths. Its encrypted vault lives under
`%LOCALAPPDATA%/tilo-authenticator` unless `XDG_DATA_HOME` overrides it. Windows
protects the vault key with current-user DPAPI and a private file ACL; it does
not fall back to a plaintext key. A private legacy key is migrated only after
the protected replacement has been read back and verified. Reopening finishes
interrupted plaintext-key cleanup only after the keys match and any existing
vault authenticates; conflicting material and failed deletions require recovery.
A missing or corrupt
key or vault produces a recovery banner and prevents writes until access is
restored and the app restarted. DPAPI custody is tied to the Windows user
profile; copying the vault files is not a portable account export.

Password fields expose the native Windows password role and support protected
value entry while returning only a constant empty accessibility value. Shared
semantic trees never contain their secret values. Native verification covers
manual enrollment, QR import, encrypted persistence, restart, recovery without
overwriting ciphertext, and clipboard codes checked against an independent
RFC-key calculation.

```powershell
cargo test --locked -p totp --all-targets
./tools/install-windows-apps.ps1 -Apps totp
```

Feeds runs natively on Windows with asynchronous feed fetching, persisted
subscriptions and article state, and native OPML Open and Save pickers. Article links
use the Windows default application through ShellExecute; local file and script
URLs are rejected. The shared subscription dialog exposes its real text input and
footer actions to Windows accessibility. OPML export suggests
`feeds-subscriptions.opml` and writes the subscription snapshot to the selected
destination. Import and export choosers are also available as local resource
actions. Transfer outcomes are exposed as accessibility announcements, and
background fetches and file-picker results are polled with reduced motion enabled.

```powershell
cargo test --locked -p feeds --all-targets
./tools/install-windows-apps.ps1 -Apps feeds
```

Unchanged shared fonts are preserved during installation, so a running Tilo app
does not prevent installing another app. If font contents change, close running
Tilo apps before installing the update.

Clocks saves private device state under `%LOCALAPPDATA%/tilo/clocks.json` on
Windows (an absolute `XDG_STATE_HOME` overrides that location). Its live startup
restores that state; screenshot scenarios never save or consume shell commands.
The installer registers `dev.tilo.clocks` on the Start menu shortcut and in the
current user's notification display-name registry. The SDK's `notifications`
feature provides Windows toast submission and app-scoped history inspection.
Alerts obey Windows notification settings and Do Not Disturb. Clocks hands timer
deadlines to Windows, so timers continue to notify after the app closes. Pausing
or deleting a timer cancels its pending Windows alert. A saved receipt records
the accepted deadline so reopening after delivery does not submit it again.
Each state profile durably records the timer schedules it owns before submitting
them to Windows. Opening another profile cannot cancel those schedules; deleted
timers retain ownership until cancellation succeeds, including across restarts.
Alarms still require the app to be running; closed-app alarm scheduling remains
pending. Alarms persist their next UTC deadline, resolved from the requested
local time and weekdays. A newly added past-time alarm waits for its next day;
reopening after an elapsed deadline catches up once, then rearms recurring
alarms for a future occurrence. Turning an alarm off and back on discards its
old deadline. Older saved alarms without a deadline are armed on their first tick.
Alerts carry the `tilo-clocks:` protocol activation URI, which the
installer registers to open the installed executable. This route launches a
normal Clocks window; it does not yet focus an already-running instance. The
registered command ignores URI contents rather than forwarding them as arguments.

For device verification, build `cargo build --locked -p ui --example windows-host
--features notifications`. The helper accepts `history dev.tilo.clocks` to inspect
only Clocks' notifications and `remove dev.tilo.clocks TAG` to dismiss one test
notification. `show ID TAG TITLE BODY` exercises the production SDK API; Windows
may reject a settings query before the app's first notification is submitted.
`schedule ID TAG SECONDS TITLE BODY`, `scheduled ID` and `cancel ID TAG` exercise
the native scheduler. Scheduled IDs are limited to 15 UTF-16 units on this host;
ordinary toast tags allow 16. First-use scheduler registration submits and removes
a suppressed, short-lived notification before adding a schedule.
Apps opt into a registered activation URI with
`HostNotifications::with_protocol_activation`; both immediate and scheduled
notifications use the same XML builder. `show-protocol ID TAG URI TITLE BODY`
verifies the payload independently. The SDK keeps one process-lifetime MTA usage
reference alongside windows-rs' cached activation factories; each caller still
balances its thread-local COM apartment. The regression tests recreate notification
hosts on successive threads and inspect their XML without showing test alerts.

For smaller local development artifacts, set `CARGO_PROFILE_DEV_DEBUG=0` and
`CARGO_PROFILE_TEST_DEBUG=0` in the PowerShell environment before building. Do not
set `CARGO_TARGET_DIR`. When checking Linux from WSL in the same worktree, run
Cargo sequentially with Windows builds and use `--target` with the Linux host
triple so the application artifacts are kept separate.

### Windows ARM64 media development

The native MSVC ARM64 media backend has been tested against MSYS2 CLANGARM64
GStreamer 1.28.6, with `gstreamer`, `gst-plugins-base`, `gst-plugins-good` and
`pkgconf` installed (package names start with `mingw-w64-clang-aarch64-`).
Its import libraries link directly; no `.lib` conversion is required. Configure
the current PowerShell process for your installation:

```powershell
$gstPrefix = 'C:/Users/user/msys64/clangarm64' # Replace with your installation.
$env:PATH = "$gstPrefix/bin;$env:PATH"
$env:PKG_CONFIG = "$gstPrefix/bin/pkgconf.exe"
$env:PKG_CONFIG_PATH = "$gstPrefix/lib/pkgconfig"
cargo test --locked -p media --features decode,encode --lib
cargo check --locked -p video-player --tests
cargo test --locked -p video-player --test real_decode
```

The media suite runs headlessly. To exercise actual compressed video decoding
and seeking without a window or audible output, supply an existing local clip:

```powershell
cargo run --locked -p media --features decode --example decode_probe -- 'C:/Media/clip.webm' 500 rgba sw
```

The probe fails if the real decoder is unavailable; synthesized frames do not
count as decoding evidence. `native` instead of `rgba` also exercises negotiated
system-memory formats such as NV12. Unix DMA-BUF remains Unix-only; this does not
establish Windows hardware decoding, zero-copy presentation or audio-device
routing. GStreamer DLLs and codec plugins must also be available when running an
app; this development setup is not a standalone installer runtime bundle.

### If you build in a container (optional)

For image-based hosts (Silverblue, Bazzite) where `/usr` is read-only and the
libraries cannot be installed without `rpm-ostree` layering plus a reboot per base
update. Create a local `.build-container` containing your distrobox name (for
example, `printf '%s\n' tilo-build > .build-container`). This file is ignored by
Git; there is no shared machine-specific default. Install `tools/cargo-shim`
as `cargo` on PATH; every invocation from the tree or a worktree routes into it.

It also selects the sccache server, which this route requires: sccache's server
executes rustc, so it must share the build's mount namespace or it cannot see that
namespace's headers. distrobox shares the host network namespace and copies the host
environment inward, so without an explicit choice on both sides the container build
reaches the host's server and every compilation misses.

Not needed if the libraries are present natively — a nix/devenv shell removes both
the routing and the sccache-namespace problem, and the shim with them.

### Fedora 44 disposable-VM loop

For desktop development, build the selected debug executable in a Fedora 44
toolbox and copy it to a disposable VM:

```bash
# One time.  This builds a dependency image and creates a persistent distrobox.
dist/dev/fedora44-vm-iterate.sh bootstrap

# Usually the entire edit/build step.  This is debug, with no release fat LTO.
dist/dev/fedora44-vm-iterate.sh build shell

# The VM's loopback SSH forward is intentionally required, never guessed.
TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh deploy shell
dist/dev/fedora44-vm-iterate.sh handoff
# After the normal VM UI logout/login:
TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh verify

# The three commands above, after the first container setup.
TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh cycle shell
```

Use SSH keys normally (set `TILO_VM_SSH_IDENTITY` when the key is not in the
SSH agent), or provide `TILO_VM_PASSWORD` only in the invoking environment.
The script checks `systemd-detect-virt` before copying files. After `handoff`
or `cycle`, log out and back in through the VM's UI to test session cleanup.
`verify` compares the deployed debug shell with the running guest process's
`/proc/<pid>/exe`.

The VM needs the `tilo-session-devel` developer entry once, and **"Tilo (my
build)"** must be selected in its greeter once.  `deploy shell` writes the
marked debug binary to `~/.local/share/tilo-dev/shell` and updates the existing
developer-session config, so later cycles do not install an RPM.  Change the
remote account/location with `TILO_VM_USER`, `TILO_VM_HOST`, and
`TILO_VM_DEV_ROOT`; `TILO_VM_SSH_PORT` is always mandatory. Build an app or
the two canonical host executables on demand with, for example,
`build calculator`, `build tilo-launcher`, `build tilo-unlock-helper`, or
`build host`, rather than rebuilding the application collection.

The shell command builds `--bin shell --features dev-session` so Cargo does not
also compile screenshot tooling during compositor iteration. It retains the
normal shell features inside Fedora 44, matching the actual session rather than
silently dropping media behavior to work around a host ABI mismatch.

The loop also gives this checkout a dedicated sccache server and namespace.
Distrobox copies host environment variables inward; sharing either its generic
cache or the host cache can replay a path-sensitive Rust build script (such as
the vendored shader compiler, which embeds `CARGO_MANIFEST_DIR`) into the host
target directory. This is not a valid host or Fedora 44 artifact. The cache is
persistent for warm iterations; set `TILO_FEDORA44_SCCACHE_PORT` only if two
active checkouts collide on its derived port.

### Hardware-dependent settings

Two settings are derived from the hardware rather than committed:

- **Job count** — bounded by memory, not cores: rustc peaks around 7 GB on the
  larger crates. The shim exports `CARGO_BUILD_JOBS` as `min(cores, RAM ÷ 7 GB)`.
  `jobs = 4` in `.cargo/config.toml` is only a floor for builds that bypass the
  shim, such as CI.
- **sccache ceiling** — 20% of the filesystem, floor 64 GB, cap 200 GB.

Override either with `CARGO_BUILD_JOBS` / `SCCACHE_CACHE_SIZE`. Neither is part
of sccache's cache key, so varying them does not fragment it.

`CARGO_TARGET_DIR` **is** in the cache key: measured, a clean rebuild of one
crate hits with it unset and misses with it set to the identical path. Never set
it, and never point two worktrees at one target directory — cargo's artifact key
omits the package path, so worktrees of one repo collide and cargo reports
`Fresh` while handing you the other's binary.

### What does not make builds faster here

Measurements from `ui`'s test binary:

| change | effect |
|---|---|
| mold instead of GNU ld | none (40 s vs 41 s, and 61 s vs 61 s) |
| `-C split-debuginfo=unpacked` | binary 697 -> 507 MB, build 14% **slower** |
| `-C debuginfo=0` | binary 665 -> 475 MB, build unchanged |
| nightly `--jobs-frontend=4` / `=8` | +13% / 0% |
| nightly cranelift backend | 23% slower |

Compilation dominates: `-Z time-passes` puts type checking at 14% of a debug
build and LLVM at 29%, the rest rustc lowering MIR to IR.

Run-to-run noise on a busy machine is ±20%, which is larger than most of the
effects above. Measure on an idle machine.

Use sccache to share compilation results across worktrees; target directories
cannot be shared. A measured `ui` build in a fresh worktree took 126 s at 77%
cache hits, compared with ~562 s cold.

Changing `build.rustflags` invalidates cached compilations across worktrees.
Batch flag changes and avoid them during verification runs.

### Sizing the compilation cache

116 members over ~1,480 external crates. One full debug+test build is ~2,200
cacheable units, a few GB compressed (measured: 105,000 objects in 100 GB, mean
0.97 MB, median 0.22 MB).

Cache dependencies between lockfile changes. Changes to a shared crate such as
`ui` rebuild the compositor and dependent apps. At roughly 3–5 GB per full set,
200 GB holds about forty versions.

Prefer cache over target directories when trading disk: a cached object serves
every worktree and survives `cargo clean`, a target directory serves one and
does not.

The compiler is part of the cache key, so bumping `rust-toolchain.toml` costs one
fully cold rebuild.

## Disk, and why builds hang

One worktree's `target/` reaches 150+ GB. Delete `target/` from worktrees whose
branches have landed; sccache retains reusable compilation results.

If builds *hang* rather than run slowly — uninterruptible sleep, no progress at
all — that is btrfs metadata exhaustion, and deleting files alone will not fix
it: freed space returns inside existing chunks, so `df` moves while
`unallocated` stays near zero and metadata cannot grow. Free something first,
**then**:

```bash
sudo btrfs balance start -dusage=80 -dlimit=25 /var/home
```

In this environment, `-dusage=30` relocated no chunks despite exiting
successfully; `-dusage=80` was needed.

`/tmp` is a 16 GB tmpfs. Concurrent rustc processes fill it, and the resulting
failures do not name the cause: bogus "Quota exceeded" messages, exit 1 with no
output, from any command. Point `TMPDIR` at real disk for large parallel builds.

## Building and testing

```bash
cargo build -p shell            # or ui, fs, terminal, file-explorer, browser
cargo test -p fs
cargo test -p shell --lib room_activity      # scope it; the compile dominates
cargo check -p shell --tests                 # before finishing
```

Full OS image, which needs rootless `podman` for the container step and `sudo`
for the disk-image step:

```bash
./os/build.sh
./os/test-vm.sh                              # needs OVMF + KVM
./os/verify-test.sh                          # test image with a validation unit
./os/test-vm.sh os/output-test/qcow2/disk.qcow2
```

VM login `tester` / `tilo`. Check the FUSE mount with
`mountpoint /var/workspace`; the test image's results are in
`journalctl -u fs-test`.

## Running Tilo on your own machine (Silverblue / Bazzite)

`dist/` packages the compositor as a Wayland login session for Silverblue-family
hosts. An optional developer entry starts a local build.

- **Nested** — the compositor runs as a window inside your current session.
  No logout is needed. DRM, seat access and session locking are unavailable.
- **Real session** — the compositor drives KMS on its own VT, as a login
  session. Use this to test screen locking, input and multiple outputs.
  Log out and back in after each build.

### Install once

```bash
dist/rpm/build-rpm.sh
```

Builds `shell`, `tilo-launcher` and `tilo-unlock-helper` in release, stages them
with the session entries and PAM stack, and runs `rpmbuild` (in the build
container, since an image-based host has no `rpm-build`). Two packages land in
`dist/rpm/build/RPMS/x86_64/`:

```console
$ rpm -qlp dist/rpm/build/RPMS/x86_64/tilo-session-0.1.0-1.fc43.x86_64.rpm
/etc/pam.d/tilo-unlock
/usr/bin/tilo-launcher
/usr/bin/tilo-shell
/usr/libexec/tilo-session
/usr/libexec/tilo-unlock-helper
/usr/share/wayland-sessions/tilo.desktop
/usr/share/xdg-desktop-portal/portals/tilo.portal

$ rpm -qlp dist/rpm/build/RPMS/x86_64/tilo-session-devel-0.1.0-1.fc43.x86_64.rpm
/usr/libexec/tilo-session-dev
/usr/share/wayland-sessions/tilo-dev.desktop
```

GDM reads session entries from `/usr/share/wayland-sessions/`. On an ostree host,
this directory is read-only, so install the entries through the package.

Then install, either way:

```bash
# Persistent, and the one you want long term. Needs a reboot to take effect.
sudo rpm-ostree install ./dist/rpm/build/RPMS/x86_64/tilo-session-[0-9]*.rpm

# Or: this boot only, no reboot. Mounts a writable overlay on /usr and unpacks
# the same files into it. Everything under /usr is gone at the next boot.
sudo dist/rpm/usroverlay-install.sh
# Add `--devel` only when you explicitly need the developer session entry.
sudo dist/rpm/usroverlay-install.sh --devel
sudo dist/rpm/usroverlay-install.sh --uninstall
```

Either way, log **out** (not lock) afterwards: the greeter reads
`/usr/share/wayland-sessions` when it starts, so a new entry appears at the next
greeter, and "Tilo" shows up in the session menu next to your usual desktop.

Skip `tilo-session-devel` on any machine you are not developing Tilo on — see
"Edit → build → log into your own build" below for what it can do.

### Edit → build → run nested

The compositor prints its socket name and writes it to
`$XDG_RUNTIME_DIR/tilo-session/wayland-display`:

```console
$ cargo build -p shell && ./target/debug/shell --backend winit
tilo compositor listening on "wayland-1"
  WAYLAND_DISPLAY=wayland-1 — e.g. `WAYLAND_DISPLAY=wayland-1 cargo run -p terminal`
  also written to /run/user/1000/tilo-session/wayland-display
```

Run an app on that socket:

```bash
cargo build -p terminal
WAYLAND_DISPLAY=$(cat "$XDG_RUNTIME_DIR/tilo-session/wayland-display") ./target/debug/terminal
```

The file is last-writer-wins — start a second compositor and it names the
second one. The printed line stays authoritative for a particular process.

To test a nested Tilo compositor:

```bash
./target/debug/shell --backend winit &                     # outer → wayland-1
WAYLAND_DISPLAY=wayland-1 ./target/debug/shell --backend winit &   # inner → wayland-2
```

Measured here on a debug build: ~7 s from exec to the inner compositor serving
its own socket, and both stayed interactive afterwards. A debug build is heavy —
it is `vello` + `wgpu` unoptimised — so expect the nested one to feel soft.
`--backend drm` is not an option from inside a running session: `libseat` will
not hand you a seat the running desktop owns, and fails with `ENOSYS` (error
code 38), which is a refusal, not an unimplemented backend.

### Edit → build → log into your own build

Once, after installing `tilo-session-devel`:

```bash
cargo build -p shell --features dev-session
echo "$PWD/target/debug/shell" > ~/.config/tilo/dev-session
chmod 600 ~/.config/tilo/dev-session
```

Then log out and pick **"Tilo (my build)"**. Every iteration after that is
`cargo build -p shell --features dev-session`, log out, log back in — the config
file keeps pointing at the same path.

`--features dev-session` adds the marker required by `/usr/libexec/tilo-session-dev`.
Release builds omit this marker even with the feature enabled. The config file,
target binary and their parent directories must be owned by you or root and
writable by nobody else. Failed checks abort startup without falling back to
the installed compositor. `dist/session/tests/dev-session-refusals.sh` tests
these checks.

The production wrapper, `/usr/libexec/tilo-session`, always starts
`/usr/bin/tilo-shell`. Install the separate `tilo-session-devel` package to
enable login sessions from local builds.

### What a stock host does not give you

The compositor starts on a Bazzite host without `/var/workspace`, `/etc/tilo`,
`/var/lib/tilo` or `/usr/share/tilo-apps`, with these limitations:

- **No `ush` runtime**, so the terminal falls back to `$SHELL` unconfined. That
  works interactively and fails inside an app sandbox.
- **No first-boot unit.** The image runs `shell --firstboot` from a systemd
  unit before the session; a host has none, so without help nothing ever
  creates `~/.config/tilo/identity/primary.json` and every session logs *"no Tilo
  identity found"*. Both session wrappers therefore pass `--firstboot`
  themselves — it does the setup and falls through into the compositor, and
  provisioning returns early once the record exists. Start the compositor by
  hand (`./target/debug/shell`) and you get the warning, because you skipped
  the wrapper.
- **`shell/src/config.rs`'s `load_config` is dead code** — nothing calls it, and
  it looks for `/etc/tilo-desktop/desktop.toml` while the image ships
  `/etc/tilo/desktop.toml`. So `desktop.toml` configures nothing today, on a host
  or on the image. The host package omits this file.

`/var/workspace` is a FUSE mount `fs.service` makes on the Tilo image; there is
no such unit here, so the permission UI will talk about a path that is not
mounted.

Alongside Tilo packages, the shell scans `$XDG_DATA_HOME/applications`, each
`$XDG_DATA_DIRS` entry's `applications`, and both Flatpak export directories
(`crates/system/desktop-entry/src/scan.rs`). Host-installed apps and Flatpaks
are available without a Tilo registry.

Unlocking requires `/usr/libexec/tilo-unlock-helper` and `/etc/pam.d/tilo-unlock`,
both installed by the package. Without the helper, unlock attempts fail.
Check PAM access with:

```console
$ printf 'not-the-password\n' | /usr/libexec/tilo-unlock-helper "$USER"; echo $?
1     # 1 = PAM ran and denied it. 2 would mean the check could not run at all.
```

Release builds ignore `TILO_UNLOCK_HELPER`,
`TILO_LOCK_TEST_PASSWORD`, `TILO_TEST_CONTROL`, `TILO_TEST_STATE_DUMP` and
`TILO_LAUNCHER_BIN`. On a systemd desktop the session environment comes from
`~/.config/environment.d/`, so an installed binary that honoured those would let
anything able to write a file in your home decide what unlocks your screen. See
`shell/src/compositor/dev_env.rs`.


## Headless testing

`AGENTS.md` covers the entry points. The full guide is
[`docs/headless-testing.md`](docs/headless-testing.md), and
[`scripts/headless-examples/`](scripts/headless-examples/) holds working scripts —
`spawn-headless.sh` starts a compositor and prints `READY <socket>`,
`full-loop.py` goes from nothing to a PNG in about five seconds, and
`_control.py` is the socket client to inline rather than rewrite.

`--backend headless` is what makes the live compositor runnable in CI, over SSH
and inside a container with no display forwarded.

## Pre-commit hook

Enable the repo's git hooks once per clone:

```bash
git config core.hooksPath .githooks
```

`pre-commit` runs `scripts/check-workspace-complete.sh`, which parses the root
manifest and rejects missing, duplicate, excluded, or stale workspace members.
Nested fixture workspaces and explicit `[workspace].exclude` entries are ignored.
Compilation and tests run in CI (`.forgejo/workflows/check.yml`).
