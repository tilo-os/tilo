# Android

A Tilo app can run as a standalone Android app: the same `main.rs`, the same
toolkit and the same widgets, drawn with wgpu on Vulkan in a `NativeActivity`.
Calculator and Notes opt in today. This is the app host only; the
compositor, `fs`, sandboxing and collaboration are Linux services and do not
exist on Android (see [Limits](#limits)).

## Build, install and run

`crates/sdk/ui-core/android/build-apk.sh` builds a debug APK without Gradle:

```bash
crates/sdk/ui-core/android/build-apk.sh calculator --abi x86_64   # emulator
crates/sdk/ui-core/android/build-apk.sh notes                     # arm64-v8a and x86_64
adb -s emulator-5554 install -r target/android/calculator/debug/calculator.apk
adb -s emulator-5554 shell am start -n so.tilo.calculator/so.tilo.android.TiloActivity
```

The output is `target/android/<app>/<debug|release>/<app>.apk`, signed with a
debug key the script generates once under `target/android/`; set
`TILO_ANDROID_KEYSTORE`, `TILO_ANDROID_KEYSTORE_PASS` and
`TILO_ANDROID_KEY_ALIAS` to sign with a real one. `--release` builds the
release profile, `--label` and `--application-id` override the launcher name
and the default `so.tilo.<app>` package. Run `build-apk.sh --help` for the
full usage.

The Rust library is built with `$CARGO`, by default `rcargo` and otherwise
`cargo`; the Java classes are compiled with `javac` and packed with the SDK's
build tools. [DEVELOPMENT.md](../DEVELOPMENT.md#android-app-host) lists what
each needs. The APK requires API 29 and Vulkan 1.1
(`AndroidManifest.xml.in`).

Everything the toolkit writes to stdout and stderr, including `tracing` and
panics, goes to logcat under the tag `tilo`:

```bash
adb -s emulator-5554 logcat -s tilo
```

## Opting an app in

An app needs three things, as `apps/calculator` shows:

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

The example name is the contract: `build-apk.sh` builds `<app>-android`, and
the CI lane below selects every affected app that declares one.

## Architecture

**Entry.** `TiloActivity` extends `NativeActivity`, which loads the library
named in the manifest's `android.app.lib_name` and starts `android_main` on
its own thread. `android_main!` makes that call `ui::launch::android::run_main`,
which prepares the process the way a desktop launcher would and then calls
the app's ordinary `main`, so the app takes the same `ui::launch::run` path
and winit event loop as on Linux. Preparing the process means:

- stdout and stderr are piped into logcat;
- `HOME`, `TMPDIR` and the XDG base directories point into the app's private
  storage (`launch/android_env.rs`), so `xdg-paths` and every store built on
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

**Fonts.** `build-apk.sh` packs the font catalog into `assets/tilo/fonts`.
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
- **Two apps.** Only Calculator and Notes declare an Android example.
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
crates/sdk/ui-core/android/build-apk.sh notes --abi x86_64
adb -s emulator-5554 install -r target/android/notes/debug/notes.apk
adb -s emulator-5554 shell am start -n so.tilo.notes/so.tilo.android.TiloActivity
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

Nothing in CI runs on a device. The `android` job in `check.yml` runs
`cargo clippy -D warnings` on every affected app's `<app>-android` example for
`aarch64-linux-android` and `x86_64-linux-android`; it is a type check, not an
APK build ([CI performance](ci-performance.md)).
