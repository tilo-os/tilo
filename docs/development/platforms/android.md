# Android app-host development

This target runs a Tilo app as an Android APK using the shared SDK and Vulkan.
It is **not** a Tilo OS image, Linux desktop, filesystem projection or sandbox
port. Running Android apps _on Tilo_ is a different task: see
[Android compatibility qualification](../../android-compatibility.md).

For app creation and CLI installation, start with the
[app developer guide](../../developers/getting-started.md). The maintained
[Android guide](../../android.md) owns building, device selection, packaging,
signing and platform limits; this page covers host prerequisites only.

## Check before installing

```sh
tilo-app doctor --platform android
```

This checks the Android toolchain without requiring an app directory. It reports
missing system tools separately from SDK components. It is a prerequisite probe,
not proof that an APK builds, boots or works on a device.

The current
[setup/doctor implementation](../../../crates/dev/tilo-app-cli/src/android.rs)
uses these inputs:

| Input         | Requirement                                                                                |
| ------------- | ------------------------------------------------------------------------------------------ |
| Android SDK   | Setup installs build-tools **36.1.0**, platform **android-36**, and platform-tools (`adb`) |
| NDK           | Setup installs **r29 / 29.0.14206865** when no NDK toolchain directory is discovered       |
| Rust targets  | `aarch64-linux-android` and `x86_64-linux-android`                                         |
| JDK           | **17 or newer**, including `java`, `javac`, `keytool` and `jarsigner`                      |
| Archive tools | Info-ZIP-compatible `zip` and `unzip` on `PATH`                                            |
| Downloader    | `curl` on `PATH` for setup downloads                                                       |
| Bundletool    | Setup downloads **1.18.1**; optional for APK-only work, needed for `.aab` output           |

No Gradle installation is needed. Install the JDK and archive tools through your
host's package manager; `doctor` supplies Fedora/RHEL, Debian/Ubuntu or Homebrew
hints. Setup does **not** install those system packages. Put the JDK's `bin` on
`PATH` or set `JAVA_HOME` to the JDK, not a JRE. Windows users must supply
Windows-native tools; a Linux JDK or NDK inside WSL is not a native Windows
toolchain. Keep a WSL build in its [Linux checkout](linux.md#wsl-checkouts).

## SDK installation and discovery

Once the system tools are present:

```sh
tilo-app setup android
tilo-app doctor --platform android
```

Setup may download the SDK command-line tools, installs the SDK packages,
downloads bundletool into the SDK, and invokes `rustup target add` for both
Android architectures. It prompts for SDK licenses;
`tilo-app setup android --accept-licenses` opts into noninteractive acceptance,
for example in CI. Setup is an install operation, not a dry run.

Discovery considers `ANDROID_HOME`, then `ANDROID_SDK_ROOT`, then the host's
Android Studio default:

| Host    | Default SDK directory        |
| ------- | ---------------------------- |
| Linux   | `~/Android/Sdk`              |
| macOS   | `~/Library/Android/sdk`      |
| Windows | `%LOCALAPPDATA%\Android\Sdk` |

The NDK is discovered through `ANDROID_NDK_HOME`, `ANDROID_NDK_ROOT` or
`ANDROID_NDK_LATEST_HOME`, then the SDK's `ndk/` (preferring the pinned version)
or `ndk-bundle`. The CLI supplies Cargo's linker, C/C++ compiler and archiver
variables from that NDK; existing explicit environment settings are preserved.
Do not copy the builder image's `/opt/android-ndk` paths into host setup. An
existing compatible build-tools installation can be discovered instead of the
pinned version; these setup pins are not an assertion that doctor enforces exact
versions or validates every executable.

## Next: one app, one device

From an app directory, use `tilo-app build --platform android --arch x86_64` for
an x86_64 APK, or `--arch aarch64` for ARM64. Building does not require a
connected device. For execution, `tilo-app run --platform android` needs an
ADB-connected emulator or device; `--device SERIAL` disambiguates multiple
devices. Setup does not create or launch an emulator or install its system
image.

The app host requires Android API 29 or newer and Vulkan 1.1. A successful
cross-build or stub-tool orchestration test does not qualify emulator graphics,
touch input, permissions, media or real hardware. Follow the
[Android guide](../../android.md) for the complete run/dev and signing flows,
and report which device and behavior you actually tested.
