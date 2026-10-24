# Development Guide

Build dependencies, local development and testing. The project's rules, and the
check that enforces each, are in [`docs/policies.md`](docs/policies.md); `AGENTS.md`
is what agents working here are told.

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
`tilo device control`. The built-in guide (`tilo device guide`) covers concepts, swapping,
driving, storage, platforms and troubleshooting; its source is
`crates/dev/tilo-vm/guide/`.

Image-level changes (`os/`, units, packages) move the VM onto a new image
instead: `tilo device vm upgrade --from sha:<commit>` for any commit CI has built,
`--from local` for one that is not pushed yet (`tilo device guide images`). t
builds the OS and dev images of any tree (`t test os-dev-image-x86_64`, likewise
`os-image-<arch>` and `os-board-<board>`) and gates installed images in
`os-acceptance-<arch>`. Every night `publish-os-dev-<arch>` publishes main's head once that
gate passed (`t dispatch publish-os-dev-x86_64 --ref main` does it now):
`tilo-ci:main-<sha7>-<arch>`, the dev image `tilo-vm:main-<sha7>-ci` and its disk, and moves
`tilo-vm:main-ci` and `tilo-vm:latest` forward.

To run a main dev image on a physical x86_64 (or generic aarch64) machine, write its
published disk to a disk of at least 64 GB and boot it; it follows `tilo-vm:main-ci`,
so `sudo bootc upgrade && sudo systemctl reboot` moves it to main's newest dev image.
The disk needs a forge token with package read access:

```sh
tag=main-<sha7>-ci
base=https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk/$tag
curl -fL -H "Authorization: token $FORGEJO_TOKEN" -O "$base/tilo-vm-$tag-x86_64.qcow2"
curl -fsSL -H "Authorization: token $FORGEJO_TOKEN" "$base/tilo-vm-$tag-x86_64.qcow2.sha256" | sha256sum -c -
sudo qemu-img convert -p -f qcow2 -O raw tilo-vm-$tag-x86_64.qcow2 /dev/sdX
```

The [Fedora 44 disposable-VM loop](#fedora-44-disposable-vm-loop-legacy)
(`dist/dev/fedora44-vm-iterate.sh`) is the legacy path until slots cover what it
does; `tilo device guide fedora-loop` maps its commands.

### Board dev images on real hardware

A board dev image is the board layer (`os/Containerfile.board`: firmware, device
tree, initramfs) on the same commit's aarch64 dev image. It is sealed: bootc hands
the kernel a device tree only from a UKI's `.dtb` section, so the image carries a
UKI, which keeps the device tree across updates. Every night t builds main's head and
publishes it once `os-board-<board>`, `os-board-size-<board>` and `os-acceptance-aarch64`
pass (`publish-os-board-<board>` in `t.toml`); a dispatch does it now, a few hours from
cold:

```sh
t dispatch publish-os-board-lenovo-83ed --ref main
```

| Board | Image of one build | Moving tag (main) |
| --- | --- | --- |
| Lenovo Yoga Slim 7x (83ED) | `registry.tilo.so/tilo/tilo-vm:main-<sha7>-lenovo-83ed` | `registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci` |
| Orange Pi 3B v2.1 | `registry.tilo.so/tilo/tilo-vm:main-<sha7>-orangepi-3b-v2-1` | `registry.tilo.so/tilo/tilo-vm:orangepi-3b-v2-1-ci` |

The moving tag moves only forward along main: a build of an older commit leaves it.
`t test os-board-lenovo-83ed` builds the image and its flashable disk from your tree without
publishing them (`t outputs` gives the OCI archive and `disk/disk.qcow2`). The disk is installed
following the moving tag and replaces the board's durable copy, again only forward along main:
64 GB for the Lenovo, written to a USB drive or its internal disk and booted from the firmware
boot menu; 16 GB for the Orange Pi, written to an SD card of at least 16 GB (its root grows on
first boot). `<board>` is `lenovo-83ed` or `orangepi-3b-v2-1`:

```sh
base=https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk-latest/aarch64-<board>
curl -fLO "$base/tilo-vm-<board>.qcow2"
curl -fsSL "$base/tilo-vm-<board>.qcow2.sha256" | sha256sum -c -
sudo qemu-img convert -p -f qcow2 -O raw tilo-vm-<board>.qcow2 /dev/sdX
```

The installed system follows the moving tag on its own: the update timer stages each new
build, what can apply live does, and a board restarts into the rest while nobody is at it.
To take a build now:

```sh
sudo bootc upgrade && sudo systemctl reboot
# or one particular build:
sudo bootc switch registry.tilo.so/tilo/tilo-vm:main-<sha7>-<board> && sudo systemctl reboot
```

A board dev image carries tilo-vmd, sshd and the toolchain, so the machine is a
`tilo-vm` target like a dev VM: register it once, then swap, roll back, take
pictures and move it between images with the same commands (`tilo device guide
devices`). Components never go onto it by hand (no `bootc usr-overlay` and
copies into `/usr`): nothing could roll that back.

```sh
tilo device vm add yoga --ssh <you>@<address> --key <your key>   # checks SSH, root through sudo, tilo-vmd, toolchain, bootc
tilo device vm use yoga
tilo device slot create --headless && tilo device swap shell         # your build in your slot; tilo-vm rollback undoes it
tilo device shot -o target/yoga.png                              # tilo-vm seat claim puts it on the built-in display
tilo device vm upgrade --from sha:<commit>                       # that commit's image for this board, then a reboot
```

The board job checks the Lenovo image's audio stack (topology, DSP firmware,
UCM profile: `os/tests/board_audio_contract.sh --root`), but only the laptop
proves the sound. After booting a build that changes it, `wpctl status` lists a
speaker sink and a microphone source for Built-in Audio, and
`journalctl -b -k | grep -i -e q6apm -e wsa884x` shows no errors. The four
speakers run without DSP speaker protection, as upstream ships them: test them
at low volume.

## Routine delivery workflow

How changes reach `main`, `stable` and releases, and how to test them, is in
[docs/t.md](docs/t.md).

### Packaging and publication

After a push to `main`, app packaging selects directly changed apps and apps that depend
on changed shared crates, then reuses unchanged collection members. PRs run the
selection plan without release-mode packaging. Shared runtime or toolchain changes
can still select every app; see [Preview affected applications](#preview-affected-applications)
for how selection and shared runtime compatibility interact. Use **Release / apps** manual
dispatch for a development artifact, selecting the smallest applicable app and
architecture set and leaving `publish` off unless publication is intended.

For an independent Browser engine update, dispatch **Release / apps** on `main`
with `engine_runtime_only=true`, `apps=browser` and the needed native architectures.
This builds the digest-pinned CEF engine and the package tools, verifies native ELF
architecture and hardening, and seals the runtime the Browser manifest names.
`publish=true` uses the existing runtime/app signing identities and publisher OIDC;
`publish=false` signs with disposable test identities and performs no registry write.
The mode rejects collection qualification, coordinator and builder overrides. It
builds neither the OS image nor the app collection. Changing engine code requires a
new runtime version in the SDK, Browser manifest and OS recipe before publication.

The explicit native pipeline proof uses no release credentials:

```bash
cargo run --locked -p runtime-bundle --example browser_runtime_release
```

Use `python3 tools/apps/browser_runtime_release.py proof-cargo` with Cargo or a
configured Cargo-compatible native builder selected through `CARGO` or
`--cargo-command`; repeat `--cargo-argument` for executable-specific arguments.
Source-only builders may omit Git metadata; this launcher
admits only a clean local commit and passes its actual commit object. The builder
recomputes the Git tree from mirrored bytes, executable modes and symlink targets
before compiling and sealing. The snapshot proof refuses CI events, publication
and signing identity variables; production release jobs retain their Git guards.
Its output is `target/browser-runtime-release-proof-<source-commit>`; archive/blob verification uses
the same canonical tools as publication. The source must be clean and committed.

The app registry is publicly reachable at `https://apps.tilo.so`. Main-branch
app publication now submits each newly built immutable tapp to it; separate collector
and networkless signer jobs independently verify accepted requests before advancing
the signed catalogue. Forgejo package publication remains a distinct artifact channel
and alone does not make an app discoverable. [Publishing with t](docs/publishing-on-t.md)
says what is still to move.

OS releases are dispatched on stable or a release tag (`t dispatch
publish-os-release --ref stable`, docs/release-pipeline.md). macOS setup packaging,
cask publication and installation tests remain in **Release / imager** but are
disabled during alpha; tags do not enable those lanes.

## Measurements

How performance and footprint are measured and gated, and what to do when a gate
fails, is in `docs/measurements/`: [performance KPIs and A/B runs](docs/measurements/performance.md),
[app package size](docs/measurements/app-package-size.md),
[runtime size](docs/measurements/runtime-size.md),
[OS image size](docs/measurements/os-image-size.md) and
[build speed and the compilation cache](docs/measurements/build-speed.md).

## Fast checks before assembly

For a native integration test without a full workspace run, ask t for one
test or one package's lints; it runs them on its warm builds, uncommitted
changes included (docs/t.md):

```sh
t test test-x86_64/shell --test 'concept_parity_spec::overview_add_workspace::'
t cargo check -p shell --tests
t test clippy/shell
```

Jobs someone waits for already run at high priority; `t cargo` takes no
`--priority`. The shell's tests run on x86_64 alone (`test-x86_64/shell`);
`t suggest --all` lists every check's name.

`clippy/<package>` is CI's lint gate (all targets, `-D warnings`, the workspace
lint table), including for the Linux-only code a Mac does not compile (`shell`,
`tilo-relay`, `launcher`).

On a Mac, `cargo check --locked -p shell --tests` and `-p tilo-relay --tests`
compile locally. Code that needs Linux (the Wayland compositor state, `ush`,
namespaces, netlink, `/proc`) and the tests that drive it sit behind
`#[cfg(target_os = "linux")]`, so a passing Mac check is a quick first gate,
not a replacement for the Linux run. A test that needs only portable code
should stay ungated.

Run these from the checkout on Linux (including WSL), using Python 3.11+:

```sh
tools/policy/run.sh
python3 tools/dev.py check installer
python3 tools/dev.py check app-packaging
```

Give unrelated tasks their own worktree before starting validation:

```sh
python3 tools/dev.py worktree music-fix --base origin/main
```

The command prints the new path, branch and exact source commit. It creates the
worktree in `.worktrees/` beside the shared Git directory, so invoking it from a
temporary checkout does not place another checkout on `/tmp`'s user quota. It
uses committed source; existing uncommitted edits stay where they are. An
existing destination or branch is an error.

Each task has its own Cargo target directory and remote source-sync lock. Remote
Cargo drop-ins can use the shared Git identity to seed dependency artifacts and
reuse compiler/download caches. Never share `target/` or set `CARGO_TARGET_DIR`. Run related checks
sequentially within a task, combine related test targets where appropriate, and
keep required integration checks. For diagnostics-only builds/checks use
the remote Cargo drop-in's option to skip artifact downloads; leave fetching
enabled for rust-analyzer and request artifacts when needed locally. Heavy non-Cargo work runs on the
builder through SSH; keep only orchestration and small checks on the laptop.

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

Signal working-profile custody has an explicit native qualification feature:
`cargo test --locked -p shell --lib --features signal-profile-native-test working_profile::tests`
and `cargo test --locked -p shell --lib --features signal-profile-native-test verified_enrollment_ram_profile_native_qualification`.
These execute the production materializer, not a substitute memory policy. They
require a private tmpfs whose opened mount reports `noswap`, or a host whose swap
is absent or entirely zram without disk writeback. Otherwise the fixture attempts
a disposable user/mount namespace; kernels can refuse `noswap` on unprivileged
mounts, in which case qualification fails. The fixture does not change host swap
or trust settings. Ordinary custody tests still cover encrypted storage and the
original protected grant; they do not qualify native working-profile custody.
A provider lease handed to the launcher cannot use the prelaunch cancellation
checkpoint. The strict service launcher requires a successful exit of the actual
private PID-namespace init before collecting the newest bounded working image.
Forced shutdowns preserve the interrupted marker. Signed-runtime qualification
remains a separate acceptance requirement.

CI's repository policy tests discover the same app-packaging tests, including
the build-to-collection handoff regression.
For an actual installer VM cycle with an already prepared medium, continue using
`os/dev/test-installer-cycle.sh`. To run a change in the Tilo OS image, use
`tilo-vm` ([Dev VMs](#dev-vms-tilo-vm)); for the disposable Fedora desktop
development session use `dist/dev/fedora44-vm-iterate.sh`.

Source builds prepare the pinned font catalog during compilation, outside the
source tree. Python 3.11+ is required. The first build downloads the exact reviewed
fonts; subsequent builds verify the cached bytes. Prewarm with
`python3 tools/font-catalog.py root crates/sdk/ui-core/assets/fonts --scope fixtures`.
Set `TILO_FONT_ASSETS_OFFLINE=1` (or `CARGO_NET_OFFLINE=true`) to require a warm
cache, or supply a complete matching catalog with `TILO_FONT_CATALOG_ROOT`.
See [font artifact preparation](crates/sdk/ui-core/assets/fonts/ARTIFACTS.md).

A Cargo drop-in compiling on another machine prepares the catalog there. If the
fetched executable will run locally, use the client's generated-artifact fetch
facility to bring `target/font-artifacts/catalogs` back as part of the build,
preserving its relative path. Fetching an executable alone does not bring font
data, license notices or provenance back. With a custom cache location, preserve
the catalog path compiled into the executable.

Verify the received catalog locally without a network using
`python3 tools/font-catalog.py root crates/sdk/ui-core/assets/fonts --scope fixtures --offline`.
Tests and executables running on the builder use its catalog directly.
Applications do not download fonts at runtime.

For optimized application iteration, use the existing `thin-app` profile:

```sh
cargo run --locked --profile thin-app -p ebook
```

This also builds standalone apps: no `thin` feature or dynamic SDK is required.
It keeps optimization level 3, omits whole-program LTO, and compiles `tilo-ui`
with sixteen codegen units. `cargo run --release -p ebook` uses the shipping
profile's fat LTO and one codegen unit; every app edit repeats that final
whole-program optimization even when the toolkit is fresh. Use `--release`
for shipping-profile acceptance and measurements. Keep the same worktree and
profile between edits so Cargo can reuse their outputs. The first `thin-app`
build needs its own artifacts; it is not a warm edit measurement. The profile
remains non-incremental and eligible for sccache.

For measured Rust edit feedback on the remote builder, run one scoped command:

```sh
python3 tools/rust_feedback.py check -p tilo-ui --lib
python3 tools/rust_feedback.py test -p shell --test it --filter interaction_test::
```

This uses `$CARGO` (or `cargo`) and the committed lockfile. Choose a Cargo
drop-in with `--cargo-command`; remote-only policy belongs to that command. The receipt under
`target/rust-feedback/` records client elapsed time, first observed diagnostic
and artifact, rebuilt/fresh Cargo units, source identity and actual test results.
Checks use Cargo JSON; tests retain the project's runner (including nextest) and
record its summary. A test filter matching no executed tests fails the feedback
command. Missing Cargo messages fail a check. Raw output remains live and is not
saved in receipts; ambient environment values and diagnostic contents are not collected.
An existing receipt cannot be overwritten; use `--out <new-file>` to name one.

Keep the same worktree, target, package/feature scope and compiler configuration
for baseline/edit/repeat measurements. Source hashes include tracked edits and
untracked files; a source change during the command invalidates its result.
The timestamps include remote transport/buffering; the requested-package artifact
is distinguished from earlier dependency artifacts. First artifact availability
is not linking time or a passed test. Queue, link and per-run cache timings remain
unknown when not observable. A remote builder's metrics supply shared contention and cache counters, not an
isolated invocation's hit rate. Use Cargo's
[`--timings`](https://doc.rust-lang.org/cargo/reference/timings.html) report for
unit/concurrency inspection; its HTML is a human report, not a stable JSON API.

Batch related small targets in one invocation instead of admitting each
separately. Repeat `-p` or `--test`, and combine `--lib` with named integration
targets to retain their union. For example:

```sh
python3 tools/rust_feedback.py test -p tilo-clock -p tilo-advisory-lock --lib --test lock
```

This keeps the same Cargo/test runner and selected tests while avoiding a second
admission and client setup. It does not bypass admission or replace final
integration checks. A larger feature/dependency union can cost more compilation,
so batch related checks rather than the workspace.

Start one heavy validation per task at a time. The builder owns admission; do
not reserve slots or poll queued jobs. A fresh worktree's seed/sync cost can
exceed a small leaf compile, so compare warm runs separately. A focused result
is only feedback: retain affected-package integration checks and CI, and for
shell changes finish with `$CARGO check --locked -p shell --tests`.
[`cargo check`](https://doc.rust-lang.org/cargo/commands/cargo-check.html) omits
code generation and can miss errors that appear in a test/build.

For a failing check, `t log <check>` prints its whole log (docs/t.md).

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
git clone ssh://git@code.tilo.so/tilo/tilo.git "$HOME/src/tilo-linux"
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
| `gstreamer-1.0` `gstreamer-app-1.0` | `gstreamer1-devel` `gstreamer1-plugins-base-devel` | media apps, and the shell's video (sharing, projection, recording) |
| `autoreconf` `libtoolize` (binaries), `libgcrypt.m4` | `autoconf` `automake` `libtool` `libgcrypt-devel` | `tilo-ntfs-helper` bootstraps its pinned NTFS reader |
| `libclang` (shared library) | `clang-libs` | bindgen for PipeWire bindings; Python 3 checks that it can load |
| `bwrap` (binary) | `bubblewrap` | the launcher's confinement tests drive the real sandbox |
| `rust-analyzer` (binary) | `rustup component add rust-analyzer` | the code editor's language-server test drives a real server |
| `cryptsetup` (binary) | `cryptsetup` | rollback mechanism tests drive a real LUKS2 header |
| `swtpm` `swtpm_setup` (binaries) | `swtpm` `swtpm-tools` | rollback mechanism tests drive an isolated software TPM |
| `tpm2_getcap` (binary) | `tpm2-tools` | rollback mechanism tests exercise production TPM policy commands |
| `xwayland-satellite` `Xwayland` (binaries) | `tools/ci/install-xwayland-satellite.sh` (needs `xcb-util-cursor-devel` `open-sans-fonts`), `xorg-x11-server-Xwayland` | the X11 clipboard tests drive a real satellite bridge with Tilo's clipboard handoff patch; they fail, not skip, without it |

The sandbox, language-server, and rollback mechanism tests require the binary
tools listed above. Install them where Cargo runs, including inside a build
container.

### Native Windows Imager builds

Install the pinned Rust toolchain, a Windows SDK, and the Visual Studio C++
tools for the target architecture. Windows ARM64 needs the Build Tools
component `Microsoft.VisualStudio.Component.VC.Tools.ARM64`; installing only
the x64 C++ tools does not provide ARM64 target libraries.

From an ARM64 host, run:

```powershell
cargo test -p imager --lib --features host-media-writer --target aarch64-pc-windows-msvc --locked
cargo test -p tilo-ui --test hosted-platform --target aarch64-pc-windows-msvc --locked
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
cargo run -p video-editor
```

The editor enables GStreamer playback and export by default. Homebrew's
`gstreamer` formula supplies the plugins and GLib dependency as well. Check
that Cargo can discover the libraries with:

```bash
pkg-config --modversion glib-2.0 gstreamer-1.0 gstreamer-app-1.0
```

### Android app host

`tilo-app build --platform android` ([docs/android.md](docs/android.md)) needs
the Android SDK (build-tools 36.1.0, platform android-36, platform-tools), NDK
r29 with the two Android Rust targets, a JDK 17 or newer and `zip`/`unzip`.
`tilo-app doctor --platform android` names each missing piece with its fix,
and `tilo-app setup android` installs the SDK side into `$ANDROID_HOME`
(default `~/Android/Sdk`) and adds the Rust targets.

The builder image and the Fedora 44 development image
(`dist/dev/Containerfile.fedora44`) carry the NDK at `/opt/android-ndk`, with
the `CARGO_TARGET_*_LINUX_ANDROID_LINKER`, `CC_*`, `CXX_*` and `AR_*`
variables that point Cargo at its clang; `tools/ci/install-android-ndk.sh`
does the same in a CI or image build. Elsewhere the CLI finds an NDK under
`$ANDROID_NDK_HOME` or the SDK's `ndk/` and sets those variables itself.
Testing needs `adb` and an `x86_64` emulator image or a device.

### Native Windows development

Use the pinned Rust MSVC toolchain, Visual Studio C++ build tools for the host
architecture. The native
ARM64 toolkit, Calculator, Notes, Calendar, Contacts, Sheets, Slides, Weather and
Scratchpad, Files, Word Processor, Terminal, PDF Viewer, Books, Clocks, Photos,
Hex Editor, Code Editor, API Tester, Diff and Merge, Snippets, Database
Client, Git Client and Feeds have been built and run on Windows.
Other apps are still being ported; a workspace-wide build currently includes
Linux services and is not a Windows
application build command.

From PowerShell at the workspace root:

```powershell
capnp --version
cargo check --locked -p ui --tests
cargo run --locked -p calculator
cargo run --locked -p notes
```

Windows rendering defaults to Direct3D 12. `WGPU_BACKEND` remains an explicit
developer override. `ui::portal` opens the native Open, Save, multiple-file and
folder dialogs (`tilo-dialogs` selects the Common Item Dialogs on Windows, the
panels on macOS and the XDG portal on Linux; apps enable no feature), with
results delivered on the existing picker channel. To exercise the host picker
without opening or modifying a document:

```powershell
cargo run --locked -p tilo-dialogs --example pick -- open
# Also accepts: multiple, directory, save, uri URL
cargo test --locked -p tilo-dialogs
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

Private identity files and local account-vault keys use the shared
`tilo_files::native::private` module. On Windows, files are created with a
protected, current-user-only DACL; reads validate the opened file's owner, access rules and reparse status. Existing
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
cargo test --locked -p tilo-files -p tilo-user-identity -p accounts --lib
```

Apps that need persistent content-addressed blobs without mounting a filesystem
can depend on `fs` with `default-features = false`, as Scratchpad does. This uses
the same content store, durable protection tags and legacy-store migration as
the OS. The default `filesystem` feature adds the namespace, FUSE projection,
service protocols and their integration tests.

```powershell
cargo test --locked -p tilo-fs --no-default-features --lib
cargo test --locked -p scratchpad --all-targets
```

Files supports native local-file browsing and operations, including drive and UNC
paths. Its Start menu entry is **Files** and its Cargo package is `file-explorer`.
The Tilo filesystem projection, received-share transport, and shell-owned Nearby
Send service are not yet available on native Windows; their control operations
return errors. Delete uses
the Windows recycle bin, but in-app recycle-bin browsing and per-item restoration
are not implemented. Projection-dependent screenshot scenarios also fail explicitly.

The `--test-threads=1` runs below work around tests that share process-wide
state (environment, working directory, fixture guards) inside one test
process. On Linux, CI and `scripts/test-local.sh` run each test in its own
process with nextest instead; drop these flags once the tests no longer share
that state.

Run File Explorer tests serially because its fixture guard observes process-wide
filesystem access:

```powershell
cargo test --locked -p file-explorer --lib -- --test-threads=1
cargo test --locked -p file-explorer --test it -- --test-threads=1
```

Word Processor keeps every document in its framework record, so there is no
separate crash-recovery copy. Native tests cover editing, DOCX/ODT/HTML round
trips through a launch on the file and Save As, and PDF/RTF/EPUB export:

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
its intentional placeholder. `cargo test --locked -p pdf-viewer --test it capture::`
exercises this worker path separately from the synchronous library tests.

Books opens EPUBs through its background importer and wakes idle windows when an
import completes. Device-local source paths and watched folders stay outside the
collaborative document. `EBOOK_LIBRARY_DIR` uses Windows `;` path-list separators;
drive letters are preserved. Windows imports remain local unless an absolute
`TILO_WORKSPACE_DIR` explicitly identifies a configured collaborative workspace.
The installer does not provide that workspace service. Books captures accept an
EPUB positional argument and wait for its import to finish.

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
cargo test --locked -p tilo-content
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

Clocks keeps alarms, timers, cities and the stopwatch in its synced resource
record; which clocks float and which deadlines this device has already rung stay
in the app's device-local settings. Screenshot scenarios never ring, schedule or
consume shell commands.
The installer registers `so.tilo.clocks` on the Start menu shortcut and in the
current user's notification display-name registry. The SDK's `notifications`
feature provides Windows toast submission and app-scoped history inspection.
Alerts obey Windows notification settings and Do Not Disturb. Clocks hands timer
deadlines to Windows, so timers continue to notify after the app closes. Pausing
or deleting a timer cancels its pending Windows alert. A device-local receipt records
the accepted deadline so reopening after delivery does not submit it again.
Each profile durably records the timer schedules it owns before submitting
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

For device verification, build `cargo build --locked -p ui-examples --example windows-host`.
The helper accepts `history so.tilo.clocks` to inspect
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
$gstPrefix = 'C:/msys64/clangarm64' # Replace with your installation.
$env:PATH = "$gstPrefix/bin;$env:PATH"
$env:PKG_CONFIG = "$gstPrefix/bin/pkgconf.exe"
$env:PKG_CONFIG_PATH = "$gstPrefix/lib/pkgconfig"
cargo test --locked -p tilo-media --features decode,encode --lib
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

Agent sessions identified by `CODEX_THREAD_ID`, `CODEX_SESSION_ID` or
`CLAUDECODE` require a remote-only Cargo executor. Set `QCARGO_REMOTE_CARGO` in
an untracked developer environment to one executable name or path. The executor
accepts ordinary Cargo arguments and must fail on remote errors rather than
compile locally. It may be any Cargo-compatible drop-in or a local adapter that
configures the chosen remote build service; shell command strings are not
accepted. Other unattended callers opt into this policy with
`QCARGO_REMOTE_ONLY=1` in their own environment.

The shim checks this policy before `QCARGO_BYPASS` and container routing. A
missing executor or a builder failure fails visibly. It sets
`QCARGO_REMOTE_ACTIVE=1` when invoking the executor: a returned Cargo call in the
same policy context is rejected before compilation. Executors should not forward
agent identity, `QCARGO_REMOTE_ONLY` or `QCARGO_REMOTE_ACTIVE` into remote builder
processes, where ordinary Cargo must run locally. Human shells retain their
local controls; metadata, formatting and dependency commands can still run
without compiling. Invoking an absolute Rustup Cargo path bypasses the shim.
This is a routing guard, not an operating-system execution boundary.

Run `python3 tools/tests/test_cargo_shim_routing.py` for recording-stub routing
checks; they never invoke Rust. `tools/tests/cargo_shim.sh` also uses stubs to
verify cache/container environment handling. The separate real-Cargo cache
policy tests belong on the builder, not on a remote-only development laptop.

It also selects the sccache server, which this route requires: sccache's server
executes rustc, so it must share the build's mount namespace or it cannot see that
namespace's headers. distrobox shares the host network namespace and copies the host
environment inward, so without an explicit choice on both sides the container build
reaches the host's server and every compilation misses.

Not needed if the libraries are present natively — a nix/devenv shell removes both
the routing and the sccache-namespace problem, and the shim with them.

### Fedora 44 disposable-VM loop (legacy)

For Tilo OS work, use `tilo-vm` ([Dev VMs](#dev-vms-tilo-vm)); `tilo device guide
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

### Build speed

Share compilation results across worktrees through sccache; target directories
cannot be shared. Changing `build.rustflags` invalidates cached compilations
across worktrees, and bumping `rust-toolchain.toml` costs one fully cold
rebuild, so batch such changes and avoid them during verification runs.
Run-to-run noise on a busy machine is ±20 %: measure on an idle machine. What was
measured not to help (mold, debuginfo and frontend flags, cranelift) and how the
cache is sized: [docs/measurements/build-speed.md](docs/measurements/build-speed.md).

## Disk, and why builds hang

One worktree's `target/` reaches 150+ GB. Delete `target/` from worktrees whose
branches have landed; sccache retains reusable compilation results.

The Linux linker compresses test binaries' debug sections with zlib, keeping
file:line backtraces and reducing disk use. Measurements and why split debug
info is unsuitable with sccache: [debug info in test binaries](docs/measurements/build-speed.md#debug-info-in-test-binaries).

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
cargo test -p tilo-fs
cargo test -p shell --lib room_activity      # scope it; the compile dominates
cargo check -p shell --tests                 # before finishing
```

### The whole suite, as CI runs it

[tools/ci/README.md](tools/ci/README.md#running-pull-request-ci-locally) lists
the local command for each pull-request CI job.

The developer image (`dist/dev/Containerfile.fedora44`) carries the suite's
runtime — GStreamer plugins, fonts, lavapipe, PulseAudio, PDFium, the test
subjects and `cargo-nextest` — from `dist/ci/install-build-deps.sh fedora test`.
`tools/test-env.sh COMMAND...` gives the command CI's test environment: the
clean locale, `HOME` and session of `tools/with-test-environment.sh` (see
[Running tests like CI](#running-tests-like-ci)), lavapipe as the only Vulkan
adapter, cached native NTFS fixtures and a private audio endpoint. CI's test
jobs call it too. The visual goldens run separately on an idle renderer:

```bash
golden='(package(tilo-ui) and test(typography_visual_regression::)) or (package(ebook) and (test(serif_reader_matches_typography_baseline) or test(mathematical_reader_matches_shared_typesetting_baseline))) or (package(shell) and test(e2e_shell_flows::))'
tools/test-env.sh cargo nextest run --workspace --locked --profile ci -E "not ($golden)"
tools/test-env.sh cargo nextest run --workspace --locked --profile ci -E "$golden"
```

The fixtures it caches live in `~/.cache/tilo/test-env` (`TILO_TEST_CACHE_DIR`),
one set per generator revision. On Tilo OS your `tilo-cache-gc.timer` removes a
set nothing has read for 14 days, never one a running test holds the lock of;
`tilo os clean` does it now, `tilo os status` says what went, and
`~/.config/tilo/cache-gc.toml` changes the policy (see
`/etc/tilo/cache-gc.toml`).

### Tests and the process environment

`cargo test` runs a binary's tests as threads of one process, so an
environment variable one test sets is visible to every test running beside it.
A test that points `XDG_DATA_HOME` at a temporary directory and removes it
afterwards also removes whatever value was there before, and a reader on another
thread sees whichever value was set last.

- **Inject the path or store.** A function that resolves a directory from the
  environment should delegate to a variant that takes the directory or the
  store as an argument — `cli::standalone_state_dir_in(home, package)`,
  `DocStore::shared(store, key)`, `RecordSet::open_in(store, package, room)`,
  `persist::open_room_in(store, …)`, `RecordOwners::new(root)`. Tests call that
  variant with an `FsStore` over their own temporary directory and run in
  parallel with no shared state. This is the default for new code.
- **Use `tilo-test-env` only where the environment is the interface** — the
  resolver itself, or a launcher contract such as `TILO_PACKAGE_ID`. Add it as a
  dev-dependency (`tilo-test-env = { path = "…/crates/dev/test-env" }`).
  `ScopedEnv` sets and removes variables and restores each prior value on drop,
  also when an assertion fails; `TestHome` points `HOME` and every XDG base
  directory at a fresh temporary tree. Both hold one process-wide, re-entrant
  lock, so scopes nest and never interleave.
- **Fixture scripts** are written with `tilo_test_env::script(path, body)` and
  run through `Script::exec_argv()` (`/bin/sh <path>`) or `Script::command()`.
  Executing a freshly written file directly fails intermittently with `ETXTBSY`
  when a sibling test forks while the file is still open for writing.

Clippy's `disallowed_methods` (configured in `clippy.toml`, and repeated in
`crates/sdk/ui-core/clippy.toml`) rejects `std::env::set_var`/`remove_var`
calls outside `tilo-test-env`, and the PR lint gate fails on any new site. Use an
injected path or `ScopedEnv`; only production code that must change its own
environment (for example clearing systemd's `LISTEN_*` before any thread starts)
takes a narrow `#[allow]` whose reason says why that is safe there.

### Running tests like CI

CI runs tests with `cargo nextest run --profile ci`: every test in its own
process, no retry, a JUnit report in `target/nextest/ci/junit.xml`. A failure
on the first run fails the job. The summary still lists slow tests, and any
case that only passed on a retry if a profile turns retries back on; treat
that as a bug, not as noise. Each run goes
through `tools/with-test-environment.sh` (directly, or via `tools/test-env.sh`),
which sets `LANG=C.UTF-8` and
`TZ=UTC`, points `HOME` and every XDG base directory at a fresh temporary tree
and removes the desktop session (D-Bus buses, display, keyring socket; see
[Tests never touch your desktop](#tests-never-touch-your-desktop)).
GPU tests use Mesa's llvmpipe Vulkan driver and audio tests an isolated
PulseAudio endpoint (`tools/with-headless-audio.sh`).

Reproduce that locally with:

```bash
scripts/test-local.sh                            # the workspace, visual goldens excluded
scripts/test-local.sh -p tilo-fs -p tilo-ui           # selected packages
scripts/test-local.sh -p tilo-fs -- checked_restore   # arguments after -- go to nextest run
scripts/test-local.sh --goldens                  # only the visual goldens
CARGO=my-cargo scripts/test-local.sh -p tilo-fs  # any cargo drop-in compiles, run here
```

It compiles in your environment, then runs the compiled tests (through
nextest's build metadata, without Cargo) from a cleared one (`env -i`), so a
test cannot see your identity, preferences, installed apps, locale or live
session. A test that passes under plain `cargo test` but not
here depends on one of those.

### Tests never touch your desktop

Every test runs as if on a machine with no desktop, whoever runs it and
however, because a test that reaches a live session pauses its media, opens
file choosers on it or ends it:

- **Cargo's runner.** `.cargo/config.toml` sets `tools/cargo-runner.sh` as the
  Linux runner, so `cargo test`, `cargo nextest` and `cargo bench` start every
  test binary (and doctest) inside `tools/with-test-environment.sh`. That
  gives it a fresh `HOME` and XDG tree, no display, and points
  `DBUS_SESSION_BUS_ADDRESS` and `DBUS_SYSTEM_BUS_ADDRESS` at sockets nothing
  listens on, so the session bus, portals, media players, notifications, the
  secret service, the systemd user manager and logind are all out of reach.
  An enclosing test environment (CI, `tools/test-env.sh`) is kept. `cargo run`
  targets start unchanged.
- **A test binary started by hand** skips the runner. A test that starts a
  compositor or other desktop-facing process calls
  `tilo_test_env::require_isolated_desktop()` first, which refuses to go on
  outside the test environment. Run such a binary as
  `tools/with-test-environment.sh target/debug/deps/<test>`.
- **Tests that need a bus bring their own**: a p2p `UnixStream` pair (see
  `crates/session/shell/tests/portal_test.rs`) or a private `dbus-daemon`, and
  hand that connection or address to the code under test.
- **Shell and Python suites** run through `tools/ci/suites.py`, which wraps
  each in `tools/with-test-environment.sh`. A shell test that runs session
  code sources `tools/desktop-isolation.sh`: `tilo_test_isolate "$@"` re-runs the
  script in the test environment when started directly, and
  `tilo_stage_script SRC DEST NAME=PATH...` stages a packaged script from the
  tree with its command variables rewritten, failing when the copy still runs
  an installed `/usr/libexec/tilo*`, `/usr/bin/tilo-*`, `systemctl` or
  `loginctl`.
- **Hardware state** (backlight, keyboard light, sysfs) is not covered by the
  environment. Tests pass a fixture root (`set_brightness_in(root, …)`) or a
  recording `os_actions::Dispatcher`.

**Capability-gated tests.** A test that needs a GPU adapter, audio output,
media codecs, `/dev/fuse` or fanotify uses `tilo_test_env::require_capability!`
instead of printing "skipping" and returning:

```rust
let mut renderer = require_capability!(
    Gpu,
    try_new_renderer(width, height),
    "no wgpu adapter available for offscreen render"
);
```

It skips with a message where the capability is missing, and fails when
`TILO_REQUIRE_<CAP>=1` (`GPU`, `AUDIO`, `MEDIA`, `FUSE`, `FANOTIFY`, `CHOWN`,
`USERNS`) says the environment provides it. CI test lanes set `TILO_REQUIRE_GPU`
and `TILO_REQUIRE_AUDIO`, the FUSE lane `TILO_REQUIRE_FUSE`; the
`sandbox-host-ids` micro-VM lane sets `TILO_REQUIRE_CHOWN` and
`TILO_REQUIRE_USERNS` for the fixtures that give files to another user or enter
a user or mount namespace. `scripts/test-local.sh` sets the first two when
llvmpipe and pulseaudio are installed. Existing
`eprintln!("skipping …"); return;` sites move to the macro when their file is
next changed.

**The native data plane (Tier 1).** `fs --test native_tier1` and
`fs-native --test it` need root, two dedicated Btrfs volumes and a kernel with
pre-content fanotify events (6.14 or later, `CONFIG_FANOTIFY_ACCESS_PERMISSIONS`).
`tools/native-tier1.sh` builds both as you (with `$TILO_CARGO`, default `cargo`),
makes two loop-mounted Btrfs stores, runs every test once as root through
`sudo -n` with `TILO_REQUIRE_FANOTIFY=1`, and removes the stores and mounts
again:

```bash
tools/native-tier1.sh                                        # both suites
TILO_NATIVE_TIER1_TARGETS=fs:native_tier1 \
    tools/native-tier1.sh sqlite --nocapture                 # one scenario
```

A rootless container (a distrobox, a build container) cannot run them: a
rootless user namespace gets neither a filesystem mark on a Btrfs store nor a
pre-content group. `sqlite3` and `git` must be on your `PATH`, which the root
run inherits.

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
with the session entries and PAM stack, and runs `rpmbuild`. Compiling always
goes through the `cargo` on your `PATH`; on an image-based host without
`rpm-build`, only `rpmbuild` runs in the container named by `.build-container`
(see "If you build in a container"), found from any worktree of the checkout.
Without either, the script stops before compiling and says what to set up. Two
packages land in `dist/rpm/build/RPMS/x86_64/`:

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
# Arm the developer entry only while you explicitly need it, then disarm it.
sudo dist/rpm/usroverlay-install.sh --arm-devel
sudo dist/rpm/usroverlay-install.sh --disarm-devel
sudo dist/rpm/usroverlay-install.sh --uninstall
```

To move an existing install to the current checkout, use the helper instead. It
builds, names the packages after the commit (`tilo-session-0.1.12-0.202609271530.git5764267571`),
refuses RPMs left over from another commit, and swaps whatever `tilo-session*`
packages are layered in one `rpm-ostree` transaction, keeping `-devel` only if
it was there:

```bash
dist/rpm/host-update.sh               # build HEAD and stage it for the next boot
dist/rpm/host-update.sh --apply-live  # also make it the running /usr; just log out
dist/rpm/host-update.sh --no-cargo    # repackage binaries already in target/release
```

Each step reruns on its own: after a failure, rerun the same command. The app
collection (`dist/rpm/build/app-release`) is kept while it was built from the
same commit and uncommitted changes, and rebuilt otherwise, `--no-cargo`
included.

`--apply-live` refuses while a Tilo session is running on the machine. Builds go
through the `cargo` on your `PATH`, so any cargo drop-in on it is used.

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
its own socket, and both stayed interactive afterwards. Dev and test builds keep
application code unoptimised, but optimise Vello's encoding, geometry, layout
and wgpu's command-processing crates. Assertions, overflow checks, GPU validation
and line-table backtraces stay enabled: using `cargo build` should not require
turning debugging off to interact with the desktop.

To compare lock-screen typing without locking your desktop, use the same GPU,
output size and simulated refresh for both binaries:

```bash
python3 scripts/perf/lock_screen_bench.py --shell-bin target/debug/shell --label dev
# Build on the remote builder, as for other compiling Cargo commands.
cargo build --locked --release -p shell --bin shell --features dev-overrides
python3 scripts/perf/lock_screen_bench.py --shell-bin target/release/shell --label release
```

The benchmark creates a private headless session, types only synthetic characters
and never submits a password. It checks every key reached the locked field and
requires both rendering and retirement samples. Results under `target/lock-bench`
separate input-to-queue, simulated frame retirement, CPU submission and GPU
completion waits; none is a measurement of a physical panel's photon latency.
The benchmark cycles typing and backspace below the field's dot cap so every
sample visibly changes the image, and reports raster pixels and dense frames.
Password-only edits should repaint the bounded password part, not the full
output. The first lock frame, either lock edge, display changes and unproven
scene differences remain full-frame boundaries. The nested backend retains its
own render target and checks the same predecessor relation before sparse work;
it never relies on a swapchain image retaining pixels.

`--backend drm` is not an option from inside a running session: `libseat` will
not hand you a seat the running desktop owns, and fails with `ENOSYS` (error
code 38), which is a refusal, not an unimplemented backend.

### Nested desktop with Tilo's own apps

A bare `shell --backend winit` has an empty launcher: it lists only installed,
signed `.tapp` packages. One command builds the shell and the apps on a t
server (t.toml's `nested-session` check, downloaded with `t outputs`: the thin
apps need the exact `libruntime_dylib.so` of their build, which `t cargo build`
does not copy back), packages and signs the apps with a development key,
installs them with their runtime bundle into a private store, and starts the
nested shell beside the model gateway. The machine running it needs `t` and no
Rust toolchain:

```bash
dist/dev/nested-session.sh                                  # core apps + agent
dist/dev/nested-session.sh --apps "notes chat agent"        # a smaller set
dist/dev/nested-session.sh --headless --fresh-desktop       # no window at all
```

Everything lives in `~/.local/state/tilo-nested` (`--state DIR`): a scratch
HOME and XDG tree, the app store, the runtime, the publisher key and a
throwaway identity (re-minted before its 24-hour expiry). The shell gets a
private `XDG_RUNTIME_DIR` and D-Bus session, so it cannot collide with your
desktop or another Tilo session. Re-running repackages only the apps whose
binary, manifest or surface changed. The script prints the `tilo-control`
command line for driving the session; logs are under `logs/` in the state
directory. Configure a model provider in the nested Settings app before
asking the agent to act. The browser installs without its Chromium engine and
offers to fetch it itself. `--fresh-desktop` forgets saved workspaces and
windows but keeps the identity and the installed apps. The window opens at
1280x800 logical pixels at the host's scale, which leaves the task centre room
for its cards; `TILO_WINIT_SIZE=WxH` (physical pixels) overrides it.

`--headless` starts the same stack on the offscreen backend and needs no
Wayland session: nothing appears on your desktop, and you drive it only
through the printed `tilo-control` line (`actions invoke`, `key`, `type`,
`click-at`, `read APP`, and `snapshot FILE`, which renders the 1280x800 output,
or `TILO_HEADLESS_SIZE`). Use it for scripted and agent end-to-end runs.

Run it on the host itself, not in a toolbox or another container: a Tilo
desktop admits a Wayland client it did not launch only when the client shares
its user and mount namespaces (anything else could be a sandboxed app reaching
past its launch), so a container's nested window is refused with "Connection
reset by peer", and the script says so before it starts. Nothing it runs needs
a toolchain or a package the host lacks; each shell gets a private D-Bus from
`dist/dev/session-bus.sh`, with the build's own `dbus-daemon` where the host
has none (Tilo OS runs the user bus on dbus-broker).

On the first start the script makes the Home workspace a native room: agent
tasks act with a room's native authority, and the throwaway person has no
second device to govern one. It runs the packaged-desktop governor fixture,
which signs an invitation with the session's own device, and the shell
redeems it at boot; `logs/native.log` says when the room is native. Later
starts restore it from the saved desktop, and `--fresh-desktop` enrols a new
one. Admit an agent to the room (`so.tilo.desktop` `add-agent`) before queuing
tasks for it.

### Two people in one workspace: a nested pair

To try collaboration on one machine (presence, shared apps, calls), start two
nested sessions with two throwaway identities, together in a community
workspace the way two computers are in tilo.coffee's Lounge:

```bash
dist/dev/nested-pair.sh                          # two windows, side by side
dist/dev/nested-pair.sh --apps "chat notes"      # fewer apps, faster to package
dist/dev/nested-pair.sh --headless               # no windows; tilo-control only
dist/dev/nested-pair.sh --fresh                  # two new people
dist/dev/nested-pair.sh --fresh-site             # a new Lounge, nothing kept
```

It runs a local site the way tilo.coffee runs: the room service in
persistent mode with its workspace host (`tilo-relay`), and an `entry.json`
naming that host's room as the site's home workspace, "Lounge", which opens
in Chat, with the room service's door admitting people
(`crates/hosting/tilo-room-service/dev/run.py --no-bundle`, on ports 8680 and
up; `--ports BASE` moves them). Both binaries come with the nested-session
build. Each session's community site is that local site (`nested-session.sh
--community SITE`, which stands in for `/etc/tilo/community.toml`), so each
joins the Lounge through the door exactly as a Tilo computer joins
tilo.coffee's, and holds it over the host's routes. Once both are in, the
script takes each person to the Lounge and has them show where they are (a
public workspace starts at "that I'm here", which shares no pointer); the
avatar menu changes it back.

State lives in `~/.local/state/tilo-nested-pair` (`--state DIR`): A in `a/`,
B in `b/`, the site in `site/` (its log is `site/site.log`). Identities and
the Lounge persist between runs. The script prints a `tilo-control` line for
each session; closing either window or Ctrl-C stops both and the site.
`--ready FILE` writes both control sockets, the site and the room as JSON
once both people are in the Lounge.

The presence acceptance drives such a pair headless and checks, through each
session's control socket and the actions people and agents use, that each
roster shows the other person, that a Chat message A posts is read on B,
that A's pointer lands on B's tile for the same window at the same offset,
that following A moves B's view to A's window and with A, and that A's
cursor chat shows beside A's pointer on B:

```bash
dist/dev/nested-pair-check.py                    # build, start a fresh pair, check, stop
dist/dev/nested-pair-check.py --no-build         # from the last build
dist/dev/nested-pair-check.py --attach READY     # a pair started with --ready READY
```

It prints one line per check and keeps both sessions' state dumps and
snapshots in `--out` (a temporary directory by default). The network half of
the pointer check also runs on t, without a compositor:
`shell::network::site_join_tests::two_computers_in_a_sites_workspace_see_each_others_pointer`.

### Edit → build → log into your own build

After installing `tilo-session-devel`, or while the transient developer entry is
armed with `usroverlay-install.sh --arm-devel`:

```bash
cargo build -p shell --features dev-session
echo "$PWD/target/debug/shell" > ~/.config/tilo/dev-session
chmod 600 ~/.config/tilo/dev-session
```

Then log out and pick **"Tilo (my build)"**. Every iteration after that is
`cargo build -p shell --features dev-session`, log out, log back in — the config
file keeps pointing at the same path.

For a transient host trial, disarm the entry when that development run is over:

```bash
sudo dist/rpm/usroverlay-install.sh --disarm-devel
```

This removes only `tilo-dev.desktop` and its developer wrapper; the packaged
**Tilo** session remains installed. A persistently layered
`tilo-session-devel` package remains an explicit package-manager choice and must
be uninstalled through the package manager when no longer wanted.

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
- **No `/etc/tilo/desktop.toml`.** The host package omits it, so the compositor
  uses its defaults unless `~/.config/tilo-desktop/desktop.toml` exists.

`/var/workspace` is the image's `fs.service` mount. The Fedora host payload
instead packages the existing daemon/admin tool and the user service
`tilo-files-workspace.service`, using `~/.local/state/tilo/files/mount` on the
standard XDG profile. It starts only for saved, signed and Files-authorized
primary state; package installation never substitutes an empty namespace for
missing recovery data. The host shell exports that root only through the
launcher's existing capability filter. See [host Files preparation and
recovery](os/docs/peer-sync.md#fedora-primary-files-service-and-explicit-first-run).
The Settings Storage pane's host socket wiring is still pending; its image
socket does not address this user service.

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
`tilo device swap` builds by default; `tilo device swap --profile dev` trades that
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

## Dependency features: workspace-hack

Cargo resolves third-party features per `-p` selection, so `cargo test -p a`
and `cargo test -p a -p b` can compile the same dependency twice with different
features and share nothing in `target/` or sccache. `crates/workspace-hack`,
generated by [cargo hakari](https://docs.rs/cargo-hakari), depends on every
third-party crate the workspace builds with more than one feature set, with the
union of those features, and managed members depend on it, so every selection
builds each dependency once. Settings and the deliberate exclusions live in
`.config/hakari.toml`.

After adding, removing or changing a dependency or its features, regenerate and
commit the result with your change: `t fix hakari` does it on the server, or
locally (`cargo install --locked cargo-hakari --version 0.9.39` once):

```bash
cargo hakari generate      # rewrite crates/workspace-hack/Cargo.toml
cargo hakari manage-deps   # add `workspace-hack` to new members
```

t's `hakari` check runs `cargo hakari generate --diff`, `manage-deps --dry-run`
and `verify`, and fails until both commands leave nothing to change. Neither
moves a locked version; `Cargo.lock` only gains or loses `workspace-hack`
edges. Never edit between the `HAKARI SECTION` markers by hand.

The hack's feature set is native and does not build for wasm32. A member the
browser runtime also builds for `wasm32-unknown-unknown` names it under
`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` instead; the policy
suite names a member that `manage-deps` gave the unconditional line.

## Git hooks and merge drivers

Turn on the repo's git hooks and merge drivers once per clone:

```bash
python3 tools/dev/install-git-hooks.py
```

The settings go into the clone's config, so every worktree of the clone has them. The installer
verifies the hooks, preserves unrelated hook configuration, and uses worktree configuration when
enabled (then run it in each linked worktree). It refuses an unrelated hooks path or custom
default hooks; arrange explicit chaining rather than overwriting them.

`pre-push` runs t's push gate on a push to `main`, and the merge drivers resolve generated files
in a rebase ([docs/t.md](docs/t.md), "How changes reach a release").

`commit-msg` requires `type(optional-scope)!: description`. Types are `build`,
`chore`, `ci`, `docs`, `dev`, `env`, `feat`, `fix`, `perf`, `refactor`, `revert`,
`style`, and `test`. For example, `fix(shell): preserve window identity` or
`feat(ui)!: expose resource actions`. Bodies follow a blank line and can include
`BREAKING CHANGE:` footers. Git's genuine merge, target-bound revert and
existing-target autosquash messages are accepted; an arbitrary `Merge ...`
subject on an ordinary commit is rejected. Generated sources use ordinary
headers such as `chore(codegen): refresh bindings`.

New or modified staged non-text and non-LF blobs are rejected, even when an
exception entry is added in the same commit. Existing pinned assets are only
admitted unchanged; deletion is allowed. The staged index is inspected, so
partial staging and filenames with spaces are handled correctly. Fast quality
checks all new commits and exact blobs in its comparison range, catching
`--no-verify` commits and intermediate binary additions later deleted. It fails
when history cannot be inspected. Existing exceptions remain migration work.

The lines a commit adds carry no credential, merge-conflict marker or `dbg!`
(`dbg!` only in Rust outside `vendor/`). One definition,
`tools/policy/added_line_policy.py`, serves the hook, `scripts/scan-secrets.sh`
and t's `changes` check, which repeats it on every new commit, so `--no-verify` does
not get past it. A line counts as added when no parent's version of the file
holds it: an existing string does not fire again when its file is touched, and
a merge answers only for its resolution. A credential-shaped string that is not
one needs `example` or `placeholder` on the same line; that is the only
exception.

`pre-commit` checks the staged change the way t's checks would: rustfmt
on the staged Rust files, workspace membership
(`scripts/check-workspace-complete.sh`), the app text and shell copy contracts,
lint `#[allow]` policy, `#[ignore]` reasons, integration-test module
declarations, conflict markers, `dbg!` and secrets. Compilation and tests run in
t's checks (`t.toml`). [`docs/policies.md`](docs/policies.md) maps
every rule to the hook or CI step that holds it.

Committing a merge checks only what the resolution changed. A file staged
exactly as the merged-in side has it already passed that side's checks, so the
hook runs against a temporary index where those files are back at `HEAD`. A
merge of `origin/main` into a branch 300 commits behind (3,400 staged files)
takes about 13 seconds instead of nearly three minutes.

A vendored crate under `vendor/` is upstream's code, carried as it ships: the
added-code checks (`#[ignore]` reasons, `#[allow]` placement, `dbg!`) and
rustfmt skip it. rustfmt also skips a generated source, one with an
`@generated` marker in its first five lines (capnpc-rust bindings, tilo-app
registries); CI's `tools/ci/check-changed-rustfmt.sh` applies the same rule.

The hook refuses a staged `Cargo.lock` change with no `Cargo.toml` beside it,
except a relock that moves no version (what `t fix lockfile` writes after a
rebase) and a declared single-crate update. Name the `cargo update` you ran (several
separated by `;`):

```bash
cargo update -p rustls --precise 0.23.31
git add Cargo.lock
TILO_CARGO_UPDATE='-p rustls --precise 0.23.31' git commit
```

`tools/ci/check_lock_update.py` then admits the lock only when
`cargo metadata --locked` accepts the working tree (which must equal the staged
lock and manifests), every package whose version moved is declared at its
`--precise` version, and every entry added or removed is a dependency of a
declared package's new or old version. Any other lock change stays refused.

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
cargo test --locked -p pdf-viewer --test it capture::
```

The installer verifies the archive before writing, retains its license files,
and requires the shared library to load on the native architecture. The capture
test additionally observes the production worker's raster backend before testing
unavailable imported content and its later arrival. Missing PDFium fails rather
than skipping coverage or qualifying the text fallback as a raster result.
