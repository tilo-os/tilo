# macOS development

Use [shared setup](../setup.md) for the pinned Rust toolchain and Python 3.11 or
newer. Install Apple's Xcode Command Line Tools for the native compiler, linker
and SDK. Work on selected packages, not a workspace-wide OS build.

## Apps and media prerequisites

Portable SDK apps run as native macOS windows. Linux compositor libraries such
as libseat and PipeWire are not general Mac app prerequisites. For apps using
GStreamer (including Video Editor's default playback and export features),
Homebrew supplies the native media stack. The macOS `shell` also links this
stack:

```sh
brew install pkgconf gstreamer
pkg-config --modversion glib-2.0 gstreamer-1.0 gstreamer-app-1.0
```

GStreamer-enabled media requires version 1.24 or newer. Discoverable development
libraries do not prove codec availability, audible playback or hardware decode.
Run the relevant app's integration checks on the Mac for those claims.

For example, from the repository root after installing the app's dependencies:

```sh
cargo run --locked -p video-editor
```

This is a source-run app, not a signed, relocatable distribution. Font
preparation and offline use are covered by the
[font artifact guide](../../../crates/sdk/ui-core/assets/fonts/ARTIFACTS.md).
For a new independent app, use the
[app developer guide](../../developers/getting-started.md) and
`tilo-app guide start`.

## Desktop as an app, not a login replacement

On macOS, `shell` runs Tilo's desktop chrome and hosted workspace content inside
an ordinary native application window. It does **not** replace the macOS login
desktop, own a Linux DRM seat, or serve as a Wayland compositor. Linux's
`--backend winit`, `--backend drm` and session-package instructions do not
apply.

The [macOS runner](../../../crates/session/shell/src/compositor/macos.rs) owns
its argument parser. From the repository root:

```sh
cargo run --locked -p shell --bin shell -- --help
cargo run --locked -p shell --bin shell
```

Its options are `--state-dir PATH` for workspace/record storage,
`--files-dir PATH` for the initial Files directory, and `--shots FILE.png` for
one offscreen image followed by exit. The defaults use the local data
directory's `tilo/macos-shell` profile and your home directory respectively. Use
a separate profile and fixture files for experiments; a live session holds its
profile lock, and malformed saved state is refused rather than overwritten. An
offscreen render is not evidence of native input, accessibility or display
presentation correctness.

The developer shell is also distinct from the distributed **Tilo.app** runtime
and Store bundle. For runtime packaging and signing, follow the
[macOS distribution guide](../../../dist/macos/README.md); building an app
locally does not qualify its signing, notarization or installation.

## Linux and OS changes from a Mac

A Mac check sees only the code enabled for that target. For changes involving
Linux services, use the affected Linux checks through [t](../../t.md), or a Tilo
development VM. Do not treat a passing Mac check as Linux coverage.

`tilo device guide quickstart` and `tilo device guide platforms` own VM setup
(also available as `tilo-vm guide ...`). The
[platform guide](../../../crates/dev/tilo-vm/guide/platforms.md) describes the
macOS ARM64 UTM adapter and host tools. Builds for the Linux guest run in the
guest; no macOS native build supplies the Linux compositor. VM rendering does
not qualify Mac hardware rendering or a physical Tilo installation.

For Android app-host work, use [Android setup](android.md).
