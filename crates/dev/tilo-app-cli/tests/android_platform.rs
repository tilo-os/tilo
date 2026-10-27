#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
//! `tilo-app build|package --platform android` builds the app's library as a
//! `cdylib` for the Android triple with the NDK's linker, and assembles,
//! signs and (for `package`) bundles and packs it with the SDK's tools. Every
//! tool here is a stand-in that records what it was asked, so the test proves
//! the orchestration; a real APK is proved against a real SDK
//! (docs/android.md).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Stands in for Cargo: `metadata` names a fake SDK checkout; a build
/// records its arguments and linker, and reports one shared library.
const FAKE_CARGO: &str = r#"#!/bin/sh
if [ "$1" = metadata ]; then
  printf '{"target_directory":"%s","packages":[{"name":"tilo-app","manifest_path":"%s/crates/sdk/tilo-app/Cargo.toml"}]}\n' "$PWD/target" "$FAKE_CHECKOUT"
  exit 0
fi
printf '%s\n' "$@" > "$FAKE_LOG/cargo-args"
printf '%s\n' "$*" >> "$FAKE_LOG/cargo-builds"
env | grep -E '^(CARGO_TARGET|CC|AR)_.*ANDROID' | sort > "$FAKE_LOG/cargo-env"
while [ $# -gt 0 ]; do
  case "$1" in
    --manifest-path) manifest="$(cd "$(dirname "$2")" && pwd -P)/Cargo.toml"; shift ;;
    --target) target="$2"; shift ;;
  esac
  shift
done
crate=$(sed -n 's/^name = "\(.*\)"/\1/p' "$manifest" | head -n1 | tr - _)
out="$(dirname "$manifest")/target/$target/debug/lib$crate.so"
mkdir -p "$(dirname "$out")"
printf 'fake library' > "$out"
printf '{"reason":"compiler-artifact","manifest_path":"%s","target":{"name":"%s","kind":["lib"]},"filenames":["%s"]}\n' "$manifest" "$crate" "$out"
"#;

/// The SDK, NDK and JDK tools, each as a small script. An "archive" is a
/// text file of `entry:` lines, which is all the test reads back.
const TOOLS: &[(&str, &str)] = &[
    (
        "sdk/build-tools/36.1.0/aapt2",
        r#"#!/bin/sh
echo "aapt2 $*" >> "$FAKE_LOG/tools"
if [ "$1" = compile ]; then
  mkdir -p "$3"; : > "$3/mipmap-xxxhdpi_ic_launcher.png.flat"; exit 0
fi
while [ $# -gt 0 ]; do
  case "$1" in -o) out="$2"; shift ;; --manifest) manifest="$2"; shift ;; esac
  shift
done
{ echo "entry:AndroidManifest.xml"; cat "$manifest"; } > "$out"
"#,
    ),
    (
        "sdk/build-tools/36.1.0/d8",
        r#"#!/bin/sh
echo "d8 $*" >> "$FAKE_LOG/tools"
while [ $# -gt 0 ]; do [ "$1" = --output ] && out="$2"; shift; done
: > "$out/classes.dex"
"#,
    ),
    (
        "sdk/build-tools/36.1.0/zipalign",
        "#!/bin/sh\necho \"zipalign $*\" >> \"$FAKE_LOG/tools\"\nshift 4\ncp \"$1\" \"$2\"\n",
    ),
    (
        "sdk/build-tools/36.1.0/apksigner",
        r#"#!/bin/sh
echo "apksigner $*" >> "$FAKE_LOG/tools"
while [ $# -gt 0 ]; do
  case "$1" in --out) out="$2"; shift ;; --ks) ks="$2"; shift ;; *) in="$1" ;; esac
  shift
done
{ cat "$in"; echo "signed-by:$ks"; } > "$out"
"#,
    ),
    ("sdk/platforms/android-36/android.jar", ""),
    (
        "sdk/platform-tools/adb",
        "#!/bin/sh\necho 'List of devices attached'\necho\n",
    ),
    (
        "ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip",
        "#!/bin/sh\ncp \"$4\" \"$3\"\n",
    ),
    (
        "jdk/bin/javac",
        r#"#!/bin/sh
echo "javac $*" >> "$FAKE_LOG/tools"
while [ $# -gt 0 ]; do [ "$1" = -d ] && out="$2"; shift; done
mkdir -p "$out/so/tilo/android"; : > "$out/so/tilo/android/TiloActivity.class"
"#,
    ),
    (
        "jdk/bin/keytool",
        r#"#!/bin/sh
echo "keytool $*" >> "$FAKE_LOG/tools"
while [ $# -gt 0 ]; do [ "$1" = -keystore ] && ks="$2"; shift; done
printf 'keystore' > "$ks"
"#,
    ),
    (
        "jdk/bin/java",
        r#"#!/bin/sh
echo "java $*" >> "$FAKE_LOG/tools"
for arg; do case "$arg" in --output=*) out="${arg#--output=}" ;; esac; done
printf 'bundle' > "$out"
"#,
    ),
    (
        "jdk/bin/jarsigner",
        r#"#!/bin/sh
echo "jarsigner $*" >> "$FAKE_LOG/tools"
while [ $# -gt 0 ]; do
  case "$1" in -signedjar) out="$2"; in="$3"; shift 2 ;; esac
  shift
done
cp "$in" "$out"
"#,
    ),
    (
        "bin/zip",
        r#"#!/bin/sh
archive="$4"
while read -r name; do echo "entry:$name" >> "$archive"; done
"#,
    ),
    (
        "bin/unzip",
        "#!/bin/sh\nmkdir -p \"$4\"\n: > \"$4/AndroidManifest.xml\"\n",
    ),
    ("bundletool.jar", ""),
    (
        "checkout/crates/runtime/tilo-runtime/Cargo.toml",
        "[package]\nname = \"tilo-runtime\"\n",
    ),
    (
        "checkout/crates/sdk/ui-core/android/java/so/tilo/android/TiloActivity.java",
        "class TiloActivity {}\n",
    ),
    ("checkout/crates/sdk/ui-core/assets/fonts/Inter.ttf", "font"),
    (
        "checkout/crates/sdk/ui-core/assets/fonts/manifest.toml",
        "[[font]]\nscope = \"product\"\nfile = \"Inter.ttf\"\n",
    ),
];

struct Fixture {
    dir: tempfile::TempDir,
    app: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("log")).unwrap();
        write_executable(&root.join("fake-cargo"), FAKE_CARGO);
        for (path, contents) in TOOLS {
            write_executable(&root.join(path), contents);
        }
        let fixture = Self {
            app: root.join("notes"),
            dir,
        };
        let created = fixture.tilo_app(fixture.root(), &["new", "notes", "--template", "starter"]);
        assert!(created.status.success(), "{}", stderr(&created));
        fixture
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn tilo_app(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        let root = self.root();
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(tilo_test_env::package_binary!("tilo-app"));
        command
            .args(args)
            .current_dir(cwd)
            .env("CARGO", root.join("fake-cargo"))
            .env("FAKE_LOG", root.join("log"))
            .env("FAKE_CHECKOUT", root.join("checkout"))
            .env("PATH", path)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("ANDROID_HOME", root.join("sdk"))
            .env("ANDROID_NDK_HOME", root.join("ndk"))
            .env("JAVA_HOME", root.join("jdk"))
            .env("TILO_BUNDLETOOL_JAR", root.join("bundletool.jar"))
            .env_remove("TILO_UI_PATH")
            .env_remove("TILO_PUBLISHER_KEY")
            .env_remove("ANDROID_SDK_ROOT")
            .env_remove("ANDROID_NDK_ROOT")
            .env_remove("ANDROID_NDK_LATEST_HOME")
            .env_remove("ANDROID_SERIAL")
            .env_remove("TILO_ANDROID_KEYSTORE")
            .env_remove("TILO_ANDROID_KEYSTORE_PASS");
        for (key, _) in std::env::vars() {
            if key.contains("ANDROID")
                && (key.starts_with("CARGO_TARGET_")
                    || key.starts_with("CC_")
                    || key.starts_with("CXX_")
                    || key.starts_with("AR_"))
            {
                command.env_remove(key);
            }
        }
        command.output().expect("run tilo-app")
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.root().join("log").join(name)).unwrap_or_default()
    }
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("JSON on stdout")
}

#[test]
fn a_scaffolded_app_builds_a_debug_apk_from_its_library() {
    let fixture = Fixture::new();
    let output = fixture.tilo_app(
        &fixture.app,
        &[
            "build",
            "--platform",
            "android",
            "--arch",
            "x86_64",
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let args = fixture.log("cargo-args");
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(
        &args[..4],
        ["rustc", "--lib", "--crate-type", "cdylib"],
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--target", "x86_64-linux-android"])
    );
    assert!(args.windows(2).any(|pair| pair == ["--profile", "dev"]));
    let linker = fixture
        .root()
        .join("ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android29-clang");
    assert!(
        fixture.log("cargo-env").contains(&format!(
            "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER={}",
            linker.display()
        )),
        "{}",
        fixture.log("cargo-env")
    );

    let report = json(&output);
    let result = &report["result"];
    let apk = PathBuf::from(result["apk"].as_str().expect("apk path"));
    assert_eq!(
        apk,
        fixture
            .app
            .join("target/android/notes/dev/notes-x86_64.apk")
    );
    assert_eq!(result["abi"], "x86_64");
    assert_eq!(result["version_code"], 1000);
    let contents = std::fs::read_to_string(&apk).unwrap();
    for entry in [
        "entry:AndroidManifest.xml",
        "entry:classes.dex",
        "entry:lib/x86_64/libnotes.so",
    ] {
        assert!(contents.contains(entry), "{entry} missing:\n{contents}");
    }
    assert!(
        contents
            .contains(r#"<meta-data android:name="android.app.lib_name" android:value="notes" />"#)
    );
    assert!(contents.contains(r#"android:debuggable="true""#));
    assert!(contents.contains(r#"android:versionName="0.1.0""#));
    let debug_key = fixture.root().join("home/.android/debug.keystore");
    assert!(debug_key.is_file(), "the debug key is created on first use");
    assert!(contents.contains(&format!("signed-by:{}", debug_key.display())));
    assert!(
        !fixture.log("tools").contains("jarsigner"),
        "a build makes no bundle"
    );
}

#[test]
fn an_android_package_without_arch_is_one_package_per_architecture() {
    let fixture = Fixture::new();

    // One --output cannot name two packages, and nothing is built to learn it.
    let named = fixture.tilo_app(
        &fixture.app,
        &[
            "package",
            "--platform",
            "android",
            "--output",
            "dist/one.tapp",
        ],
    );
    assert!(!named.status.success());
    assert!(stderr(&named).contains("add --arch"), "{}", stderr(&named));
    assert_eq!(fixture.log("cargo-builds"), "");

    let output = fixture.tilo_app(
        &fixture.app,
        &["package", "--platform", "android", "--no-bundle", "--json"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let builds = fixture.log("cargo-builds");
    assert_eq!(builds.lines().count(), 2, "{builds}");
    assert!(
        builds.contains("--target aarch64-linux-android"),
        "{builds}"
    );
    assert!(builds.contains("--target x86_64-linux-android"), "{builds}");

    let dist = fixture.app.join("dist");
    for (arch, abi) in [("aarch64", "arm64-v8a"), ("x86_64", "x86_64")] {
        let apk = std::fs::read_to_string(dist.join(format!("notes-android-{arch}.apk"))).unwrap();
        assert!(
            apk.contains(&format!("entry:lib/{abi}/libnotes.so")),
            "{apk}"
        );
        let tapp = dist.join(format!("notes-android-{arch}.tapp"));
        let reader = std::io::BufReader::new(std::fs::File::open(&tapp).unwrap());
        let read =
            tilo_app_package::tapp::read(reader, &mut tilo_app_package::DiscardBlobs).unwrap();
        assert_eq!(
            read.meta.manifest.target.as_ref().unwrap().architecture,
            arch
        );
    }
    let packages = json(&output)["result"]["packages"].clone();
    let packages = packages.as_array().expect("one result per package");
    assert_eq!(packages.len(), 2);

    // Both builds are measured against the registry's Android limit.
    let limit = tilo_app_package::platform_size_limit("android");
    assert_eq!(limit, tilo_app_package::MAX_DESKTOP_APP_TAPP_BYTES);
    for package in packages {
        assert_eq!(package["size"]["limit_bytes"], limit, "{package}");
    }
    assert!(
        stderr(&output).contains("of the 64.0 MiB limit"),
        "{}",
        stderr(&output)
    );

    // Every output is checked before either architecture is rebuilt.
    for extension in ["tapp", "apk"] {
        std::fs::remove_file(dist.join(format!("notes-android-aarch64.{extension}"))).unwrap();
    }
    let kept = fixture.tilo_app(
        &fixture.app,
        &["package", "--platform", "android", "--no-bundle"],
    );
    assert!(!kept.status.success());
    assert!(
        stderr(&kept).contains("notes-android-x86_64"),
        "{}",
        stderr(&kept)
    );
    assert_eq!(fixture.log("cargo-builds").lines().count(), 2);
}

#[test]
fn an_android_package_is_release_signed_bundled_and_packed_for_the_registry() {
    let fixture = Fixture::new();
    let output = fixture.tilo_app(
        &fixture.app,
        &[
            "package",
            "--platform",
            "android",
            "--arch",
            "aarch64",
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let args = fixture.log("cargo-args");
    assert!(args.contains("aarch64-linux-android\n"), "{args}");
    assert!(args.contains("--profile\nrelease\n"), "{args}");

    let dist = fixture.app.join("dist");
    let apk = std::fs::read_to_string(dist.join("notes-android-aarch64.apk")).unwrap();
    assert!(apk.contains("entry:lib/arm64-v8a/libnotes.so"), "{apk}");
    assert!(apk.contains(r#"android:debuggable="false""#));
    let release_key = fixture.root().join("config/tilo/android-release.keystore");
    assert!(apk.contains(&format!("signed-by:{}", release_key.display())));
    let pass = fixture
        .root()
        .join("config/tilo/android-release.keystore.pass");
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&pass).unwrap().permissions());
    assert_eq!(mode & 0o077, 0, "the keystore password is private");
    assert!(dist.join("notes-android-aarch64.aab").is_file());
    assert!(fixture.log("tools").contains("jarsigner"));

    let tapp = dist.join("notes-android-aarch64.tapp");
    let reader = std::io::BufReader::new(std::fs::File::open(&tapp).unwrap());
    let read = tilo_app_package::tapp::read(reader, &mut tilo_app_package::DiscardBlobs).unwrap();
    let target = read
        .meta
        .manifest
        .target
        .as_ref()
        .expect("an android [target]");
    assert_eq!(target.platform, "android");
    assert_eq!(target.architecture, "aarch64");
    assert_eq!(target.module, "bin/notes.apk");
    assert_eq!(
        read.meta.collection.entries.keys().collect::<Vec<_>>(),
        ["bin/notes.apk"]
    );

    // An existing package is refused before anything is compiled.
    let builds = fixture.log("cargo-args");
    let kept = fixture.tilo_app(
        &fixture.app,
        &["package", "--platform", "android", "--arch", "aarch64"],
    );
    assert!(!kept.status.success());
    assert!(
        stderr(&kept).contains("already exists; pass --force to replace it"),
        "{}",
        stderr(&kept)
    );
    assert_eq!(
        fixture.log("cargo-args"),
        builds,
        "no build before the refusal"
    );

    // The same key signs the next release.
    let again = fixture.tilo_app(
        &fixture.app,
        &[
            "package",
            "--platform",
            "android",
            "--arch",
            "aarch64",
            "--no-bundle",
            "--force",
        ],
    );
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(fixture.log("tools").matches("keytool").count(), 1);

    let named = fixture.tilo_app(
        &fixture.app,
        &[
            "package",
            "--platform",
            "android",
            "--force",
            "--keystore",
            "missing.keystore",
        ],
    );
    assert!(!named.status.success());
    assert!(
        stderr(&named).contains("missing.keystore does not exist"),
        "{}",
        stderr(&named)
    );
}

#[test]
fn run_names_what_to_connect_when_no_device_is_ready() {
    let fixture = Fixture::new();
    let output = fixture.tilo_app(&fixture.app, &["run", "--platform", "android"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no Android device is ready"),
        "{}",
        stderr(&output)
    );
    assert!(
        fixture.log("cargo-args").is_empty(),
        "nothing is built without a device"
    );
}
