# Development Guide

Build dependencies, local development and testing. See `AGENTS.md` for repository
conventions.

## Dev VMs (tilo-vm)

`tilo-vm` is how to run your changes in the Tilo OS image, and the dev story going
forward: one host CLI for Linux, macOS and Windows that gives each person or agent
a **slot** in a shared dev VM, meaning its own user, session and stack of layers over
the VM's `/usr`. Swapping a component is one command and never needs a new VM or a
reboot; every swap is a generation you can roll back. The VM runs under the host's
native hypervisor, so you watch it in its UI: on Linux, libvirt's `qemu:///session`
in virt-manager (File > Add Connection > QEMU/KVM user session) or GNOME Boxes; UTM
on macOS; Hyper-V Manager on Windows.

```sh
cargo build -p tilo-vm && target/debug/tilo-vm guide quickstart
target/debug/tilo-vm doctor          # hypervisor, KVM, tokens, published disk, each with its fix
target/debug/tilo-vm vm create dev   # checks everything, then downloads the published dev disk
target/debug/tilo-vm vm start
target/debug/tilo-vm slot create --headful
target/debug/tilo-vm swap shell      # sync this worktree, build in the VM, restart the session
target/debug/tilo-vm shot -o target/shot.png
```

Swaps build release binaries with the manifest's features plus `dev-overrides`
(realistic speed, and the harness variables below still work); `--profile dev`
compiles faster. Driving is the same `tilo-control` story as on the host, through
`tilo-vm control`. The built-in guide (`tilo-vm guide`) covers concepts, swapping,
driving, storage, platforms and troubleshooting; its source is
`crates/dev/tilo-vm/guide/`.

The [Fedora 44 disposable-VM loop](#fedora-44-disposable-vm-loop-legacy)
(`dist/dev/fedora44-vm-iterate.sh`) is the legacy path until slots cover what it
does; `tilo-vm guide fedora-loop` maps its commands.

## Routine delivery workflow

Routine changes land through pull requests to `main`. Each PR head revision gets
its own affected-package check and fast-quality lint result; those runs are keyed
to the exact event and commit and are not cancelled when a newer commit arrives.

Once the required checks pass and the head is stable, the controller posts a PR
comment explaining the next step. Keep iterating until ready, then the PR author
comments `/merge <full-head-sha>` to request integration of that exact revision.
A new push requires fresh checks and a fresh merge request. The controller also
checks that the branch is up to date and the change is eligible for routine
integration; a green check alone does not merge a PR.

Review is optional and informational for every change. Review requests,
approvals and change requests never affect merge admission; exact-head CI and
author intent are authoritative. Automatic review-agent launches are currently
disabled, so a native request alone does not start an agent.

CI policy, publication, signing and named security-boundary paths use the
protected-path operator flow. Changes crossing into those paths or changing an
existing file's executable mode may require additional CI or provenance checks,
but never review. After checks pass, post the author's `/merge <full-head-sha>`
intent. An authorized operator then runs this command from the `PodestaAI/infra`
checkout:

```sh
python scripts/run-tilo-reviewed-merge.py PR FULL_HEAD_SHA --execute
```

It runs the deployed merge worker, which rechecks scope, current main, CI and
author intent before integration. This command accepts only changes classified
into the protected scope. Review state does not change a PR's scope or queue
priority.
Direct merges do not bypass that worker.
The [deployed admission policy](https://code.sandbox.example/PodestaAI/infra/src/branch/main/apps/forgejo/tilo-merge-controller/policy.json)
defines the protected paths and bounded dependency exceptions. The OpenH264
source-order exception additionally requires its pinned provenance and native
dependency check; it is not ordinary vendored-source admission. Symlinks,
submodules and malformed changes are rejected rather than treated as routine.

A bounded queue pilot on 2026-09-20 created [integration PR 169](https://code.sandbox.example/qos/qos/pulls/169)
for [author PR 162](https://code.sandbox.example/qos/qos/pulls/162), tested the generated
candidate and landed exact commit `12fbe8301ddc9b3d410cefe49d79630b334034d3`
without changing the author's branch. This verifies one integration through the
pilot; it does not establish general queue availability or sustained reliability.
Continue using the controller's current eligibility instructions.

The CI scheduler assigns runners and queue order. Submit normally; do not reserve
build slots or negotiate them with other agents. The repository's Codex hook sends
final CI status back to the task after a successful normal `git push` when the
hook is installed in the running environment. Use `fgj pr checks <number>` or
`fgj actions` to inspect missing notifications or failures, rather than repeatedly
polling queued jobs.

After a merge, app packaging selects directly changed apps and apps that depend
on changed shared crates, then reuses unchanged collection members. PRs run the
selection plan without release-mode packaging. Shared runtime or toolchain changes
can still select every app; see [Preview affected applications](#preview-affected-applications)
for how selection and shared runtime compatibility interact. Use **Release / apps** manual
dispatch for a development artifact, selecting the smallest applicable app and
architecture set and leaving `publish` off unless publication is intended.

The app registry is publicly reachable at `https://apps.tilo.so`. Main-branch
app publication now submits each newly built immutable tapp to it; separate collector
and networkless signer jobs independently verify accepted requests before advancing
the signed catalogue. Forgejo package publication remains a distinct artifact channel
and alone does not make an app discoverable. See the [production roadmap](docs/ci-production-roadmap.md)
for the remaining acceptance gates.

OS publication remains explicit: use a `vMAJOR.MINOR.PATCH` release tag or manually
run **Release / OS** from `main` with `publish` enabled. Ordinary `main` pushes and
scheduled integration runs do not publish an OS release. macOS setup packaging,
cask publication and installation tests remain in **Release / imager** but are
disabled during alpha; tags do not enable those lanes.

## Fast checks before assembly

For a native integration test without a full workspace dispatch, use
**Development / focused native test** (`focused-test.yml`). Select a full source
commit SHA, one Cargo package, integration target and exact Rust test path. For
example: package `shell`, target `it`, test
`concept_parity_spec::overview_add_workspace::overview_add_workspace_click_appends_workspace`.
Use operation `check-tests` with the target/name empty to compile that package's
test targets. Both modes use `--locked` on one native x86-64 runner and share the
normal download/compiler caches. Missing or ignored tests fail. The job records
source SHA, workflow SHA, builder digest and exit status; its diagnostic result
does not replace required PR checks or trigger publication.

For a related group, set `target_kind=lib` (leave `test_target` empty), or keep
`target_kind=integration` and name the integration target. Supply `test_names` as
a JSON array of up to 32 exact paths, leaving `test_name` empty. All names must
exist before any case runs. Every case runs even if an earlier case fails, and
the run fails if any case fails or is ignored. One job reuses one target tree;
there is no workspace-wide selection or per-case runner dispatch.

For example, write `["tests::first_case", "tests::second_case"]` to `cases.json`
and submit directly through the scheduler (replace the example package and SHA):

```sh
jq -n --rawfile tests cases.json --arg source FULL_SOURCE_SHA \
  '{ref:"main",inputs:{source_sha:$source,package:"shell",operation:"test",target_kind:"lib",test_names:$tests}}' \
  | fgj api -X POST repos/qos/qos/actions/workflows/focused-test.yml/dispatches --hostname code.sandbox.example
```

Use this structured request for lists: the installed `fgj workflow run` does not
expand `-F` files and parses `-f` values as CSV, which breaks JSON arrays.

Do not reserve runners or ask another agent to allocate a slot. Each case's
result and the exact source/workflow/builder identities appear in the run summary.

Run these from the checkout on Linux (including WSL), using Python 3.11+:

```sh
tools/policy/run.sh
python3 tools/dev.py check installer
python3 tools/dev.py check app-packaging
```

The repository-policy command is the same deterministic suite used by the
fast-quality workflow. It checks live workflow triggers and timeouts in
addition to running the policy unit tests, so testing a checker without
auditing the current repository cannot produce a false-green result.

The installer checks exercise target-mode logic, artifact materialization and the
installer overlay. The app checks exercise selection, collection membership and
the actual packaging/upload/assembly layout with fixture build/signing tools.
Neither command compiles the workspace, builds an OS, creates a VM or changes a
device. They do not replace native package, VM install or hardware acceptance.

Use `--plan` to see the commands and required tools without running them.
Use `--step target-modes` to rerun just one installer contract; repeat `--step`
to select more. Receipts explicitly list omitted checks and identify subset runs.
`--keep-going` runs the remaining independent checks after a failure. Each run
writes `target/dev-runs/<id>/run.json` with per-stage timings, results and unrun
steps; `--out <new-directory>` chooses the receipt location. Existing evidence is
never overwritten. Output stays live in the terminal; receipts do not collect the
environment or credentials and are not release attestations.

App packaging checks need Bash, jq, tar, sha256sum and python3. Installer checks
need Bash; non-root developers also need `unshare` and enabled unprivileged user
namespaces. The materialization test then sees root-owned fixtures inside a
private user namespace, without sudo or weakening the production ownership check.
If the environment prohibits user namespaces, use the existing isolated Linux
builder where these tests can run as container root. Missing capabilities fail
the command and are recorded in its receipt.

CI's repository policy tests discover the same app-packaging tests, including
the build-to-collection handoff regression.
For an actual installer VM cycle with an already prepared medium, continue using
`os/dev/test-installer-cycle.sh`. To run a change in the Tilo OS image, use
`tilo-vm` ([Dev VMs](#dev-vms-tilo-vm)); for the disposable Fedora desktop
development session use `dist/dev/fedora44-vm-iterate.sh`.

For CI timing, use the repository run number from the Forgejo URL or `fgj actions
run list`, not the Forgejo API's internal database ID:

```sh
python3 tools/ci/timing_report.py --repo qos/qos --run 11408 --output target/ci-timing.json
```

For an automatic before/after view of the newest comparable runs, no run IDs or
manual receipts are needed:

```sh
python3 tools/ci/timing_report.py --repo qos/qos --recent 30 \
  --workflow check.yml --compare-latest --output target/ci-experience.json
```

This uses your configured `fgj` authentication and reports queue time separately
from run duration and the observed container pull-to-create interval. It does not
sum parallel jobs into elapsed time or invent missing timestamps. Raw job logs and
credentials are not written to the report; use `--input` for saved local fixtures.
Forgejo reruns retain the original run creation time, so the report labels the
attempt and leaves its queue time unknown unless that attempt's creation is present.
Schema 3 keeps the deprecated run-created-to-runner field for first attempts and
reports it as unknown for reruns; its replacement is explicitly attempt-scoped.
Pass two runs in baseline/current order with `--compare` to print queue, wall and
same-named job deltas directly. Job names appear beside Forgejo's numeric IDs, so
the report can be read without a second API lookup.
The recent view is keyed by workflow, event and exact commit. It separates queue,
runner assignment, image pull/create, checkout, measured compile/test phases and
total time, and includes sccache hit indicators when the job reports them. Its
snapshot also flags superseded active runs, terminal runs with active tasks, runs
which started without a runner, and observed runner time discarded by cancellation. These
markers identify work to inspect; the runner broker remains authoritative for
whether a Kubernetes runner resource is actually orphaned.
An unavailable run or log is retained with a `collection_errors` evidence gap;
it does not discard the remaining snapshot or become a comparison baseline.
When too little complete evidence remains, the report records that the optional
comparison is unavailable and still exits successfully with the snapshot.

For a failing pull request, start with the exact-head contexts and then open only
the failed run:

```sh
fgj pr checks <pr-number>
fgj actions run view <run-number>
fgj actions run logs <run-number>
```

The app plan and affected-package selector are workflow summaries, so inspect
them before assuming CI built the whole workspace or app fleet. Main-branch
check receipts are recorded out of band by `tools/ci/receipt.sh`; they do not
modify `main` and are evidence for one source SHA and run ID only. A successful
receipt does not qualify a package, OS release, registry deployment or hardware
target. For queue/startup regressions, preserve the run number and compare it
with `timing_report.py` instead of estimating from the web UI.

## Preview affected applications

When an app packaging selection is surprising, inspect the existing selector
without compiling or dispatching CI. From a branch with committed changes and
an up-to-date `origin/main`:

```sh
python3 -I tools/ci/python.py tools/apps/app_release.py --base origin/main
```

The JSON lists selected apps, count and input classification. Selection includes
apps that transitively depend on changed workspace crates; shared runtime,
lockfile, toolchain or packaging changes can select the whole app fleet. This
command needs the pinned Rust toolchain for `cargo metadata --locked --no-deps`.
It compares committed Git revisions and does not include uncommitted edits.

The selected list controls which app binaries are compiled and packaged. Cargo
still resolves the full default collection's dependency features to preserve the
shared Rust runtime ABI. Shared dependencies may rebuild; unselected app binaries
are not compiled.

A zero count means no app package is selected; check/lint and relevant tests
still apply. Packaging automation compares against its last independently
verified collection baseline, which can differ from this branch-to-main preview.

## Linux builds in WSL: keep the checkout on the Linux filesystem

Use a separate Linux checkout under your WSL home directory for sustained Rust
development. On the investigated WSL host, `/mnt/c` preserves fractional mtimes
on normal writes but truncates explicitly restored mtimes to whole seconds.
Cargo restores fingerprint timestamps, so generated files and proc-macro outputs
can appear newer on every unchanged check. A 2026-09-19 no-edit Cargo trace showed
`serde_derive` 633 ms and `tracing_attributes` 500 ms newer than those truncated
fingerprints. This causes real dependency recompilation; adding compiler cache
capacity does not repair local freshness checks.

Start a new, independent checkout **inside a WSL shell**, leaving any running
Windows-hosted checkout in place:

```bash
mkdir -p "$HOME/src"
git clone ssh://git@code.sandbox.example/qos/qos.git "$HOME/src/tilo-linux"
cd "$HOME/src/tilo-linux"
git switch -c codex/my-change
findmnt -T "$PWD" -o FSTYPE,TARGET
tools/ci/check-build-env.sh
cargo check -p shell --tests --locked
```

The destination must be new, and the mount should be a native Linux filesystem
(normally ext4), not a Windows drive mounted under `/mnt/c` or another `/mnt`
drive. Set up SSH authentication and the native packages described below first.
Rustup uses the checked-in toolchain. This clone contains committed remote state;
uncommitted changes from another checkout are not copied automatically. Bring
over the intended branch or reviewed patch explicitly before testing it.

Each checkout owns its ordinary `target/`. Never set `CARGO_TARGET_DIR`, share
target directories, or relocate an active checkout. The first native-filesystem
build can be cold; the benefit is reliable reuse on later unchanged invocations.
Native Windows builds remain in their Windows checkout.

To diagnose unexpected rebuilding after an already successful check, run the
same command with fingerprint logging and inspect the reported dirty inputs:

```bash
CARGO_LOG=cargo::core::compiler::fingerprint=info \
  cargo check -p shell --tests --locked 2>target/cargo-fingerprint.log
```

Do not rewrite source or fingerprint timestamps to force freshness. The trace
may identify a genuine source, configuration or generated-input change instead.

## What the build needs

Install Rust through rustup and the native dependencies below, either on the host
or in your build container or Nix shell. Rustup reads `rust-toolchain.toml`.
Check dependencies with:

```bash
tools/ci/check-build-env.sh
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
| `cryptsetup` (binary) | `cryptsetup` | rollback mechanism tests drive a real LUKS2 header |
| `swtpm` `swtpm_setup` (binaries) | `swtpm` `swtpm-tools` | rollback mechanism tests drive an isolated software TPM |
| `tpm2_getcap` (binary) | `tpm2-tools` | rollback mechanism tests exercise production TPM policy commands |

The sandbox, language-server, and rollback mechanism tests require the binary
tools listed above. Install them where Cargo runs, including inside a build
container.

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
`./tools/windows/install-apps.ps1 -Apps passwords`. It is not yet included in the
default verified-app set: native backup/import/export and failure reporting are
verified, but successful Windows clipboard copying still needs verification.
The automated session currently receives an access-denied error from Windows.
Mail is available as an explicit preview with
`./tools/windows/install-apps.ps1 -Apps mail`. Native empty-state setup, protected
password fields, responsive provider discovery, and cancellation of stalled
credential verification are verified. Configured-account reading and sending
still need native end-to-end verification, so Mail remains outside the default set.
Typst is also available as an explicit preview with
`./tools/windows/install-apps.ps1 -Apps typst-editor`; native source editing,
Save/Open, and PDF export have been verified, including reduced-motion settings
and emoji export through the shared font catalog. It remains outside the
default set while startup measurements and Start integration checks finish. The
installed optimized binary has passed those native file and PDF workflows.

The installer verifies the real Windows Start catalog and reports an error if
Windows has not indexed the shortcut, even when the executable was copied.

Install the currently verified native apps into your Windows user profile, with
Start menu entries under **Tilo**. Run the installer from an ordinary,
unpackaged PowerShell session: packaged terminals or agents can redirect
AppData writes into their private package cache. Such files look installed
inside that process but are invisible to Explorer. Check the actual executable
and shortcut paths from outside the packaged process before diagnosing a stale
Start catalog; restarting Explorer cannot expose private package files.
Before building, the installer creates and immediately removes a temporary
probe in each destination and checks its physical path. Redirected package
cache writes fail this check; merely resolving the directory is insufficient.

```powershell
./tools/windows/install-apps.ps1
# Or select applications (optimized release builds are the default):
./tools/windows/install-apps.ps1 -Apps calculator,notes
# Use dev explicitly when debugging, rather than for everyday installed apps:
./tools/windows/install-apps.ps1 -Apps calculator -Profile dev
```

This builds before copying executables to `%LOCALAPPDATA%\Tilo\Applications`.
The installer limits Cargo to two concurrent jobs to bound memory use during
release linking. Use `-Jobs 1` on a memory-constrained machine; `-Jobs` also
applies to the Windows notification helper build.
Unoptimized builds spend substantially longer validating and loading fonts at
startup. The installer includes Microsoft's pinned DXC 1.8.2505.1 compiler and
its MIT/LLVM notices beside the apps. Without DXC, wgpu's FXC fallback spends
seconds compiling UI shaders on every launch. `install-dxc.ps1` can
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
The same variable works with `--headless-drive`: the launcher flushes metrics
before its explicit process exit, including failed actions. Headless events
separate harness setup, actions, output and persistence. These measure the
production headless path, not native window creation or time to first visible
content; use an isolated app data directory when benchmarking startup so a test
does not load or modify personal documents.
Close an installed app before updating its executable. Application documents
remain in their normal user data locations; the installer does not move them.
The installer also checks both Windows' application catalog and `Get-StartApps`
against the installed executable paths and intended app names. A shortcut on disk alone is insufficient:
First rule out redirected AppData writes as described above. If the real files
exist, investigate Explorer's catalog, then run
`./tools/windows/check-start-apps.ps1 -Apps hex-editor` (substitute
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
The Tilo filesystem projection, received-share transport, and shell-owned Nearby
Send service are not yet available on native Windows; their control operations
return errors. Delete uses
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
./tools/windows/install-pdfium.ps1
./tools/windows/install-pdfium.ps1 -ArchivePath C:/Downloads/pdfium-win-arm64.tgz
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
./tools/windows/install-apps.ps1 -Apps image-editor
```

Photos uses the native file picker and keeps imported image bytes in the shared
filesystem blob store. On Windows the default is `%LOCALAPPDATA%/tilo/blobs`,
with `USERPROFILE/AppData/Local` as the native fallback; `TILO_BLOB_STORE` and
`XDG_CACHE_HOME` retain their explicit override behavior. The document stores
content hashes and edits, so both its document and this store must survive a
restart. Live startup and headless automation install the durable store before
opening images; screenshot fixtures stay in memory. Completed thumbnail jobs
wake the host so the library finishes loading without pointer or keyboard input.
Photos' launcher-provided model gateway remains Unix-only; Windows reports model
analysis unavailable instead of bypassing that service with direct network access.

```powershell
cargo test --locked -p photos --all-targets
cargo test --locked -p qdoc --lib blobstore::
./tools/windows/install-apps.ps1 -Apps photos
```

Hex Editor is installed as **Hex Editor** (`hex-editor.exe`). Open a file through
the native picker or pass its path as the first positional argument. `Ctrl+S`
saves to the opened file. Bytes, paths and undo history stay device-local rather
than entering the collaboration document. The executable regression test opens a
Unicode path, edits a byte through the real headless input path, saves it and
checks both the resulting file and the collaboration document.

```powershell
cargo test --locked -p hex-editor --all-targets
./tools/windows/install-apps.ps1 -Apps hex-editor
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
./tools/windows/install-apps.ps1 -Apps code-editor
```

API Tester is installed as **API Tester** (`api-tester.exe`). Requests run on a
background worker. Completed request history is saved in the app's CRDT document
and restored on restart; the recorded URL and method describe the request sent,
even if the builder is edited while that request is in flight. The native test
suite exercises the resource Send operation against a real loopback HTTP server.

```powershell
cargo test --locked -p api-tester --all-targets
./tools/windows/install-apps.ps1 -Apps api-tester
```

Snippets uses the shared multiline code editor for snippet bodies. Editing,
IME input and the resource's `edit-body` action update the same CRDT text;
indentation and trailing newlines survive restart. Ctrl+Z uses the host's
document undo. The variable-fill dialog exposes each field and its Copy/Cancel
actions through the same semantic interface used by Windows accessibility.

```powershell
cargo test --locked -p snippets --all-targets
./tools/windows/install-apps.ps1 -Apps snippets
```

Database Client supports SQLite files, including Unicode Windows paths, and the
PostgreSQL and MySQL drivers. Saved connection profiles and SQL editor text are
restored on restart; reopening a profile explicitly reconnects it. Passwords stay
in the running session and are not saved in the collaborative document. The
network drivers currently use unencrypted connections, so use a local server or
an existing trusted tunnel.

```powershell
cargo test --locked -p db-client --lib
./tools/windows/install-apps.ps1 -Apps db-client
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
./tools/windows/install-apps.ps1 -Apps git-client
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
./tools/windows/install-apps.ps1 -Apps totp
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
./tools/windows/install-apps.ps1 -Apps feeds
```

Unchanged shared fonts are preserved during installation, so a running Tilo app
does not prevent installing another app. If font contents change, close running
Tilo apps before installing the update.

Clocks saves private device state under `%LOCALAPPDATA%/tilo/clocks.json` on
Windows (an absolute `XDG_STATE_HOME` overrides that location). Its live startup
restores that state; screenshot scenarios never save or consume shell commands.
The installer registers `so.tilo.clocks` on the Start menu shortcut and in the
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
--features notifications`. The helper accepts `history so.tilo.clocks` to inspect
only Clocks' notifications and `remove so.tilo.clocks TAG` to dismiss one test
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

The installer bundles a shared, manifest-verified runtime for Music and Video
Player. Check installed apps from an ordinary, unpackaged PowerShell session:

```powershell
./tools/windows/test-installed-media.ps1
./tools/windows/test-gstreamer.ps1 -RuntimeDirectory "$env:LOCALAPPDATA/Tilo/Applications" -MediaPath ./apps/video-player/tests/fixtures/h264-aac.mp4
```

The first check rejects package-private installation paths, uses isolated
profiles and a system-only `PATH`, and opens no visible windows. It runs Music's
CPU widget benchmark and decodes the local H.264/AAC fixture in Video Player
into an offscreen PNG. Its output directory retains stdout, stderr, the PNG and
JSON results. Reported durations cover the entire test process, not time to the
first visible frame. The second check verifies runtime hashes, plugin origins
and decoding to completion; it needs `llvm-readobj.exe` on `PATH`. Neither
check proves audible output or Windows hardware decoding.

Music constructs its empty UI without initializing GStreamer. Loading a source
starts one background initializer; track replacement, pause, volume and seek
requests retain their latest intent while it runs. Stopping cancels delivery
without waiting for native initialization. Playback errors remain explicit,
and handoff readiness requires decoded audio rather than a requested URI.
The same lifecycle is used on every supported platform.

Music can open an audio path or `file:` URI passed on the command line. Its
empty Home view and `music.library/open-file` action use the shared native
picker; remote callers cannot supply a raw filesystem path through that action.
The current local source and resume position live in the device profile, outside
the collaborative library. Local files are not advertised as cross-device
handoffs or uploaded into the shared catalogue. Picker cancellation, decoder
failure and confirmed playback have distinct operation results.

### If you build in a container (optional)

For image-based hosts (Silverblue, Bazzite) where `/usr` is read-only and the
libraries cannot be installed without `rpm-ostree` layering plus a reboot per base
update. Create a local `.build-container` containing your distrobox name (for
example, `printf '%s\n' tilo-build > .build-container`). This file is ignored by
Git; there is no shared machine-specific default. Install `tools/ci/cargo-shim`
as `cargo` on PATH; every invocation from the tree or a worktree routes into it.

It also selects the sccache server, which this route requires: sccache's server
executes rustc, so it must share the build's mount namespace or it cannot see that
namespace's headers. distrobox shares the host network namespace and copies the host
environment inward, so without an explicit choice on both sides the container build
reaches the host's server and every compilation misses.

Not needed if the libraries are present natively — a nix/devenv shell removes both
the routing and the sccache-namespace problem, and the shim with them.

### Fedora 44 disposable-VM loop (legacy)

For Tilo OS work, use `tilo-vm` ([Dev VMs](#dev-vms-tilo-vm)); `tilo-vm guide
fedora-loop` maps each command below to it. This loop remains the path for
testing Tilo as a developer session on a stock Fedora 44 desktop, which dev VM
slots do not cover yet.

Build the selected debug executable in a Fedora 44 toolbox and copy it to a
disposable VM:

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
marked debug binary to `~/.local/share/tilo-app/shell` and updates the existing
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
/usr/share/wayland-sessions/tilo-app.desktop
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
Release builds omit this marker even with the feature enabled, unless they also
enable `dev-overrides`. The config file,
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
- **`crates/session/shell/src/config.rs`'s `load_config` is dead code** — nothing calls it, and
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
`TILO_LAUNCHER_BIN`, and say so on stderr at startup when one is set. On a
systemd desktop the session environment comes from `~/.config/environment.d/`,
so an installed binary that honoured those would let anything able to write a
file in your home decide what unlocks your screen. See
`crates/session/shell/src/compositor/dev_env.rs`.

Dev images need release-speed binaries that harnesses can still drive; build
those with `cargo build --release -p shell --features dev-overrides` (what
`tilo-vm swap` builds by default; `tilo-vm swap --profile dev` trades that
speed for faster compiles). Such a
binary honours the variables above and announces itself as a dev-overrides
build at startup. It must never ship: `tools/policy/check_dev_overrides_contract.py`
fails if a production `os/Containerfile*` enables `dev-overrides` or
`dev-session`.


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

### Linux PDF capture tests

The PDF capture integration test requires native PDFium; text extraction alone
does not qualify page rendering. CI uses Chromium/8057 on x86_64 and aarch64,
matching the Windows runtime version. Both Linux archives and their published
SHA-256 digests are pinned in `os/external-assets/github-assets.tsv`.

The builder stages the runtime from its verified external asset context. The
affected-PDF and nightly test workflows also stage that same runtime for older
or explicitly selected builder images without changing installed OS packages.
To prepare the native runtime in a disposable local directory:

```sh
asset=$(bash tools/ci/install-pdfium.sh --asset-name)
./os/dev/cache-github-assets.sh "$asset"
export TILO_PDFIUM_LIB=$(bash tools/ci/install-pdfium.sh os/external-assets/cache /tmp/tilo-pdfium/8057 os/external-assets/github-assets.tsv)
cargo test --locked -p pdf-viewer --test capture
```

The installer verifies the archive before writing, retains its license files,
and requires the shared library to load on the native architecture. The capture
test additionally observes the production worker's raster backend before testing
unavailable imported content and its later arrival. Missing PDFium fails rather
than skipping coverage or qualifying the text fallback as a raster result.
