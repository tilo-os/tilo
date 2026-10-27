# Android

For running Android APKs on Tilo, see [Android compatibility qualification](android-compatibility.md).

A Tilo app can run as a standalone Android app: the same code, the same
toolkit and the same widgets, drawn with wgpu on Vulkan in a `NativeActivity`.
Every app `tilo-app new` scaffolds builds for Android with no Android code of
its own. This is the app host only; the compositor, `fs`, sandboxing and
collaboration are Linux services and do not exist on Android (see
[Limits](#limits)).

## Commands

Android is one more `--platform` of the ordinary commands:

```bash
tilo-app doctor --platform android           # what the toolchain lacks, and each fix
tilo-app setup android                       # install the SDK side of it
tilo-app run --platform android              # build, install, start, stream the log
tilo-app dev --platform android              # the same, again on every save
tilo-app build --platform android --arch x86_64
tilo-app package --platform android          # per architecture: signed .apk + .aab + a registry .tapp
tilo-app check --platform android            # Clippy for aarch64-linux-android
```

`--arch` is `aarch64` (arm64-v8a) or `x86_64`. `build` and `check` default to
`aarch64`; `run` and `dev` build for the device's own ABI; `package` builds
both unless `--arch` picks one.

**Devices.** `run` and `dev` install on `--device SERIAL`, else
`$ANDROID_SERIAL`, else the one device `adb devices` lists; with none or
several they say what to connect or pick. The APK is installed with
`adb install -r -d`, started with `am start`, and the app's logcat (tag
`tilo`, plus crashes and native backtraces) streams until the app exits or
Ctrl-C stops it. `run` does not forward arguments: an activity has no
command line.

**`dev` reloads by reinstalling.** A saved change rebuilds the library,
repacks and reinstalls the APK and restarts the activity, so the app starts
fresh each time. There is no in-place code swap on Android as there is on the
desktop; a rebuild takes as long as an incremental `cargo build` of the app.

**Output.** `build` writes
`target/android/<app>/<profile>/<app>-<abi>.apk`, debuggable and signed with
the per-user debug key. `package` writes, for each architecture,
`dist/<app>-android-<arch>.tapp` and, beside it, `.apk` and `.aab` with the
same name: a release build, signed as below. It checks every output before it
builds either, and `--output` names one package, so it needs `--arch`. There
is one APK per architecture rather than one universal APK because the registry
lists one build per architecture and a device downloads only its own. The
`.tapp` is what `tilo-app publish` uploads; the registry lists it under the
`android` platform, and Tilo OS clients never install it.

**Size.** An Android package answers to the registry's limit for a build that
links the toolkit in, 64 MiB (`tilo_app_package::platform_size_limit`, the
function the registry enforces), not to `[budget]`, which bounds the thin Tilo
OS build. A release starter app is about 39 MiB, most of it the library and
the font catalog.

## The toolchain

The build uses no Gradle. It needs:

- the Android SDK at `$ANDROID_HOME` (or `$ANDROID_SDK_ROOT`, default
  `~/Android/Sdk`) with `build-tools;36.1.0`, `platforms;android-36` and
  `platform-tools` (for `adb`);
- NDK r29, from `$ANDROID_NDK_HOME` or the SDK's `ndk/`, and the Rust targets
  `aarch64-linux-android` and `x86_64-linux-android`. The CLI points Cargo at
  the NDK's clang unless `CARGO_TARGET_*_LINUX_ANDROID_LINKER` is already set,
  as it is in the builder image;
- a JDK 17 or newer (`javac`, `keytool`, `jarsigner`) and `zip`/`unzip`;
- optionally bundletool, for the `.aab`. Without it `package` makes the APK
  only and says so; `--no-bundle` skips the bundle on purpose.

`tilo-app doctor --platform android` checks each and prints the fix. For the
JDK and the zip tools that is the one command for this system's family, read
from `/etc/os-release` (every family's when it is none of them):

| Family | Command |
| --- | --- |
| Fedora, RHEL (dnf) | `sudo dnf install -y java-21-openjdk-devel zip unzip` |
| Debian, Ubuntu (apt) | `sudo apt-get install -y openjdk-17-jdk-headless zip unzip` |
| macOS (brew) | `brew install openjdk@21 zip unzip` |

`tilo-app setup android` fetches the command-line tools when `sdkmanager` is
missing, installs the SDK packages (and the NDK when none is found), downloads
bundletool into the SDK and adds the Rust targets; `--accept-licenses` accepts
the SDK licences without prompting, for CI. It cannot install a JDK or the zip
tools, so it prints the command above for whichever are missing, first and
again at the end, and stops early without `unzip` when it must unpack the
command-line tools.

`crates/sdk/ui-core/android/build-apk.sh <app> [--abi ABI]... [--release]`
remains as a thin wrapper over the same commands for apps in this checkout.

## From `tilo-app.toml` to the APK

| APK | Source |
| --- | --- |
| Application id | `[package] id`, with `-` as `_` (`so.tilo.pocket-notes` → `so.tilo.pocket_notes`) |
| Launcher label | `[package] name`, else the id's last part, title-cased |
| `versionName` | `[package] version` |
| `versionCode` | `MAJOR × 1 000 000 + MINOR × 1 000 + PATCH` from the version; each part below 1000 |
| Icon | `icon.png` in the app directory, else a generated coloured disc |
| Permissions | `POST_NOTIFICATIONS` always (the notification service asks at runtime); `INTERNET` and `ACCESS_NETWORK_STATE` unless `[net] mode = "off"`; the `location`, `camera` and `microphone` portals map to `ACCESS_COARSE_LOCATION` + `ACCESS_FINE_LOCATION`, `CAMERA` and `RECORD_AUDIO`, unless their policy is `deny` |

The APK requires API 29 and Vulkan 1.1, targets API 36, and packs the
toolkit's font catalog into `assets/tilo/fonts`.

## Signing

- **Debug** (`build`, `run`, `dev`): `~/.android/debug.keystore`, the key
  Android Studio uses, created with `keytool` if missing. Every debug build on
  the machine can update every other.
- **Release** (`package`): `--keystore PATH`, else `$TILO_ANDROID_KEYSTORE`,
  with `$TILO_ANDROID_KEYSTORE_PASS`, `$TILO_ANDROID_KEY_ALIAS` (default
  `tilo-release`) and `$TILO_ANDROID_KEY_PASS` (default the store's). Without
  either, the release key is the one kept beside the publisher key,
  `$XDG_CONFIG_HOME/tilo/android-release.keystore`, created on first use with
  a random password in `android-release.keystore.pass` (mode 0600). Back it up
  with the publisher key: Android refuses an update signed with another key.

The APK is signed with `apksigner` (v2 and v3 schemes) after `zipalign`
(16 KiB page-aligned libraries), the bundle with `jarsigner`.

## CI

`tilo-app add ci` adds an `android` `aarch64` build to the package matrix. It
runs in the builder image on the forge's ordinary Linux x86_64 runner (for
GitHub, `ubuntu-latest`): the image already carries the NDK and its linker
variables, and the job installs a JDK and `zip` with `dnf`, then the SDK with
`tilo-app setup android --accept-licenses`, cached between runs. The SDK and
JDK are not in the image because Tilo's own CI never packages APKs and they
would add about a gigabyte to every job that pulls it. Set the
`TILO_ANDROID_KEYSTORE` secret (the keystore, base64) and
`TILO_ANDROID_KEYSTORE_PASS` to sign releases with your own key; the `.apk`
and `.aab` are uploaded as an artifact, and a tag publishes the `.tapp` with
the others.

## How an app enters

A scaffolded app's `src/lib.rs` ends in `ui::dev_reload_entry!(App);`. Off
Android that exports the desktop's hot-reload entry; on Android it also
exports `android_main`, which runs `ui::framework::run::<App>()` through
`ui::launch::android::run_main`. `build` compiles the library as a `cdylib`
(`cargo rustc --lib --crate-type cdylib`), so the app's `Cargo.toml` needs
nothing Android-specific.

An app without that library, such as Calculator and Notes, opts in the older
way instead:

1. `ui::android_main!();` in its `main.rs`. It expands to nothing off Android.
2. A `src/android.rs` that includes that `main.rs` as a module:

   ```rust
   #[path = "main.rs"]
   mod app;
   ```

3. A `cdylib` example named `<app>-android` behind an `android` feature in its
   `Cargo.toml`:

   ```toml
   [[example]]
   name = "calculator-android"
   path = "src/android.rs"
   crate-type = ["cdylib"]
   required-features = ["android"]

   [features]
   android = []
   ```

When an app declares that example the CLI builds it with `--features
android`; otherwise it builds the library. The CI lane below selects every
affected app that declares the example.

Everything the toolkit writes to stdout and stderr, including `tracing` and
panics, goes to logcat under the tag `tilo`, which `run` streams.

## Architecture

**Entry.** `TiloActivity` extends `NativeActivity`, which loads the library
named in the manifest's `android.app.lib_name` and starts `android_main` on
its own thread. `android_main!` makes that call `ui::launch::android::run_main`,
which prepares the process the way a desktop launcher would and then calls
the app's ordinary `main`, so the app takes the same `ui::launch::run` path
and winit event loop as on Linux. Preparing the process means:

- stdout and stderr are piped into logcat;
- `HOME`, `TMPDIR` and the XDG base directories point into the app's private
  storage (`launch/android_env.rs`), so `tilo-paths` and every store built on
  it resolve there unchanged;
- `LANG` follows the device locale;
- `TILO_RUNTIME_STANDALONE=1`, since there is no runtime host to re-exec under.

The activity handles every configuration change itself (rotation, resizing,
keyboards, density, locale, theme) rather than being recreated, and ends the
process when it is destroyed: winit's event loop and the toolkit's process
state run once per process.

**The Java layer.** `crates/sdk/ui-core/android/java/so/tilo/android` holds
the only Java: `TiloActivity` and one helper class per thing a native
activity cannot do from Rust: `TiloTextInput` (the soft keyboard's
`InputConnection`), `TiloDocuments` and `TiloActivityResults` (Storage Access
Framework choosers), `TiloFileProvider` (sharing a file with another app),
`TiloNotifications`, `TiloSpeech` and `TiloLocation`. `TiloActivity` also
reports the window's system-bar, cutout and keyboard insets, which the runner
keeps content out of.

**JNI in both directions.** Rust calls Java through the `JavaVM` and activity
that android-activity publishes in `ndk_context` (`tilo-host`'s
`native/android/mod.rs`, `with_activity`). Java calls Rust in two ways:

- `TiloActivity` calls `System.loadLibrary` before `NativeActivity` starts, so
  its own `static native` methods resolve to exported
  `Java_so_tilo_android_TiloActivity_<method>` symbols
  (`nativeSafeAreaChanged` in `launch/android.rs`).
- The helper classes are loaded through the activity's class loader, since
  `FindClass` on a thread Rust attached sees only system classes, and their
  natives are bound with `RegisterNatives` when first loaded
  (`tilo-host/src/native/android/bridge.rs`,
  `ui-core/src/app/android_text_input/bridge.rs`).

A native method runs on the UI thread and can run before `android_main` has
started, so the Rust side records what it is given and wakes the event loop
rather than touching the toolkit. A chooser's or the location service's
answer is sent through `tilo_host::wake::reply()`, which wakes the loop once
the answer is queued.

**Input.** Touches reach the toolkit as winit `Touch` events, and
`TouchPointerEmulator` also synthesises the pointer moves, presses and
releases that mouse-only widgets understand. Soft keyboards talk to
`TiloTextInput`'s `InputConnection`; its commits, compositions and deletions
become the same `WindowEvent::Ime` and key events as on every other platform
(`app/android_text_input.rs`), so text fields need nothing Android-specific.
The keyboard is given the focused field's text around the caret (up to 2048
bytes each side, nothing for a password field) for suggestions and
auto-capitalisation.

**Accessibility.** The AccessKit tree reaches TalkBack through
`accesskit_android`'s injecting adapter on the window's decor view
(`accesskit_android_adapter.rs`). The vendored winit leaves hover events
unhandled so explore-by-touch reaches that view.

**Fonts.** `tilo-app build --platform android` packs the font catalog into `assets/tilo/fonts`.
The first start of each catalog version copies it into the app's files
directory, since fontdb cannot map a file inside an APK; the device's
`/system/fonts` cover the scripts the catalog does not.

## Host services

`tilo-host`'s `native/android` implements `HostServices`:

| Service | Android implementation |
| --- | --- |
| Clipboard | `ClipboardManager`. There is no primary selection; select-to-copy stays in the process. Shared text is marked sensitive. |
| Files | Storage Access Framework Open and Save choosers, answering with the opened descriptor; a path-based Open reads a private read-only copy. A path-based Save and the folder chooser fail, since a `content:` document has no path. |
| Opener | An `ACTION_VIEW` intent. `file://` URIs are refused: Android forbids handing them to another app. |
| Speech | The system `TextToSpeech` engine |
| Notifications | `NotificationManager`, asking for `POST_NOTIFICATIONS` at runtime |
| Location | `LocationManager`, asking for the location permission at runtime |
| Media sessions, screen capture | Not implemented; they return `HostServices::null`'s refusals |

## Limits

- **No collaboration.** Nothing gives an Android app a collaboration route,
  so apps show "Collaboration is unavailable; remote changes are not
  synchronized." and edits stay on the device.
- **No Tilo sandbox.** The app is not launched under Tilo's runtime host
  (`TILO_RUNTIME_STANDALONE=1`), so its Landlock and bubblewrap confinement
  does not apply; the app runs in Android's own per-app sandbox.
- **No in-place reload.** `dev` reinstalls and restarts the app on each change.
- **TalkBack explore-by-touch is untested on a device.** The emulator image has
  no TalkBack and `adb` cannot inject hover events.
- **Hardware Enter** does not reach text fields as Enter (seen with
  `adb shell input keyevent 66` on the emulator).
- **The note body gives the keyboard no surrounding text**: the block
  editor's accessibility node carries no value or selection.
- **Size.** A debug x86_64 Calculator APK is 59 MB: about 33 MB is the
  compressed library and about 26 MB the compressed font catalog.
- **No pipeline cache.** A resume reuses the device and renderer, but a cold
  start compiles vello's pipelines afresh: there is no persisted
  `wgpu::PipelineCache`.

## Testing on an emulator

The toolkit's behaviour is tested headlessly like any other app's; the
emulator is for what only Android shows: the activity lifecycle, touch, the
input method, choosers and insets. With an `x86_64` AVD running:

```bash
tilo-app run --platform android --device emulator-5554 apps/notes
adb -s emulator-5554 shell input tap X Y            # physical pixels
adb -s emulator-5554 exec-out screencap -p > shot.png
```

Useful probes:

- `adb shell dumpsys input_method` shows the `EditorInfo` the keyboard was
  given (`initialSelStart`/`initialSelEnd`), which is how the surrounding text
  handed to it was checked.
- `adb shell settings put system accelerometer_rotation 0` and
  `user_rotation 1` force landscape; restore both afterwards.
- An AVD configured with a hardware keyboard never shows the soft keyboard,
  but `adb shell input text` still types into the focused field.

Nothing in CI runs on a device. t's `android` check runs
`cargo clippy -D warnings` on every app's `<app>-android` example for
`aarch64-linux-android` and `x86_64-linux-android` (tools/ci/clippy-android.sh);
it is a type check, not an APK build.
