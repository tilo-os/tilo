//! Android: the app's library as a `cdylib` inside an APK whose
//! `NativeActivity` enters it (docs/android.md), built, signed, installed and
//! run without Gradle.
//!
//! The Java layer, and the font catalog the renderer loads, come from the SDK
//! checkout the app builds against (`crates/sdk/ui-core/android`); the
//! manifest is generated here from `tilo-app.toml`. The Rust library is the
//! app's `<package>-android` example when it declares one, else its library
//! built as a `cdylib`, whose `ui::dev_reload_entry!` defines `android_main`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};

use crate::{PackageArch, PackageOutcome, PackagePlatform, PackageRequest};

/// The build-tools `aapt2`, `d8`, `zipalign` and `apksigner` come from.
pub const BUILD_TOOLS_VERSION: &str = "36.1.0";
/// The platform whose `android.jar` the Java layer compiles against.
pub const PLATFORM: &str = "android-36";
/// The NDK `tilo-app setup android` installs: r29, as the builder image's.
pub const NDK_VERSION: &str = "29.0.14206865";
/// API 29 is the first level that requires Vulkan 1.1 on 64-bit devices.
pub const MIN_SDK: u32 = 29;
pub const TARGET_SDK: u32 = 36;
/// bundletool turns a module into an Android App Bundle.
pub const BUNDLETOOL_VERSION: &str = "1.18.1";
const JAVA_RELEASE: &str = "11";
const ACTIVITY: &str = "so.tilo.android.TiloActivity";
/// The SDK command-line tools `setup` installs when `sdkmanager` is missing.
const CMDLINE_TOOLS_BUILD: &str = "13114758";
/// Every staged file gets this mtime so an archive does not depend on when it
/// was built (1980-01-02, clear of the zip epoch in every time zone).
const EPOCH_SECS: u64 = 315_619_200;

impl PackageArch {
    /// The Android ABI directory an architecture's libraries go in.
    #[must_use]
    pub const fn android_abi(self) -> &'static str {
        match self {
            Self::Aarch64 => "arm64-v8a",
            Self::X86_64 => "x86_64",
        }
    }

    /// The architecture of an Android ABI name, as `getprop` reports it.
    #[must_use]
    pub fn of_android_abi(abi: &str) -> Option<Self> {
        match abi.trim() {
            "arm64-v8a" => Some(Self::Aarch64),
            "x86_64" => Some(Self::X86_64),
            _ => None,
        }
    }
}

/// Where the Android SDK, NDK and JDK tools this machine has are.
#[derive(Debug, Clone)]
pub struct Toolchain {
    pub sdk: Option<PathBuf>,
    pub ndk: Option<PathBuf>,
    pub build_tools: Option<PathBuf>,
    pub android_jar: Option<PathBuf>,
    /// `$JAVA_HOME/bin`, else `None` for the tools on `PATH`.
    java_bin: Option<PathBuf>,
}

impl Toolchain {
    /// Find the SDK (`$ANDROID_HOME`, `$ANDROID_SDK_ROOT`, then the Android
    /// Studio default), its NDK (`$ANDROID_NDK_HOME`, `$ANDROID_NDK_ROOT`,
    /// `$ANDROID_NDK_LATEST_HOME`, then `<sdk>/ndk/`) and the JDK
    /// (`$JAVA_HOME`, then `PATH`).
    #[must_use]
    pub fn discover() -> Self {
        let sdk = sdk_root();
        let ndk = ndk_root(sdk.as_deref());
        let build_tools = sdk.as_deref().and_then(build_tools_dir);
        let android_jar = sdk
            .as_deref()
            .map(|sdk| sdk.join("platforms").join(PLATFORM).join("android.jar"))
            .filter(|jar| jar.is_file());
        let java_bin = std::env::var_os("JAVA_HOME")
            .map(|home| PathBuf::from(home).join("bin"))
            .filter(|bin| bin.join(exe("javac")).is_file());
        Self {
            sdk,
            ndk,
            build_tools,
            android_jar,
            java_bin,
        }
    }

    fn java(&self, tool: &str) -> PathBuf {
        self.java_bin
            .as_ref()
            .map_or_else(|| PathBuf::from(exe(tool)), |bin| bin.join(exe(tool)))
    }

    fn build_tool(&self, tool: &str) -> Result<PathBuf> {
        let dir = self.build_tools.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "the Android SDK has no build-tools {BUILD_TOOLS_VERSION}; run `tilo-app setup android`"
            )
        })?;
        let script = if cfg!(windows) && matches!(tool, "d8" | "apksigner") {
            format!("{tool}.bat")
        } else {
            exe(tool)
        };
        Ok(dir.join(script))
    }

    fn android_jar(&self) -> Result<&Path> {
        self.android_jar.as_deref().ok_or_else(|| {
            anyhow::anyhow!("the Android SDK has no {PLATFORM}; run `tilo-app setup android`")
        })
    }

    /// `adb`, from the SDK's platform-tools or `PATH`.
    #[must_use]
    pub fn adb(&self) -> PathBuf {
        self.sdk
            .as_ref()
            .map(|sdk| sdk.join("platform-tools").join(exe("adb")))
            .filter(|adb| adb.is_file())
            .unwrap_or_else(|| PathBuf::from(exe("adb")))
    }

    fn ndk_bin(&self) -> Option<PathBuf> {
        let bin = self
            .ndk
            .as_ref()?
            .join("toolchains/llvm/prebuilt")
            .join(ndk_host_tag())
            .join("bin");
        bin.is_dir().then_some(bin)
    }

    /// The environment Cargo needs to compile and link for `triple`: the
    /// NDK's clang as linker and C compiler, and its archiver. A variable the
    /// environment already sets (as the builder image does) is left alone.
    ///
    /// # Errors
    ///
    /// When no NDK is found and the environment names no linker.
    pub fn cargo_env(&self, triple: &str) -> Result<Vec<(String, OsString)>> {
        let upper = triple.to_ascii_uppercase().replace('-', "_");
        let lower = triple.replace('-', "_");
        let linker = format!("CARGO_TARGET_{upper}_LINKER");
        let Some(bin) = self.ndk_bin() else {
            if std::env::var_os(&linker).is_some() {
                return Ok(Vec::new());
            }
            bail!(
                "no Android NDK found for {triple} ($ANDROID_NDK_HOME or <sdk>/ndk/); \
                 run `tilo-app setup android`"
            );
        };
        let clang = |suffix: &str| {
            let name = format!("{triple}{MIN_SDK}-clang{suffix}");
            bin.join(if cfg!(windows) {
                format!("{name}.cmd")
            } else {
                name
            })
        };
        let wanted = [
            (linker, clang("").into_os_string()),
            (format!("CC_{lower}"), clang("").into_os_string()),
            (format!("CXX_{lower}"), clang("++").into_os_string()),
            (
                format!("AR_{lower}"),
                bin.join(exe("llvm-ar")).into_os_string(),
            ),
        ];
        let mut env: Vec<(String, OsString)> = wanted
            .into_iter()
            .filter(|(key, _)| std::env::var_os(key).is_none())
            .collect();
        if let Some(ndk) = &self.ndk {
            for key in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"] {
                if std::env::var_os(key).is_none() {
                    env.push((key.to_owned(), ndk.clone().into_os_string()));
                }
            }
        }
        Ok(env)
    }

    fn strip_tool(&self) -> Option<PathBuf> {
        self.ndk_bin()
            .map(|bin| bin.join(exe("llvm-strip")))
            .filter(|strip| strip.is_file())
    }

    /// bundletool: `$TILO_BUNDLETOOL_JAR`, the jar `setup` installs under the
    /// SDK, else a `bundletool` on `PATH`.
    #[must_use]
    pub fn bundletool(&self) -> Option<Bundletool> {
        if let Some(jar) = std::env::var_os("TILO_BUNDLETOOL_JAR").map(PathBuf::from) {
            return jar.is_file().then_some(Bundletool::Jar(jar));
        }
        if let Some(jar) = self
            .sdk
            .as_ref()
            .map(|sdk| bundletool_jar_path(sdk))
            .filter(|jar| jar.is_file())
        {
            return Some(Bundletool::Jar(jar));
        }
        find_on_path("bundletool").map(Bundletool::Program)
    }
}

/// How bundletool runs.
#[derive(Debug, Clone)]
pub enum Bundletool {
    Jar(PathBuf),
    Program(PathBuf),
}

fn bundletool_jar_path(sdk: &Path) -> PathBuf {
    sdk.join("bundletool")
        .join(format!("bundletool-all-{BUNDLETOOL_VERSION}.jar"))
}

fn exe(tool: &str) -> String {
    if cfg!(windows) {
        format!("{tool}.exe")
    } else {
        tool.to_owned()
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn sdk_root() -> Option<PathBuf> {
    ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .chain(default_sdk_root())
        .find(|dir| dir.is_dir())
}

/// Where Android Studio, and `tilo-app setup android`, put the SDK.
fn default_sdk_root() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        home().map(|home| home.join("Library/Android/sdk"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("Android").join("Sdk"))
    } else {
        home().map(|home| home.join("Android").join("Sdk"))
    }
}

fn ndk_root(sdk: Option<&Path>) -> Option<PathBuf> {
    let from_env = [
        "ANDROID_NDK_HOME",
        "ANDROID_NDK_ROOT",
        "ANDROID_NDK_LATEST_HOME",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .map(PathBuf::from)
    .find(|dir| dir.join("toolchains").is_dir());
    if from_env.is_some() {
        return from_env;
    }
    let sdk = sdk?;
    let pinned = sdk.join("ndk").join(NDK_VERSION);
    if pinned.is_dir() {
        return Some(pinned);
    }
    newest_version_dir(&sdk.join("ndk")).or_else(|| {
        let bundle = sdk.join("ndk-bundle");
        bundle.is_dir().then_some(bundle)
    })
}

/// The pinned build-tools, else the newest installed from 35 on.
fn build_tools_dir(sdk: &Path) -> Option<PathBuf> {
    let root = sdk.join("build-tools");
    let pinned = root.join(BUILD_TOOLS_VERSION);
    if pinned.join(exe("aapt2")).is_file() {
        return Some(pinned);
    }
    newest_version_dir(&root).filter(|dir| {
        dir.join(exe("aapt2")).is_file()
            && dir
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split('.').next())
                .and_then(|major| major.parse::<u32>().ok())
                .is_some_and(|major| major >= 35)
    })
}

fn newest_version_dir(root: &Path) -> Option<PathBuf> {
    let key = |path: &Path| -> Vec<u64> {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                name.split(['.', '-'])
                    .map(|part| part.parse().unwrap_or(0))
                    .collect()
            })
            .unwrap_or_default()
    };
    std::fs::read_dir(root)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .max_by_key(|path| key(path))
}

const fn ndk_host_tag() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin-x86_64"
    } else if cfg!(windows) {
        "windows-x86_64"
    } else {
        "linux-x86_64"
    }
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe(program)))
        .find(|candidate| candidate.is_file())
}

fn tool_runs(program: &Path, arg: &str) -> bool {
    Command::new(program)
        .arg(arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// One requirement of an Android build and whether this machine meets it.
#[derive(Debug, Clone)]
pub struct ToolCheck {
    pub name: &'static str,
    /// What was found, or `None` when it is missing.
    pub found: Option<String>,
    /// How to get it when it is missing.
    pub fix: String,
    /// `false` for what only one feature needs (bundletool: the `.aab`).
    pub required: bool,
}

/// What an Android build needs from the system rather than the Android SDK,
/// which `tilo-app setup android` cannot install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemTool {
    /// `javac`, `keytool` and `jarsigner`: a JDK 17 or newer.
    Jdk,
    Zip,
    Unzip,
}

/// A family of systems, by the package manager that installs a [`SystemTool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageManager {
    /// Fedora, RHEL and their kin.
    Dnf,
    /// Debian, Ubuntu and their kin.
    Apt,
    /// Homebrew on macOS.
    Brew,
}

impl PackageManager {
    pub const ALL: [Self; 3] = [Self::Dnf, Self::Apt, Self::Brew];

    /// This machine's, from `/etc/os-release` on Linux; `None` when the
    /// system is none of the families.
    #[must_use]
    pub fn detect() -> Option<Self> {
        if cfg!(target_os = "macos") {
            return Some(Self::Brew);
        }
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| Self::from_os_release(&text))
    }

    /// The family `os-release` names in `ID`, else in `ID_LIKE`.
    #[must_use]
    pub fn from_os_release(text: &str) -> Option<Self> {
        let value = |key: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(key))
                .map(|value| value.trim().trim_matches(['"', '\'']).to_owned())
                .unwrap_or_default()
        };
        let (id, like) = (value("ID="), value("ID_LIKE="));
        id.split_whitespace()
            .chain(like.split_whitespace())
            .find_map(|id| match id {
                "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "amzn" | "ol" => {
                    Some(Self::Dnf)
                }
                "debian" | "ubuntu" => Some(Self::Apt),
                _ => None,
            })
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Dnf => "Fedora/RHEL (dnf)",
            Self::Apt => "Debian/Ubuntu (apt)",
            Self::Brew => "macOS (brew)",
        }
    }

    fn package(self, tool: SystemTool) -> &'static str {
        match (self, tool) {
            (Self::Dnf, SystemTool::Jdk) => "java-21-openjdk-devel",
            (Self::Apt, SystemTool::Jdk) => "openjdk-17-jdk-headless",
            (Self::Brew, SystemTool::Jdk) => "openjdk@21",
            (_, SystemTool::Zip) => "zip",
            (_, SystemTool::Unzip) => "unzip",
        }
    }

    /// The one command that installs `tools`.
    #[must_use]
    pub fn install_command(self, tools: &[SystemTool]) -> String {
        let packages = tools
            .iter()
            .map(|tool| self.package(*tool))
            .collect::<Vec<_>>()
            .join(" ");
        match self {
            Self::Dnf => format!("sudo dnf install -y {packages}"),
            Self::Apt => format!("sudo apt-get install -y {packages}"),
            Self::Brew => format!("brew install {packages}"),
        }
    }
}

/// How to install `tools` on a system of `manager`'s family, or on each
/// family when the system is unknown.
#[must_use]
pub fn system_install_hint(manager: Option<PackageManager>, tools: &[SystemTool]) -> String {
    let jdk = if tools.contains(&SystemTool::Jdk) {
        " (then put the JDK's bin/ on PATH or set $JAVA_HOME if it is not)"
    } else {
        ""
    };
    let commands = manager.map_or_else(
        || {
            PackageManager::ALL
                .iter()
                .map(|manager| format!("{}: `{}`", manager.name(), manager.install_command(tools)))
                .collect::<Vec<_>>()
                .join("; ")
        },
        |manager| format!("`{}`", manager.install_command(tools)),
    );
    format!("{commands}{jdk}")
}

/// `tilo-app doctor --platform android`.
#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub checks: Vec<ToolCheck>,
    /// What is missing that only the system's package manager installs.
    pub system_missing: Vec<SystemTool>,
    pub package_manager: Option<PackageManager>,
}

impl DoctorReport {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.checks
            .iter()
            .all(|check| !check.required || check.found.is_some())
    }

    #[must_use]
    pub fn summary(&self) -> String {
        let mut text = String::from("Android toolchain\n");
        for check in &self.checks {
            let (mark, detail) = match (&check.found, check.required) {
                (Some(found), _) => ("ok", found.clone()),
                (None, true) => ("missing", check.fix.clone()),
                (None, false) => ("optional", check.fix.clone()),
            };
            let _ = writeln!(text, "  [{mark}] {}: {detail}", check.name);
        }
        if let Some(install) = self.system_install() {
            let _ = writeln!(text, "Install the system packages with {install}");
        }
        text.push_str(if self.is_ready() {
            "Ready to build, install and package Android apps."
        } else {
            "Not ready: run `tilo-app setup android`, or fix the missing items above."
        });
        text
    }

    /// The install command for everything in [`Self::system_missing`].
    #[must_use]
    pub fn system_install(&self) -> Option<String> {
        (!self.system_missing.is_empty())
            .then(|| system_install_hint(self.package_manager, &self.system_missing))
    }

    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "platform": "android",
            "ready": self.is_ready(),
            "system_install": self.system_install(),
            "checks": self.checks.iter().map(|check| serde_json::json!({
                "name": check.name,
                "found": check.found,
                "fix": check.fix,
                "required": check.required,
            })).collect::<Vec<_>>(),
        })
    }
}

/// The JDK and the zip tools, which only the system's packages provide.
fn system_checks(
    tools: &Toolchain,
    package_manager: Option<PackageManager>,
) -> (Vec<ToolCheck>, Vec<SystemTool>) {
    let mut checks = Vec::new();
    let mut system_missing = Vec::new();
    for (name, tool) in [("javac", "javac"), ("keytool", "keytool")] {
        let program = tools.java(tool);
        let found = tool_runs(&program, if tool == "javac" { "-version" } else { "-help" })
            .then(|| program.display().to_string());
        if found.is_none() && !system_missing.contains(&SystemTool::Jdk) {
            system_missing.push(SystemTool::Jdk);
        }
        checks.push(ToolCheck {
            name,
            found,
            fix: format!(
                "a JDK 17 or newer: {}",
                system_install_hint(package_manager, &[SystemTool::Jdk])
            ),
            required: true,
        });
    }
    for (name, tool) in [("zip", SystemTool::Zip), ("unzip", SystemTool::Unzip)] {
        let found = find_on_path(name).map(|path| path.display().to_string());
        if found.is_none() {
            system_missing.push(tool);
        }
        checks.push(ToolCheck {
            name,
            found,
            fix: system_install_hint(package_manager, &[tool]),
            required: true,
        });
    }
    (checks, system_missing)
}

/// Check everything an Android build, install and package needs.
#[must_use]
pub fn doctor() -> DoctorReport {
    let tools = Toolchain::discover();
    let setup = "run `tilo-app setup android`".to_owned();
    let path_of = |path: &Path| path.display().to_string();
    let mut checks = vec![
        ToolCheck {
            name: "Android SDK",
            found: tools.sdk.as_deref().map(path_of),
            fix: format!("set $ANDROID_HOME, or {setup}"),
            required: true,
        },
        ToolCheck {
            name: "build-tools",
            found: tools.build_tools.as_deref().map(path_of),
            fix: format!("sdkmanager 'build-tools;{BUILD_TOOLS_VERSION}', or {setup}"),
            required: true,
        },
        ToolCheck {
            name: "platform",
            found: tools.android_jar.as_deref().map(path_of),
            fix: format!("sdkmanager 'platforms;{PLATFORM}', or {setup}"),
            required: true,
        },
    ];
    let env_linker = PackageArch::ALL.iter().all(|arch| {
        let upper = PackagePlatform::Android
            .triple(*arch)
            .to_ascii_uppercase()
            .replace('-', "_");
        std::env::var_os(format!("CARGO_TARGET_{upper}_LINKER")).is_some()
    });
    checks.push(ToolCheck {
        name: "NDK",
        found: tools
            .ndk_bin()
            .and_then(|_| tools.ndk.as_deref().map(path_of))
            .or_else(|| env_linker.then(|| "linkers from the environment".to_owned())),
        fix: format!("sdkmanager 'ndk;{NDK_VERSION}' or set $ANDROID_NDK_HOME, or {setup}"),
        required: true,
    });
    checks.push(ToolCheck {
        name: "Rust targets",
        found: installed_rust_targets().and_then(|installed| {
            let wanted = android_triples();
            wanted
                .iter()
                .all(|triple| installed.contains(triple))
                .then(|| wanted.join(", "))
        }),
        fix: format!("rustup target add {}", android_triples().join(" ")),
        required: true,
    });
    let package_manager = PackageManager::detect();
    let (system, system_missing) = system_checks(&tools, package_manager);
    checks.extend(system);
    let adb = tools.adb();
    checks.push(ToolCheck {
        name: "adb",
        found: tool_runs(&adb, "version").then(|| adb.display().to_string()),
        fix: format!("sdkmanager platform-tools, or {setup} (`run` and `dev` need it)"),
        required: true,
    });
    checks.push(ToolCheck {
        name: "bundletool",
        found: tools.bundletool().map(|tool| match tool {
            Bundletool::Jar(path) | Bundletool::Program(path) => path.display().to_string(),
        }),
        fix: format!(
            "{setup}, or set $TILO_BUNDLETOOL_JAR (only `package` needs it, for the .aab)"
        ),
        required: false,
    });
    DoctorReport {
        checks,
        system_missing,
        package_manager,
    }
}

fn android_triples() -> Vec<String> {
    PackageArch::ALL
        .iter()
        .map(|arch| PackagePlatform::Android.triple(*arch))
        .collect()
}

fn installed_rust_targets() -> Option<Vec<String>> {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    })
}

/// What `tilo-app setup android` may do without asking.
#[derive(Debug, Clone, Copy, Default)]
pub struct SetupOptions {
    /// Accept the Android SDK licences instead of showing each.
    pub accept_licenses: bool,
}

/// Install what an Android build needs into the SDK (created at the Android
/// Studio default when there is none): the command-line tools, platform-tools,
/// build-tools, the platform, the NDK, bundletool, and the Rust targets. A
/// JDK is the one thing it cannot install; the report names the package.
///
/// # Errors
///
/// When a download or an installer fails.
pub fn setup(options: SetupOptions) -> Result<DoctorReport> {
    let sdk = sdk_root().or_else(default_sdk_root).ok_or_else(|| {
        anyhow::anyhow!("cannot tell where to put the Android SDK; set $ANDROID_HOME")
    })?;
    std::fs::create_dir_all(&sdk).with_context(|| format!("creating {}", sdk.display()))?;
    eprintln!("[tilo-app] Android SDK: {}", sdk.display());
    let before = doctor();
    if let Some(install) = before.system_install() {
        eprintln!(
            "[tilo-app] setup installs the Android SDK, not system packages; install those with {install}"
        );
        if before.system_missing.contains(&SystemTool::Unzip) && sdkmanager_path(&sdk).is_none() {
            bail!(
                "the Android command-line tools come as a zip, so install `unzip` first: {install}"
            );
        }
    }
    let sdkmanager = ensure_sdkmanager(&sdk)?;
    let sdk_arg = format!("--sdk_root={}", sdk.display());
    let mut licenses = Command::new(&sdkmanager);
    licenses.arg(&sdk_arg).arg("--licenses");
    if options.accept_licenses {
        licenses.stdin(Stdio::piped());
        let mut child = licenses.spawn().context("starting sdkmanager --licenses")?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all("y\n".repeat(32).as_bytes());
        }
        child.wait().context("waiting for sdkmanager --licenses")?;
    } else {
        eprintln!("[tilo-app] review and accept the SDK licences (or pass --accept-licenses)");
        licenses.status().context("running sdkmanager --licenses")?;
    }
    let mut packages = vec![
        "platform-tools".to_owned(),
        format!("build-tools;{BUILD_TOOLS_VERSION}"),
        format!("platforms;{PLATFORM}"),
    ];
    // An NDK the environment already names (as the builder image's) serves.
    if Toolchain::discover().ndk_bin().is_none() {
        packages.push(format!("ndk;{NDK_VERSION}"));
    }
    eprintln!("[tilo-app] installing {}", packages.join(", "));
    run(Command::new(&sdkmanager)
        .arg(&sdk_arg)
        .arg("--install")
        .args(&packages))?;

    let jar = bundletool_jar_path(&sdk);
    if !jar.is_file() {
        let url = format!(
            "https://github.com/google/bundletool/releases/download/{BUNDLETOOL_VERSION}/bundletool-all-{BUNDLETOOL_VERSION}.jar"
        );
        download(&url, &jar)?;
    }

    let mut rustup = Command::new("rustup");
    rustup.args(["target", "add"]).args(android_triples());
    if let Err(error) = run(&mut rustup) {
        eprintln!("[tilo-app] {error:#}; add the targets with the toolchain that builds the app");
    }
    if std::env::var_os("ANDROID_HOME").is_none() {
        eprintln!(
            "[tilo-app] found by default; export ANDROID_HOME={} for other tools",
            sdk.display()
        );
    }
    let report = doctor();
    if let Some(install) = report.system_install() {
        eprintln!("[tilo-app] still missing from the system; install with {install}");
    }
    Ok(report)
}

/// The SDK's `sdkmanager`, else one on PATH.
fn sdkmanager_path(sdk: &Path) -> Option<PathBuf> {
    let script = if cfg!(windows) {
        "sdkmanager.bat"
    } else {
        "sdkmanager"
    };
    let installed = sdk.join("cmdline-tools/latest/bin").join(script);
    if installed.is_file() {
        return Some(installed);
    }
    find_on_path("sdkmanager")
}

fn ensure_sdkmanager(sdk: &Path) -> Result<PathBuf> {
    if let Some(found) = sdkmanager_path(sdk) {
        return Ok(found);
    }
    let host = if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(windows) {
        "win"
    } else {
        "linux"
    };
    let url = format!(
        "https://dl.google.com/android/repository/commandlinetools-{host}-{CMDLINE_TOOLS_BUILD}_latest.zip"
    );
    let work = tempfile::tempdir().context("creating a download directory")?;
    let archive = work.path().join("cmdline-tools.zip");
    download(&url, &archive)?;
    run(Command::new("unzip")
        .arg("-q")
        .arg(&archive)
        .arg("-d")
        .arg(work.path()))?;
    let latest = sdk.join("cmdline-tools/latest");
    std::fs::create_dir_all(sdk.join("cmdline-tools"))?;
    if latest.exists() {
        std::fs::remove_dir_all(&latest)?;
    }
    std::fs::rename(work.path().join("cmdline-tools"), &latest)
        .with_context(|| format!("installing {}", latest.display()))?;
    sdkmanager_path(sdk).with_context(|| {
        format!(
            "the command-line tools in {} have no sdkmanager",
            latest.display()
        )
    })
}

fn download(url: &str, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    eprintln!("[tilo-app] downloading {url}");
    let partial = to.with_extension("partial");
    run(Command::new("curl")
        .args([
            "--proto",
            "=https",
            "--tlsv1.2",
            "-fsSL",
            "--retry",
            "3",
            "-o",
        ])
        .arg(&partial)
        .arg(url))?;
    std::fs::rename(&partial, to).with_context(|| format!("saving {}", to.display()))
}

fn run(command: &mut Command) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .with_context(|| format!("starting {program}"))?;
    if !status.success() {
        bail!("{program} failed with {status}");
    }
    Ok(())
}

fn run_quiet(command: &mut Command) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .with_context(|| format!("starting {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed with {}:\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// What the APK's manifest says about the app, from `tilo-app.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidMetadata {
    /// The Cargo package.
    pub package: String,
    /// The Android application id: `[package].id`, with `-` as `_`.
    pub application_id: String,
    /// The launcher label: `[package].name`, else the id's last part.
    pub label: String,
    pub version_name: String,
    pub version_code: u32,
    /// Android permissions the manifest's sandbox grants map to.
    pub permissions: Vec<&'static str>,
}

impl AndroidMetadata {
    /// # Errors
    ///
    /// When the manifest's id or version has no Android spelling.
    pub fn from_manifest(package: &str, manifest: &tilo_app_package::Manifest) -> Result<Self> {
        let id = &manifest.package.id;
        Ok(Self {
            package: package.to_owned(),
            application_id: application_id(id)?,
            label: manifest
                .package
                .name
                .clone()
                .unwrap_or_else(|| label_from_id(id)),
            version_name: manifest.package.version.clone(),
            version_code: version_code(&manifest.package.version)?,
            permissions: permissions(manifest),
        })
    }
}

/// An Android application id for a Tilo package id: at least two
/// dot-separated parts, each a letter then letters, digits or `_`.
///
/// # Errors
///
/// When `id` has no such spelling after `-` becomes `_`.
pub fn application_id(id: &str) -> Result<String> {
    let mapped = id.replace('-', "_");
    let parts: Vec<&str> = mapped.split('.').collect();
    let valid = parts.len() >= 2
        && parts.iter().all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    if !valid {
        bail!(
            "[package].id {id:?} is not an Android application id: it needs two or more \
             dot-separated parts, each starting with a letter"
        );
    }
    Ok(mapped)
}

fn label_from_id(id: &str) -> String {
    let last = id.rsplit('.').next().unwrap_or(id);
    last.split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `versionCode` for a semver `MAJOR.MINOR.PATCH`:
/// `MAJOR * 1_000_000 + MINOR * 1_000 + PATCH`, so every release sorts above
/// the one before. A pre-release shares its release's code.
///
/// # Errors
///
/// When the version is not semver, or a part is out of that range (minor and
/// patch below 1000, major below 2100: Android's limit is 2 100 000 000).
pub fn version_code(version: &str) -> Result<u32> {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let parts: Vec<u32> = core
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("[package].version {version:?} is not MAJOR.MINOR.PATCH"))?;
    let [major, minor, patch] = parts[..] else {
        bail!("[package].version {version:?} is not MAJOR.MINOR.PATCH");
    };
    if major >= 2100 || minor >= 1000 || patch >= 1000 {
        bail!(
            "[package].version {version:?} has no Android versionCode: keep minor and patch \
             below 1000 and major below 2100"
        );
    }
    major
        .checked_mul(1_000_000)
        .zip(minor.checked_mul(1_000))
        .and_then(|(major, minor)| major.checked_add(minor))
        .and_then(|code| code.checked_add(patch))
        .ok_or_else(|| anyhow::anyhow!("[package].version {version:?} overflows a versionCode"))
}

/// The Android permissions the manifest's grants need. Notifications are the
/// toolkit's own service and asked for at run time; nothing else is granted
/// that the sandbox does not.
#[must_use]
pub fn permissions(manifest: &tilo_app_package::Manifest) -> Vec<&'static str> {
    use tilo_app_package::manifest::{NetMode, PortalPolicy};
    let mut granted = BTreeSet::from(["android.permission.POST_NOTIFICATIONS"]);
    if manifest.sandbox.net.mode != NetMode::Off {
        granted.insert("android.permission.INTERNET");
        granted.insert("android.permission.ACCESS_NETWORK_STATE");
    }
    for (portal, policy) in &manifest.sandbox.portals {
        if *policy == PortalPolicy::Deny {
            continue;
        }
        match portal.as_str() {
            "location" => {
                granted.insert("android.permission.ACCESS_COARSE_LOCATION");
                granted.insert("android.permission.ACCESS_FINE_LOCATION");
            }
            "camera" => {
                granted.insert("android.permission.CAMERA");
            }
            "microphone" => {
                granted.insert("android.permission.RECORD_AUDIO");
            }
            _ => {}
        }
    }
    granted.into_iter().collect()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The `AndroidManifest.xml` of an APK whose activity loads `lib<lib_name>.so`.
#[must_use]
pub fn android_manifest(app: &AndroidMetadata, lib_name: &str, debuggable: bool) -> String {
    let id = xml_escape(&app.application_id);
    let permissions = app
        .permissions
        .iter()
        .fold(String::new(), |mut text, permission| {
            let _ = writeln!(
                text,
                "    <uses-permission android:name=\"{permission}\" />"
            );
            text
        });
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    package="{id}"
    android:versionCode="{code}"
    android:versionName="{name}">

    <!-- Rendering is wgpu on Vulkan; API {MIN_SDK} is the first level that requires Vulkan 1.1 on
         64-bit devices. -->
    <uses-feature android:name="android.hardware.vulkan.version"
        android:version="0x401000" android:required="true" />

{permissions}
    <!-- TextToSpeech binds the engine by this action, which API 30's package visibility hides
         unless it is queried. -->
    <queries>
        <intent>
            <action android:name="android.intent.action.TTS_SERVICE" />
        </intent>
    </queries>

    <application
        android:label="{label}"
        android:icon="@mipmap/ic_launcher"
        android:hasCode="true"
        android:debuggable="{debuggable}"
        android:extractNativeLibs="true"
        android:hardwareAccelerated="true"
        android:allowBackup="false">

        <!-- The toolkit adapts to rotation, resizing, keyboards, density, locale and theme
             itself; recreating the activity would restart the app's process (see
             TiloActivity.onDestroy). enableOnBackInvokedCallback keeps Back a key event the
             app sees. -->
        <activity
            android:name="{ACTIVITY}"
            android:exported="true"
            android:launchMode="singleTask"
            android:theme="@android:style/Theme.DeviceDefault.NoActionBar"
            android:windowSoftInputMode="adjustResize"
            android:enableOnBackInvokedCallback="false"
            android:configChanges="orientation|screenSize|screenLayout|smallestScreenSize|keyboard|keyboardHidden|navigation|touchscreen|density|fontScale|fontWeightAdjustment|locale|layoutDirection|uiMode|colorMode|mcc|mnc|grammaticalGender">
            <meta-data android:name="android.app.lib_name" android:value="{lib_name}" />
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
        </activity>

        <!-- "Open with": serves only descriptors the app shared, read-only, to the app granted
             the one URI. -->
        <provider
            android:name="so.tilo.android.TiloFileProvider"
            android:authorities="{id}.tilo.files"
            android:exported="false"
            android:grantUriPermissions="true" />
    </application>
</manifest>
"#,
        code = app.version_code,
        name = xml_escape(&app.version_name),
        label = xml_escape(&app.label),
    )
}

/// Which Cargo target is the app's Android library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryTarget {
    /// A `cdylib` example named `<package>-android`, built with the
    /// `android` feature (Calculator and Notes).
    Example(String),
    /// The package library, built as a `cdylib`; `ui::dev_reload_entry!`
    /// defines its `android_main`.
    Lib,
}

/// The app's Android library target.
///
/// # Errors
///
/// When the app has neither an `<package>-android` example nor a library
/// that calls `ui::dev_reload_entry!`.
pub fn library_target(app_dir: &Path, package: &str) -> Result<LibraryTarget> {
    let manifest_path = app_dir.join("Cargo.toml");
    let manifest: toml::Table = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?
        .parse()
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    let example = format!("{package}-android");
    let declares_example = manifest
        .get("example")
        .and_then(toml::Value::as_array)
        .is_some_and(|examples| {
            examples.iter().any(|entry| {
                entry.get("name").and_then(toml::Value::as_str) == Some(example.as_str())
            })
        });
    if declares_example {
        return Ok(LibraryTarget::Example(example));
    }
    let lib_path = manifest
        .get("lib")
        .and_then(|lib| lib.get("path"))
        .and_then(toml::Value::as_str)
        .unwrap_or("src/lib.rs");
    let lib = std::fs::read_to_string(app_dir.join(lib_path)).unwrap_or_default();
    if lib.contains("dev_reload_entry!") {
        return Ok(LibraryTarget::Lib);
    }
    bail!(
        "{} has no Android entry: call `ui::dev_reload_entry!(App);` at the root of its library \
         (as `tilo-app new` does), or declare a `{example}` cdylib example (docs/android.md)",
        app_dir.display()
    )
}

/// The Cargo command that builds the Android library for `triple`.
#[must_use]
pub fn library_cargo_args(
    manifest: &Path,
    target: &LibraryTarget,
    triple: &str,
    profile: &str,
    features: &[String],
    no_default_features: bool,
) -> Vec<OsString> {
    let mut features = features.to_vec();
    let mut args: Vec<OsString> = match target {
        LibraryTarget::Example(example) => {
            if !features.iter().any(|feature| feature == "android") {
                features.push("android".to_owned());
            }
            vec!["build".into(), "--example".into(), example.into()]
        }
        LibraryTarget::Lib => vec![
            "rustc".into(),
            "--lib".into(),
            "--crate-type".into(),
            "cdylib".into(),
        ],
    };
    args.extend([
        "--manifest-path".into(),
        manifest.as_os_str().to_os_string(),
        "--target".into(),
        triple.into(),
        "--profile".into(),
        profile.into(),
        "--message-format=json-render-diagnostics".into(),
    ]);
    if no_default_features {
        args.push("--no-default-features".into());
    }
    if !features.is_empty() {
        args.push("--features".into());
        args.push(features.join(",").into());
    }
    args
}

fn cargo_program() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

/// Build the library and return the `.so` Cargo reported.
fn build_library(
    app_dir: &Path,
    tools: &Toolchain,
    args: &[OsString],
    triple: &str,
) -> Result<PathBuf> {
    let cargo = cargo_program();
    let mut command = Command::new(&cargo);
    command
        .args(args)
        .current_dir(app_dir)
        .envs(tools.cargo_env(triple)?)
        .stdout(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {}", cargo.to_string_lossy()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Cargo has no artifact stream"))?;
    let expected = app_dir
        .join("Cargo.toml")
        .canonicalize()
        .unwrap_or_else(|_| app_dir.join("Cargo.toml"));
    let mut library = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading Cargo's artifact stream")?;
        if let Some(found) = shared_library_artifact(&line, &expected) {
            library = Some(found);
        }
    }
    let status = child.wait().context("waiting for Cargo")?;
    if !status.success() {
        bail!("Cargo's Android build for {triple} failed with {status}");
    }
    library.ok_or_else(|| anyhow::anyhow!("Cargo built no shared library for {triple}"))
}

/// The `.so` a Cargo `compiler-artifact` line reports for the package at
/// `manifest`; other lines are printed (rendered diagnostics) or ignored.
#[must_use]
pub fn shared_library_artifact(line: &str, manifest: &Path) -> Option<PathBuf> {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
        eprintln!("{line}");
        return None;
    };
    match message.get("reason").and_then(serde_json::Value::as_str) {
        Some("compiler-message") => {
            if let Some(rendered) = message
                .get("message")
                .and_then(|message| message.get("rendered"))
                .and_then(serde_json::Value::as_str)
            {
                eprint!("{rendered}");
            }
            None
        }
        Some("compiler-artifact") => {
            let reported = message
                .get("manifest_path")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)?;
            let reported = reported.canonicalize().unwrap_or(reported);
            if reported != manifest {
                return None;
            }
            message
                .get("filenames")?
                .as_array()?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .find(|name| {
                    Path::new(name)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("so"))
                })
                .map(PathBuf::from)
        }
        _ => None,
    }
}

/// A keystore entry `apksigner` and `jarsigner` sign with.
#[derive(Debug, Clone)]
pub struct Keystore {
    pub path: PathBuf,
    pub store_pass: String,
    pub alias: String,
    pub key_pass: String,
}

/// The signing tools read the keystore passwords from these variables of
/// their own environment, never from argv, which other users can list.
const STORE_PASS_ENV: &str = "TILO_ANDROID_SIGNING_STORE_PASS";
const KEY_PASS_ENV: &str = "TILO_ANDROID_SIGNING_KEY_PASS";

impl Keystore {
    fn password_env(&self) -> [(&'static str, &str); 2] {
        [
            (STORE_PASS_ENV, self.store_pass.as_str()),
            (KEY_PASS_ENV, self.key_pass.as_str()),
        ]
    }
}

/// Which key signs a build.
#[derive(Debug, Clone, Default)]
pub enum Signing {
    /// `~/.android/debug.keystore`, the key Android Studio also signs debug
    /// builds with, created on first use.
    #[default]
    Debug,
    /// A release key: the given keystore, `$TILO_ANDROID_KEYSTORE`, else the
    /// release keystore kept beside the publisher key.
    Release(Option<PathBuf>),
}

impl Signing {
    fn keystore(&self, tools: &Toolchain) -> Result<Keystore> {
        match self {
            Self::Debug => debug_keystore(tools),
            Self::Release(path) => release_keystore(tools, path.as_deref()),
        }
    }
}

fn debug_keystore(tools: &Toolchain) -> Result<Keystore> {
    let path = home()
        .ok_or_else(|| anyhow::anyhow!("no home directory for ~/.android/debug.keystore"))?
        .join(".android")
        .join("debug.keystore");
    let keystore = Keystore {
        path,
        store_pass: "android".to_owned(),
        alias: "androiddebugkey".to_owned(),
        key_pass: "android".to_owned(),
    };
    if !keystore.path.is_file() {
        generate_keystore(tools, &keystore, "CN=Android Debug,O=Android,C=US")?;
    }
    Ok(keystore)
}

fn release_keystore(tools: &Toolchain, explicit: Option<&Path>) -> Result<Keystore> {
    let env = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
    let named = explicit
        .map(Path::to_path_buf)
        .or_else(|| env("TILO_ANDROID_KEYSTORE").map(PathBuf::from));
    if let Some(path) = named {
        if !path.is_file() {
            bail!("Android keystore {} does not exist", path.display());
        }
        let store_pass = env("TILO_ANDROID_KEYSTORE_PASS").ok_or_else(|| {
            anyhow::anyhow!(
                "set $TILO_ANDROID_KEYSTORE_PASS to the password of {}",
                path.display()
            )
        })?;
        return Ok(Keystore {
            path,
            key_pass: env("TILO_ANDROID_KEY_PASS").unwrap_or_else(|| store_pass.clone()),
            store_pass,
            alias: env("TILO_ANDROID_KEY_ALIAS").unwrap_or_else(|| "tilo-release".to_owned()),
        });
    }
    let dir = registry_client::key::default_path()
        .and_then(|key| key.parent().map(Path::to_path_buf))
        .ok_or_else(|| {
            anyhow::anyhow!("no configuration directory for the Android release keystore; set $TILO_ANDROID_KEYSTORE")
        })?;
    let path = dir.join("android-release.keystore");
    let pass_file = dir.join("android-release.keystore.pass");
    if path.is_file() {
        let store_pass = std::fs::read_to_string(&pass_file)
            .with_context(|| format!("reading {}", pass_file.display()))?
            .trim()
            .to_owned();
        return Ok(Keystore {
            path,
            key_pass: store_pass.clone(),
            store_pass,
            alias: "tilo-release".to_owned(),
        });
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let store_pass = hex::encode(tilo_app_package::SigningIdentity::generate().public_key_bytes());
    write_private(&pass_file, &store_pass)?;
    let keystore = Keystore {
        path,
        key_pass: store_pass.clone(),
        store_pass,
        alias: "tilo-release".to_owned(),
    };
    generate_keystore(tools, &keystore, "CN=Tilo app publisher")?;
    eprintln!(
        "[tilo-app] created the Android release key {} (password in {}); back both up: \
         every update of a published app must be signed with the same key",
        keystore.path.display(),
        pass_file.display()
    );
    Ok(keystore)
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

fn generate_keystore(tools: &Toolchain, keystore: &Keystore, dname: &str) -> Result<()> {
    if let Some(parent) = keystore.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    run_quiet(
        Command::new(tools.java("keytool"))
            .arg("-genkeypair")
            .arg("-keystore")
            .arg(&keystore.path)
            .args(["-storetype", "PKCS12", "-storepass:env", STORE_PASS_ENV])
            .args(["-keypass:env", KEY_PASS_ENV, "-alias", &keystore.alias])
            .envs(keystore.password_env())
            .args(["-keyalg", "RSA", "-keysize", "4096", "-validity", "10000"])
            .args(["-dname", dname]),
    )
    .context("creating an Android signing key with keytool")
}

/// What `build`, `run`, `dev` and `package` ask of an Android build.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub arch: PackageArch,
    /// Cargo profile; `dev` unless `package`.
    pub profile: String,
    pub signing: Signing,
    /// Also produce an Android App Bundle (needs bundletool).
    pub bundle: bool,
    pub features: Vec<String>,
    pub no_default_features: bool,
}

/// An APK (and bundle) an Android build left under `target/android/`.
#[derive(Debug, Clone)]
pub struct AndroidBuild {
    pub app_dir: PathBuf,
    pub metadata: AndroidMetadata,
    pub arch: PackageArch,
    pub profile: String,
    pub apk: PathBuf,
    pub bundle: Option<PathBuf>,
    pub elapsed: Duration,
}

impl AndroidBuild {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "app_dir": self.app_dir,
            "platform": "android",
            "application_id": self.metadata.application_id,
            "version_code": self.metadata.version_code,
            "arch": self.arch.as_str(),
            "abi": self.arch.android_abi(),
            "target": PackagePlatform::Android.triple(self.arch),
            "profile": self.profile,
            "apk": self.apk,
            "bundle": self.bundle,
            "elapsed_ms": u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX),
        })
    }
}

/// Where the SDK checkout the app builds against keeps the Android layer,
/// and Cargo's target directory.
struct Checkout {
    android: PathBuf,
    fonts: PathBuf,
    target_dir: PathBuf,
}

fn checkout(app_dir: &Path) -> Result<Checkout> {
    let cargo = cargo_program();
    let output = Command::new(&cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(app_dir.join("Cargo.toml"))
        .current_dir(app_dir)
        .stderr(Stdio::inherit())
        .output()
        .context("running cargo metadata")?;
    if !output.status.success() {
        bail!("cargo metadata failed with {}", output.status);
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing cargo metadata")?;
    let target_dir = metadata
        .get("target_directory")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("cargo metadata names no target directory"))?;
    let root = super::sdk_root_from_metadata(&metadata).ok_or_else(|| {
        anyhow::anyhow!(
            "{} does not build against the Tilo SDK (`tilo-app`)",
            app_dir.display()
        )
    })?;
    let ui_core = root.join("crates/sdk/ui-core");
    let android = ui_core.join("android");
    if !android.join("java").is_dir() {
        bail!(
            "the Tilo SDK at {} has no Android layer ({}); update the SDK",
            root.display(),
            android.display()
        );
    }
    Ok(Checkout {
        android,
        fonts: ui_core.join("assets/fonts"),
        target_dir,
    })
}

/// Build a signed APK (and, when asked, a bundle) of the app at `start`.
///
/// # Errors
///
/// When preflight, the toolchain, Cargo or an Android build tool fails.
pub fn build(start: &Path, request: &BuildRequest) -> Result<AndroidBuild> {
    let started = Instant::now();
    let app_dir = super::resolve_app_dir(start)?;
    let report = super::doctor_resolved_app(app_dir.clone());
    if !report.is_ready() {
        bail!("app preflight failed\n\n{}", report.summary());
    }
    let spec = super::load_app_package_spec(app_dir.clone())?;
    let manifest_source = std::fs::read_to_string(&spec.manifest_path)
        .with_context(|| format!("reading {}", spec.manifest_path.display()))?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&manifest_source)
        .with_context(|| format!("parsing {}", spec.manifest_path.display()))?;
    let metadata = AndroidMetadata::from_manifest(&spec.cargo_package, &manifest)?;
    let library = library_target(&app_dir, &spec.cargo_package)?;
    super::validate_profile_name(&request.profile)?;

    let tools = Toolchain::discover();
    let android_jar = tools.android_jar()?.to_path_buf();
    let keystore = request.signing.keystore(&tools)?;
    let checkout = checkout(&app_dir)?;
    let triple = PackagePlatform::Android.triple(request.arch);
    eprintln!(
        "[tilo-app] building {} for Android {} ({triple}, {})",
        metadata.application_id,
        request.arch.android_abi(),
        request.profile
    );
    let built = build_library(
        &app_dir,
        &tools,
        &library_cargo_args(
            &app_dir.join("Cargo.toml"),
            &library,
            &triple,
            &request.profile,
            &request.features,
            request.no_default_features,
        ),
        &triple,
    )?;

    let out = checkout
        .target_dir
        .join("android")
        .join(&spec.cargo_package)
        .join(&request.profile);
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let stage = tempfile::Builder::new()
        .prefix(".stage-")
        .tempdir_in(&out)
        .context("creating the APK staging area")?;
    let stage = stage.path();
    let staged = stage_apk(
        &tools,
        &android_jar,
        &checkout,
        &app_dir,
        &metadata,
        &built,
        request,
        stage,
    )?;
    let name = format!("{}-{}", spec.cargo_package, request.arch.android_abi());
    let apk = out.join(format!("{name}.apk"));
    link_apk(&tools, &android_jar, &staged, &keystore, &apk)?;
    let bundle = if request.bundle {
        Some(build_bundle(
            &tools,
            &android_jar,
            &staged,
            &out.join(format!("{name}.aab")),
            &keystore,
        )?)
    } else {
        None
    };
    Ok(AndroidBuild {
        app_dir,
        metadata,
        arch: request.arch,
        profile: request.profile.clone(),
        apk,
        bundle,
        elapsed: started.elapsed(),
    })
}

/// The parts of an APK before linking: the manifest, assets, launcher icon,
/// and the dex and library `aapt2` does not link.
struct StagedApk {
    stage: PathBuf,
    manifest: PathBuf,
    assets: PathBuf,
    icon: PathBuf,
    content: PathBuf,
}

fn stage_apk(
    tools: &Toolchain,
    android_jar: &Path,
    checkout: &Checkout,
    app_dir: &Path,
    metadata: &AndroidMetadata,
    built: &Path,
    request: &BuildRequest,
    stage: &Path,
) -> Result<StagedApk> {
    let lib_file = built
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", built.display()))?
        .to_owned();
    let lib_name = lib_file
        .to_string_lossy()
        .trim_start_matches("lib")
        .trim_end_matches(".so")
        .to_owned();

    // Everything in the APK besides what aapt2 links: the dex and the library.
    let content = stage.join("content");
    let abi_dir = content.join("lib").join(request.arch.android_abi());
    std::fs::create_dir_all(&abi_dir)?;
    let staged_lib = abi_dir.join(&lib_file);
    match tools.strip_tool() {
        // DWARF goes; the symbol table stays, so backtraces still name functions.
        Some(strip) => run_quiet(
            Command::new(strip)
                .arg("--strip-debug")
                .arg("-o")
                .arg(&staged_lib)
                .arg(built),
        )?,
        None => {
            std::fs::copy(built, &staged_lib)
                .with_context(|| format!("copying {}", built.display()))?;
        }
    }
    compile_java(
        tools,
        android_jar,
        &checkout.android.join("java"),
        stage,
        &content,
        request,
    )?;

    let assets = stage.join("assets");
    let fonts = assets.join("tilo").join("fonts");
    std::fs::create_dir_all(&fonts)?;
    super::copy_payload_tree(&checkout.fonts, &fonts)
        .with_context(|| format!("copying the font catalog {}", checkout.fonts.display()))?;
    let icon = compile_icon(tools, app_dir, metadata, stage)?;
    let debuggable = matches!(request.signing, Signing::Debug);
    let manifest_path = stage.join("AndroidManifest.xml");
    std::fs::write(
        &manifest_path,
        android_manifest(metadata, &lib_name, debuggable),
    )?;
    set_epoch(stage)?;

    Ok(StagedApk {
        stage: stage.to_path_buf(),
        manifest: manifest_path,
        assets,
        icon,
        content,
    })
}

/// Link, align and sign the staged APK into `apk`.
fn link_apk(
    tools: &Toolchain,
    android_jar: &Path,
    staged: &StagedApk,
    keystore: &Keystore,
    apk: &Path,
) -> Result<()> {
    let linked = staged.stage.join("linked.apk");
    aapt2_link(tools, android_jar, staged, &linked, false)?;
    zip_add(&linked, &staged.content)?;
    let aligned = staged.stage.join("aligned.apk");
    run_quiet(
        Command::new(tools.build_tool("zipalign")?)
            .args(["-f", "-P", "16", "4"])
            .arg(&linked)
            .arg(&aligned),
    )?;
    run_quiet(
        Command::new(tools.build_tool("apksigner")?)
            .args(["sign", "--ks"])
            .arg(&keystore.path)
            .arg("--ks-pass")
            .arg(format!("env:{STORE_PASS_ENV}"))
            .arg("--key-pass")
            .arg(format!("env:{KEY_PASS_ENV}"))
            .envs(keystore.password_env())
            .arg("--ks-key-alias")
            .arg(&keystore.alias)
            .arg("--out")
            .arg(apk)
            .arg(&aligned),
    )
    .context("signing the APK")
}

fn compile_java(
    tools: &Toolchain,
    android_jar: &Path,
    sources: &Path,
    stage: &Path,
    content: &Path,
    request: &BuildRequest,
) -> Result<()> {
    let mut java_sources = Vec::new();
    collect_files(sources, &mut java_sources)?;
    java_sources.retain(|path| path.extension().is_some_and(|ext| ext == "java"));
    java_sources.sort();
    let classes = stage.join("classes");
    std::fs::create_dir_all(&classes)?;
    run_quiet(
        Command::new(tools.java("javac"))
            .args(["--release", JAVA_RELEASE, "-classpath"])
            .arg(android_jar)
            .arg("-d")
            .arg(&classes)
            .args(["-Xlint:all,-options", "-Werror"])
            .args(&java_sources),
    )
    .context("compiling the Android Java layer (a JDK 17 or newer is needed)")?;
    let mut class_files = Vec::new();
    collect_files(&classes, &mut class_files)?;
    class_files.retain(|path| path.extension().is_some_and(|ext| ext == "class"));
    class_files.sort();
    let mode = if matches!(request.signing, Signing::Debug) {
        "--debug"
    } else {
        "--release"
    };
    run_quiet(
        Command::new(tools.build_tool("d8")?)
            .arg(mode)
            .args(["--min-api", &MIN_SDK.to_string(), "--lib"])
            .arg(android_jar)
            .arg("--output")
            .arg(content)
            .args(&class_files),
    )
    .context("dexing the Android Java layer")
}

/// The launcher icon: the app's `icon.png`, else one drawn from the id.
/// Returns the compiled resource `aapt2 link` takes.
fn compile_icon(
    tools: &Toolchain,
    app_dir: &Path,
    metadata: &AndroidMetadata,
    stage: &Path,
) -> Result<PathBuf> {
    let res = stage.join("res").join("mipmap-xxxhdpi");
    std::fs::create_dir_all(&res)?;
    let icon = res.join("ic_launcher.png");
    let authored = app_dir.join("icon.png");
    if authored.is_file() {
        let image = image::open(&authored)
            .with_context(|| format!("reading {}", authored.display()))?
            .resize_to_fill(192, 192, image::imageops::FilterType::Lanczos3);
        image
            .save(&icon)
            .with_context(|| format!("writing {}", icon.display()))?;
    } else {
        default_icon(&metadata.application_id)
            .save(&icon)
            .with_context(|| format!("writing {}", icon.display()))?;
    }
    let compiled = stage.join("compiled");
    std::fs::create_dir_all(&compiled)?;
    run_quiet(
        Command::new(tools.build_tool("aapt2")?)
            .arg("compile")
            .arg("-o")
            .arg(&compiled)
            .arg(&icon),
    )
    .context("compiling the launcher icon")?;
    Ok(compiled.join("mipmap-xxxhdpi_ic_launcher.png.flat"))
}

/// The generated launcher icon's edge and disc radius, in doubled pixel
/// coordinates so pixel centres are integers.
const ICON_SIZE: i64 = 192;
const ICON_RADIUS: i64 = ICON_SIZE - 16;

/// Launcher colours for an app with no icon of its own.
const ICON_COLOURS: [[u8; 3]; 8] = [
    [0x2f, 0x6f, 0xd6],
    [0x1f, 0x9d, 0x8b],
    [0x3c, 0x9a, 0x3f],
    [0xc2, 0x7c, 0x0e],
    [0xc8, 0x4b, 0x31],
    [0xb8, 0x36, 0x7a],
    [0x7a, 0x4f, 0xc9],
    [0x55, 0x60, 0x6e],
];

/// A disc in a colour of the application id, on transparency, for an app
/// with no `icon.png` of its own.
#[must_use]
pub fn default_icon(application_id: &str) -> image::RgbaImage {
    let hash = application_id
        .bytes()
        .fold(0_u8, |hash, byte| hash.rotate_left(3) ^ byte);
    let [r, g, b] = ICON_COLOURS
        .get(usize::from(hash & 7))
        .copied()
        .unwrap_or([0x55, 0x60, 0x6e]);
    image::RgbaImage::from_fn(192, 192, |x, y| {
        let offset = |coordinate: u32| {
            i64::from(coordinate)
                .saturating_mul(2)
                .saturating_add(1)
                .saturating_sub(ICON_SIZE)
        };
        let (dx, dy) = (offset(x), offset(y));
        let inside = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
            <= ICON_RADIUS.saturating_mul(ICON_RADIUS);
        image::Rgba([r, g, b, if inside { 255 } else { 0 }])
    })
}

fn aapt2_link(
    tools: &Toolchain,
    android_jar: &Path,
    staged: &StagedApk,
    output: &Path,
    proto: bool,
) -> Result<()> {
    let mut command = Command::new(tools.build_tool("aapt2")?);
    command
        .arg("link")
        .arg("-o")
        .arg(output)
        .arg("-I")
        .arg(android_jar)
        .arg("--manifest")
        .arg(&staged.manifest)
        .args(["--min-sdk-version", &MIN_SDK.to_string()])
        .args(["--target-sdk-version", &TARGET_SDK.to_string()])
        .arg("-A")
        .arg(&staged.assets)
        .arg(&staged.icon);
    if proto {
        command.arg("--proto-format");
    }
    run_quiet(&mut command).context("linking the APK with aapt2")
}

/// Add every file under `dir` to the zip `archive`, at its path below `dir`,
/// in sorted order.
fn zip_add(archive: &Path, dir: &Path) -> Result<()> {
    let mut files = Vec::new();
    collect_files(dir, &mut files)?;
    let mut names: Vec<String> = files
        .iter()
        .filter_map(|file| file.strip_prefix(dir).ok())
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .collect();
    names.sort();
    let mut child = Command::new("zip")
        .args(["-q", "-X", "-D"])
        .arg(archive)
        .arg("-@")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .spawn()
        .context("starting zip (install Info-ZIP `zip`)")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(format!("{}\n", names.join("\n")).as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("zip failed with {status}");
    }
    Ok(())
}

fn build_bundle(
    tools: &Toolchain,
    android_jar: &Path,
    staged: &StagedApk,
    output: &Path,
    keystore: &Keystore,
) -> Result<PathBuf> {
    let stage = &staged.stage;
    let content = &staged.content;
    let bundletool = tools.bundletool().ok_or_else(|| {
        anyhow::anyhow!(
            "an Android App Bundle needs bundletool {BUNDLETOOL_VERSION}; run `tilo-app setup android` \
             or set $TILO_BUNDLETOOL_JAR (or pass --no-bundle)"
        )
    })?;
    let proto = stage.join("proto.apk");
    aapt2_link(tools, android_jar, staged, &proto, true)?;
    // A bundle module: the manifest under manifest/, dex under dex/, and
    // everything else where the APK has it.
    let module = stage.join("module");
    std::fs::create_dir_all(&module)?;
    run_quiet(
        Command::new("unzip")
            .arg("-q")
            .arg(&proto)
            .arg("-d")
            .arg(&module),
    )
    .context("unpacking the linked bundle module (install `unzip`)")?;
    std::fs::create_dir_all(module.join("manifest"))?;
    std::fs::rename(
        module.join("AndroidManifest.xml"),
        module.join("manifest/AndroidManifest.xml"),
    )?;
    std::fs::create_dir_all(module.join("dex"))?;
    for entry in std::fs::read_dir(content)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".dex") {
            std::fs::copy(entry.path(), module.join("dex").join(&name))?;
        }
    }
    super::copy_payload_tree(&content.join("lib"), &{
        let lib = module.join("lib");
        std::fs::create_dir_all(&lib)?;
        lib
    })?;
    set_epoch(&module)?;
    let base = stage.join("base.zip");
    zip_add(&base, &module)?;
    let unsigned = stage.join("unsigned.aab");
    let mut command = match &bundletool {
        Bundletool::Jar(jar) => {
            let mut command = Command::new(tools.java("java"));
            command.arg("-jar").arg(jar);
            command
        }
        Bundletool::Program(program) => Command::new(program),
    };
    run_quiet(
        command
            .arg("build-bundle")
            .arg(format!("--modules={}", base.display()))
            .arg(format!("--output={}", unsigned.display()))
            .arg("--overwrite"),
    )
    .context("building the Android App Bundle with bundletool")?;
    run_quiet(
        Command::new(tools.java("jarsigner"))
            .arg("-keystore")
            .arg(&keystore.path)
            .args([
                "-storepass:env",
                STORE_PASS_ENV,
                "-keypass:env",
                KEY_PASS_ENV,
            ])
            .envs(keystore.password_env())
            .args([
                "-sigalg",
                "SHA256withRSA",
                "-digestalg",
                "SHA-256",
                "-signedjar",
            ])
            .arg(output)
            .arg(&unsigned)
            .arg(&keystore.alias),
    )
    .context("signing the Android App Bundle")?;
    Ok(output.to_path_buf())
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

fn set_epoch(dir: &Path) -> Result<()> {
    let epoch = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(EPOCH_SECS))
        .ok_or_else(|| anyhow::anyhow!("no 1980 on this clock"))?;
    let mut files = Vec::new();
    collect_files(dir, &mut files)?;
    for file in files {
        std::fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|handle| handle.set_modified(epoch))
            .with_context(|| format!("setting the mtime of {}", file.display()))?;
    }
    Ok(())
}

/// `tilo-app package --platform android`: for each architecture, a
/// release-signed APK, the bundle when bundletool is there, and a signed
/// `.tapp` carrying the APK for the registry, all in `dist/`. `--arch` picks
/// one architecture; without it both are packaged, since the registry lists
/// one build per architecture and a phone and an emulator want different ones.
///
/// # Errors
///
/// When the build, signing or packing fails, when an output exists without
/// `force` (checked for every architecture before anything is built), or when
/// `--output` names one file for several architectures.
pub fn package(
    start: &Path,
    request: &PackageRequest,
    options: &PackageOptions,
) -> Result<Vec<PackageOutcome>> {
    let arches = request
        .arch
        .map_or_else(|| PackageArch::ALL.to_vec(), |arch| vec![arch]);
    if arches.len() > 1 && request.output.is_some() {
        bail!("--output names one package; add --arch aarch64 or --arch x86_64 to pick its build");
    }
    let app_dir = super::resolve_app_dir(start)?;
    let keystore = options
        .keystore
        .as_deref()
        .map(|path| super::absolute_from(&app_dir, path));
    if request.static_link || request.target.is_some() {
        bail!("--static and --target are not for --platform android");
    }
    let tools = Toolchain::discover();
    let bundle = options.bundle.unwrap_or_else(|| {
        let found = tools.bundletool().is_some();
        if !found {
            eprintln!("[tilo-app] no bundletool, so no .aab; run `tilo-app setup android` for one");
        }
        found
    });
    let spec = super::load_app_package_spec(app_dir)?;
    let planned = arches
        .into_iter()
        .map(|arch| Ok((arch, PackagePaths::plan(&spec, request, arch)?)))
        .collect::<Result<Vec<_>>>()?;
    for (_, paths) in &planned {
        for path in [&paths.output, &paths.apk, &paths.bundle] {
            if path.exists() && !request.force {
                bail!(
                    "{} already exists; pass --force to replace it",
                    path.display()
                );
            }
        }
    }

    let mut outcomes = Vec::new();
    for (arch, paths) in planned {
        std::fs::create_dir_all(&paths.parent)?;
        let build = build(
            start,
            &BuildRequest {
                arch,
                profile: request
                    .profile
                    .clone()
                    .unwrap_or_else(|| "release".to_owned()),
                signing: Signing::Release(keystore.clone()),
                bundle,
                features: request.features.clone(),
                no_default_features: request.no_default_features,
            },
        )?;
        let (entrypoints, size) =
            pack_tapp(&spec, &build, arch, request, &paths.parent, &paths.output)?;
        std::fs::copy(&build.apk, &paths.apk)?;
        let mut artifacts = vec![paths.apk];
        if let Some(bundle) = &build.bundle {
            std::fs::copy(bundle, &paths.bundle)?;
            artifacts.push(paths.bundle);
        }
        for artifact in &artifacts {
            eprintln!("[tilo-app] wrote {}", artifact.display());
        }
        outcomes.push(PackageOutcome {
            output: paths.output,
            entrypoints,
            size,
        });
    }
    Ok(outcomes)
}

/// Where one architecture's package goes: the `.tapp`, and the `.apk` and
/// `.aab` named after it.
struct PackagePaths {
    output: PathBuf,
    parent: PathBuf,
    apk: PathBuf,
    bundle: PathBuf,
}

impl PackagePaths {
    fn plan(
        spec: &super::AppPackageSpec,
        request: &PackageRequest,
        arch: PackageArch,
    ) -> Result<Self> {
        let output = request.output.as_ref().map_or_else(
            || {
                spec.app_dir.join("dist").join(format!(
                    "{}-{}-{}.tapp",
                    spec.cargo_package,
                    PackagePlatform::Android.registry_name(),
                    arch.as_str()
                ))
            },
            |path| super::absolute_from(&spec.app_dir, path),
        );
        let parent = output
            .parent()
            .ok_or_else(|| anyhow::anyhow!("package output has no parent: {}", output.display()))?
            .to_path_buf();
        Ok(Self {
            apk: output.with_extension("apk"),
            bundle: output.with_extension("aab"),
            parent,
            output,
        })
    }
}

/// Sign a `.tapp` at `output` whose payload is the APK, under an android
/// `[target]`, for the registry.
fn pack_tapp(
    spec: &super::AppPackageSpec,
    build: &AndroidBuild,
    arch: PackageArch,
    request: &PackageRequest,
    parent: &Path,
    output: &Path,
) -> Result<(Vec<PathBuf>, crate::size::SizeReport)> {
    let staging = tempfile::Builder::new()
        .prefix(".tilo-app-package-")
        .tempdir_in(parent)
        .context("creating the package staging area")?;
    let payload = staging.path().join("payload");
    let module = PathBuf::from(format!("bin/{}.apk", spec.cargo_package));
    std::fs::create_dir_all(payload.join("bin"))?;
    std::fs::copy(&build.apk, payload.join(&module))?;
    let manifest_source = std::fs::read_to_string(&spec.manifest_path)?;
    let authored = tilo_app_package::Manifest::from_toml_str(&manifest_source)
        .with_context(|| format!("parsing {}", spec.manifest_path.display()))?;
    let manifest = tilo_app_package::desktop::desktop_manifest(
        &authored,
        PackagePlatform::Android.registry_name(),
        arch.as_str(),
        arch.as_str(),
        &module.to_string_lossy(),
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "{}: {error}; an android build declares the toolkit release it links as [package].runtime",
            spec.manifest_path.display()
        )
    })?;
    let identity = super::signing_identity(&spec.app_dir, request.identity.as_deref())?;
    let staged = staging.path().join("package.tapp");
    let packed = crate::pack::pack_manifest(
        manifest,
        crate::resource_kinds::declared_in(&spec.app_dir)?,
        &payload,
        &staged,
        &identity,
    )?;
    let entrypoints = vec![module];
    let size = super::within_budget(
        &payload,
        &entrypoints,
        packed.bytes,
        (crate::size::Budget::default(), PackagePlatform::Android),
        output,
    )?;
    std::fs::rename(&staged, output)
        .with_context(|| format!("publishing package to {}", output.display()))?;
    eprintln!(
        "[tilo-app] packaged {} ({}, {} bytes)",
        output.display(),
        packed.id,
        packed.bytes
    );
    Ok((entrypoints, size))
}

/// Android's own options to `tilo-app package`.
#[derive(Debug, Clone, Default)]
pub struct PackageOptions {
    /// The release keystore; see [`Signing::Release`].
    pub keystore: Option<PathBuf>,
    /// `Some(true)` requires the `.aab`, `Some(false)` skips it, `None`
    /// builds it when bundletool is there.
    pub bundle: Option<bool>,
}

/// One device or emulator `adb` can reach.
#[derive(Debug, Clone)]
pub struct Adb {
    program: PathBuf,
    pub serial: String,
}

/// The serials `adb devices` lists as ready, and those it lists in another
/// state (`unauthorized`, `offline`) with that state.
#[must_use]
pub fn parse_devices(output: &str) -> (Vec<String>, Vec<(String, String)>) {
    let mut ready = Vec::new();
    let mut other = Vec::new();
    for line in output
        .lines()
        .skip_while(|line| !line.starts_with("List of devices"))
        .skip(1)
    {
        let mut words = line.split_whitespace();
        let (Some(serial), Some(state)) = (words.next(), words.next()) else {
            continue;
        };
        if state == "device" {
            ready.push(serial.to_owned());
        } else {
            other.push((serial.to_owned(), state.to_owned()));
        }
    }
    (ready, other)
}

impl Adb {
    /// The device `requested` names, else `$ANDROID_SERIAL`'s, else the one
    /// device connected.
    ///
    /// # Errors
    ///
    /// When `adb` cannot run, or no single device is ready.
    pub fn connect(tools: &Toolchain, requested: Option<&str>) -> Result<Self> {
        let program = tools.adb();
        let output = Command::new(&program)
            .arg("devices")
            .output()
            .with_context(|| {
                format!(
                    "running {} devices; run `tilo-app setup android`",
                    program.display()
                )
            })?;
        let (ready, other) = parse_devices(&String::from_utf8_lossy(&output.stdout));
        let wanted = requested.map(str::to_owned).or_else(|| {
            std::env::var("ANDROID_SERIAL")
                .ok()
                .filter(|serial| !serial.is_empty())
        });
        let serial = match (wanted, ready.as_slice()) {
            (Some(serial), _) if ready.contains(&serial) => serial,
            (Some(serial), _) => bail!(
                "device {serial} is not connected and ready; `adb devices` lists {}",
                if ready.is_empty() {
                    "none".to_owned()
                } else {
                    ready.join(", ")
                }
            ),
            (None, [one]) => one.clone(),
            (None, []) => {
                let states: Vec<String> = other
                    .iter()
                    .map(|(serial, state)| format!("{serial} ({state})"))
                    .collect();
                bail!(
                    "no Android device is ready{}; connect one with USB debugging on, or start an \
                     emulator (`emulator -avd NAME`, `avdmanager list avd`)",
                    if states.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", states.join(", "))
                    }
                )
            }
            (None, many) => bail!(
                "{} devices are connected ({}); pick one with --device or $ANDROID_SERIAL",
                many.len(),
                many.join(", ")
            ),
        };
        Ok(Self { program, serial })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.arg("-s").arg(&self.serial);
        command
    }

    fn shell(&self, args: &[&str]) -> Result<String> {
        let output = self
            .command()
            .arg("shell")
            .args(args)
            .stderr(Stdio::inherit())
            .output()
            .context("running adb shell")?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// The device's primary architecture.
    ///
    /// # Errors
    ///
    /// When the device reports an ABI Tilo does not build for.
    pub fn arch(&self) -> Result<PackageArch> {
        let abi = self.shell(&["getprop", "ro.product.cpu.abi"])?;
        PackageArch::of_android_abi(&abi).ok_or_else(|| {
            anyhow::anyhow!(
                "{} is {abi:?}; Tilo apps build for arm64-v8a and x86_64",
                self.serial
            )
        })
    }

    /// Install `apk`, replacing an earlier build of the app.
    ///
    /// # Errors
    ///
    /// When the install fails; a build signed with another key names the
    /// uninstall that makes room for it.
    pub fn install(&self, apk: &Path, application_id: &str) -> Result<()> {
        let output = self
            .command()
            .args(["install", "-r", "-d"])
            .arg(apk)
            .output()
            .context("running adb install")?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if output.status.success() && !text.contains("Failure") {
            return Ok(());
        }
        if text.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
            bail!(
                "{} already has {application_id} signed with another key; remove it with \
                 `adb -s {} uninstall {application_id}` (this deletes its data) and run again",
                self.serial,
                self.serial
            );
        }
        bail!("adb install failed:\n{}", text.trim())
    }

    /// Start the app's activity, stopping a running copy first.
    ///
    /// # Errors
    ///
    /// When the activity manager refuses.
    pub fn launch(&self, application_id: &str) -> Result<()> {
        self.stop(application_id);
        let component = format!("{application_id}/{ACTIVITY}");
        let output = self.shell(&["am", "start", "-W", "-n", &component])?;
        if output.contains("Error") {
            bail!("starting {component}: {output}");
        }
        Ok(())
    }

    fn stop(&self, application_id: &str) {
        let _ = self.shell(&["am", "force-stop", application_id]);
    }

    fn pid(&self, application_id: &str) -> Option<u32> {
        self.shell(&["pidof", application_id])
            .ok()?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    /// Stream the toolkit's log (tag `tilo`) and crashes from now on.
    ///
    /// # Errors
    ///
    /// When `adb logcat` cannot start.
    pub fn logcat(&self) -> Result<Child> {
        self.command()
            .args(["logcat", "-v", "brief", "-T", "1"])
            .args(["tilo:V", "AndroidRuntime:E", "DEBUG:V", "libc:F", "*:S"])
            .spawn()
            .context("starting adb logcat")
    }
}

/// Where `run` and `dev` put the app.
#[derive(Debug, Clone, Default)]
pub struct DeviceRequest {
    /// An `adb` serial; see [`Adb::connect`].
    pub device: Option<String>,
    /// `None` builds for the device's architecture.
    pub arch: Option<PackageArch>,
}

fn build_for_device(start: &Path, adb: &Adb, request: &DeviceRequest) -> Result<AndroidBuild> {
    let arch = match request.arch {
        Some(arch) => arch,
        None => adb.arch()?,
    };
    let build = build(
        start,
        &BuildRequest {
            arch,
            profile: "dev".to_owned(),
            signing: Signing::Debug,
            bundle: false,
            features: Vec::new(),
            no_default_features: false,
        },
    )?;
    eprintln!(
        "[tilo-app] built {} in {:.1} s; installing on {}",
        build.apk.display(),
        build.elapsed.as_secs_f64(),
        adb.serial
    );
    adb.install(&build.apk, &build.metadata.application_id)?;
    adb.launch(&build.metadata.application_id)?;
    eprintln!(
        "[tilo-app] started {} on {}",
        build.metadata.application_id, adb.serial
    );
    Ok(build)
}

/// `tilo-app run --platform android`: build a debug APK for the device,
/// install it, start it and stream its log until it exits.
///
/// # Errors
///
/// When there is no device, or the build, install or launch fails.
pub fn run_on_device(start: &Path, request: &DeviceRequest) -> Result<AndroidBuild> {
    let tools = Toolchain::discover();
    let adb = Adb::connect(&tools, request.device.as_deref())?;
    let mut logcat = adb.logcat()?;
    let result = build_for_device(start, &adb, request);
    let build = match result {
        Ok(build) => build,
        Err(error) => {
            let _ = logcat.kill();
            let _ = logcat.wait();
            return Err(error);
        }
    };
    let id = &build.metadata.application_id;
    // The process can take a moment to appear after `am start` returns.
    let mut seen = false;
    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        match adb.pid(id) {
            Some(_) => seen = true,
            None if seen || started.elapsed() > Duration::from_secs(10) => break,
            None => {}
        }
        if logcat.try_wait().ok().flatten().is_some() {
            break;
        }
    }
    let _ = logcat.kill();
    let _ = logcat.wait();
    eprintln!("[tilo-app] {id} exited");
    Ok(build)
}

/// `tilo-app dev --platform android`: `run`, then rebuild, reinstall and
/// relaunch after every change to the app (and a local SDK) until Ctrl-C.
/// The app restarts; its state is what it stored, as after any restart.
///
/// # Errors
///
/// When there is no device, or the file watcher cannot start.
pub fn dev_on_device(start: &Path, dev: &crate::DevRequest, request: &DeviceRequest) -> Result<()> {
    if !dev.forwarded.is_empty() {
        bail!("an Android app takes no command-line arguments");
    }
    let app_dir = super::resolve_app_dir(start)?;
    let roots = super::dev_watch_roots(&app_dir, dev.watch_ui)?;
    let tools = Toolchain::discover();
    let adb = Adb::connect(&tools, request.device.as_deref())?;
    let _signals = super::DevSignalGuard::install()?;
    let mut logcat = adb.logcat()?;
    eprintln!(
        "[tilo-app] watching {}",
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut sources = super::snapshot_restart_sources(&roots).unwrap_or_default();
    let mut application_id = None;
    let mut rebuild = true;
    while !super::DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
        if rebuild {
            rebuild = false;
            match build_for_device(&app_dir, &adb, request) {
                Ok(build) => application_id = Some(build.metadata.application_id),
                Err(error) => eprintln!("[tilo-app] {error:#}\n[tilo-app] waiting for a change"),
            }
        }
        std::thread::sleep(Duration::from_millis(200));
        let Some(next) = super::snapshot_restart_sources(&roots) else {
            continue;
        };
        if next != sources {
            // Let an editor finish saving before building.
            std::thread::sleep(dev.debounce);
            sources = super::snapshot_restart_sources(&roots).unwrap_or(next);
            eprintln!("[tilo-app] change detected; rebuilding");
            rebuild = true;
        }
    }
    if let Some(id) = application_id {
        adb.stop(&id);
    }
    let _ = logcat.kill();
    let _ = logcat.wait();
    eprintln!("[tilo-app] stopped");
    Ok(())
}

/// `tilo-app check --platform android`: Clippy over the Android library for
/// `arch`, warnings denied.
///
/// # Errors
///
/// When the app has no Android entry, the NDK is missing, or Clippy fails.
pub fn clippy(start: &Path, arch: PackageArch) -> Result<()> {
    let app_dir = super::resolve_app_dir(start)?;
    let package = super::cargo_package_name(&app_dir.join("Cargo.toml"))?;
    let target = library_target(&app_dir, &package)?;
    let triple = PackagePlatform::Android.triple(arch);
    let mut args: Vec<OsString> = vec![
        "clippy".into(),
        "--manifest-path".into(),
        app_dir.join("Cargo.toml").into_os_string(),
        "--package".into(),
        package.into(),
        "--target".into(),
        triple.clone().into(),
    ];
    match target {
        LibraryTarget::Example(example) => {
            args.extend([
                "--example".into(),
                example.into(),
                "--features".into(),
                "android".into(),
            ]);
        }
        LibraryTarget::Lib => args.push("--lib".into()),
    }
    args.extend(["--no-deps", "--", "-D", "warnings"].map(OsString::from));
    let tools = Toolchain::discover();
    run(Command::new(cargo_program())
        .args(&args)
        .current_dir(&app_dir)
        .envs(tools.cargo_env(&triple)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(net: &str, portals: &str) -> tilo_app_package::Manifest {
        tilo_app_package::Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.example.field-notes"
version = "1.2.3"
kind = "app"
runtime = "tilo-runtime/2026.07"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "{net}"

[sandbox.portals]
{portals}

[entrypoint]
bin = "bin/field-notes"
type = "gui"
"#
        ))
        .expect("manifest")
    }

    #[test]
    fn metadata_comes_from_the_manifest() {
        let metadata =
            AndroidMetadata::from_manifest("field-notes", &manifest("off", "")).expect("metadata");
        assert_eq!(metadata.application_id, "so.example.field_notes");
        assert_eq!(metadata.label, "Field Notes");
        assert_eq!(metadata.version_name, "1.2.3");
        assert_eq!(metadata.version_code, 1_002_003);
        assert_eq!(
            metadata.permissions,
            ["android.permission.POST_NOTIFICATIONS"]
        );
    }

    #[test]
    fn sandbox_grants_map_to_permissions() {
        let granted = permissions(&manifest(
            "host",
            "location = \"via-host\"\ncamera = \"deny\"\nfile-chooser = \"via-host\"",
        ));
        assert_eq!(
            granted,
            [
                "android.permission.ACCESS_COARSE_LOCATION",
                "android.permission.ACCESS_FINE_LOCATION",
                "android.permission.ACCESS_NETWORK_STATE",
                "android.permission.INTERNET",
                "android.permission.POST_NOTIFICATIONS",
            ]
        );
    }

    #[test]
    fn version_codes_sort_with_versions_and_refuse_overflow() {
        assert_eq!(version_code("0.1.0").unwrap(), 1_000);
        assert_eq!(version_code("2.0.0-beta.1+build").unwrap(), 2_000_000);
        assert!(version_code("1.2.999").unwrap() < version_code("1.3.0").unwrap());
        assert!(version_code("1.1000.0").is_err());
        assert!(version_code("2100.0.0").is_err());
        assert!(version_code("1.2").is_err());
    }

    #[test]
    fn application_ids_follow_android_rules() {
        assert_eq!(
            application_id("so.tilo.api-tester").unwrap(),
            "so.tilo.api_tester"
        );
        assert!(application_id("calculator").is_err());
        assert!(application_id("so.tilo.3d").is_err());
        assert!(application_id("so..tilo").is_err());
    }

    #[test]
    fn the_manifest_names_the_library_and_escapes_text() {
        let mut metadata =
            AndroidMetadata::from_manifest("field-notes", &manifest("off", "")).expect("metadata");
        metadata.label = "Notes & <Things>".to_owned();
        let xml = android_manifest(&metadata, "field_notes", false);
        assert!(xml.contains(r#"package="so.example.field_notes""#));
        assert!(xml.contains(r#"android:versionCode="1002003""#));
        assert!(xml.contains(r#"android:label="Notes &amp; &lt;Things&gt;""#));
        assert!(xml.contains(r#"android:value="field_notes""#));
        assert!(xml.contains(r#"android:debuggable="false""#));
        assert!(xml.contains(r#"android:authorities="so.example.field_notes.tilo.files""#));
        assert!(xml.contains("android.permission.POST_NOTIFICATIONS"));
        assert!(!xml.contains("android.permission.INTERNET"));
    }

    #[test]
    fn the_library_is_the_android_example_or_the_entry_library() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path();
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(app.join("Cargo.toml"), "[package]\nname = \"calc\"\n").unwrap();
        std::fs::write(app.join("src/lib.rs"), "pub struct App;\n").unwrap();
        assert!(library_target(app, "calc").is_err());
        std::fs::write(app.join("src/lib.rs"), "ui::dev_reload_entry!(App);\n").unwrap();
        assert_eq!(library_target(app, "calc").unwrap(), LibraryTarget::Lib);
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"calc\"\n\n[[example]]\nname = \"calc-android\"\npath = \"src/android.rs\"\ncrate-type = [\"cdylib\"]\n",
        )
        .unwrap();
        assert_eq!(
            library_target(app, "calc").unwrap(),
            LibraryTarget::Example("calc-android".to_owned())
        );
    }

    #[test]
    fn the_library_command_builds_a_cdylib_for_the_triple() {
        let manifest = Path::new("/app/Cargo.toml");
        let lib = library_cargo_args(
            manifest,
            &LibraryTarget::Lib,
            "aarch64-linux-android",
            "dev",
            &[],
            false,
        );
        let lib: Vec<_> = lib
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&lib[..4], ["rustc", "--lib", "--crate-type", "cdylib"]);
        assert!(
            lib.windows(2)
                .any(|pair| pair == ["--target", "aarch64-linux-android"])
        );
        assert!(lib.windows(2).any(|pair| pair == ["--profile", "dev"]));
        assert!(!lib.iter().any(|arg| arg == "--features"));

        let example = library_cargo_args(
            manifest,
            &LibraryTarget::Example("calc-android".to_owned()),
            "x86_64-linux-android",
            "release",
            &["extra".to_owned()],
            true,
        );
        let example: Vec<_> = example
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&example[..3], ["build", "--example", "calc-android"]);
        assert!(
            example
                .windows(2)
                .any(|pair| pair == ["--features", "extra,android"])
        );
        assert!(example.iter().any(|arg| arg == "--no-default-features"));
    }

    #[test]
    fn the_artifact_stream_names_the_shared_library() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("Cargo.toml");
        std::fs::write(&manifest, "").unwrap();
        let manifest = manifest.canonicalize().unwrap();
        let line = serde_json::json!({
            "reason": "compiler-artifact",
            "manifest_path": manifest,
            "target": {"name": "calc", "kind": ["lib"]},
            "filenames": ["/t/libcalc.rlib", "/t/libcalc.so"],
        })
        .to_string();
        assert_eq!(
            shared_library_artifact(&line, &manifest),
            Some(PathBuf::from("/t/libcalc.so"))
        );
        assert_eq!(
            shared_library_artifact(&line, Path::new("/elsewhere/Cargo.toml")),
            None
        );
    }

    #[test]
    fn devices_are_read_from_adb() {
        let (ready, other) = parse_devices(
            "* daemon started successfully\nList of devices attached\nemulator-5554\tdevice\nR58M\tunauthorized\n\n",
        );
        assert_eq!(ready, ["emulator-5554"]);
        assert_eq!(other, [("R58M".to_owned(), "unauthorized".to_owned())]);
        assert_eq!(parse_devices("List of devices attached\n\n").0.len(), 0);
    }

    #[test]
    fn abis_and_triples_agree() {
        for arch in PackageArch::ALL {
            assert_eq!(PackageArch::of_android_abi(arch.android_abi()), Some(arch));
            assert!(
                PackagePlatform::Android
                    .triple(arch)
                    .ends_with("-linux-android")
            );
        }
        assert_eq!(PackageArch::of_android_abi("armeabi-v7a"), None);
    }

    #[test]
    fn the_default_icon_is_a_disc_in_the_ids_colour() {
        let icon = default_icon("so.example.notes");
        assert_eq!(icon.dimensions(), (192, 192));
        assert_eq!(icon.get_pixel(0, 0)[3], 0);
        assert_eq!(icon.get_pixel(96, 96)[3], 255);
        assert_eq!(icon, default_icon("so.example.notes"));
        let colours: BTreeSet<[u8; 4]> = ["so.a.notes", "so.a.books", "so.a.music", "so.a.maps"]
            .iter()
            .map(|id| default_icon(id).get_pixel(96, 96).0)
            .collect();
        assert!(colours.len() > 1, "ids share one colour: {colours:?}");
    }

    #[test]
    fn os_release_names_the_package_manager_family() {
        let family = PackageManager::from_os_release;
        assert_eq!(
            family("NAME=\"Fedora Linux\"\nID=fedora\n"),
            Some(PackageManager::Dnf)
        );
        assert_eq!(
            family("ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n"),
            Some(PackageManager::Dnf)
        );
        assert_eq!(
            family("ID=ubuntu\nID_LIKE=debian\n"),
            Some(PackageManager::Apt)
        );
        assert_eq!(
            family("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"),
            Some(PackageManager::Apt)
        );
        assert_eq!(family("ID=arch\n"), None);
        assert_eq!(family(""), None);
    }

    #[test]
    fn missing_system_tools_get_one_exact_install_command() {
        let all = [SystemTool::Jdk, SystemTool::Zip, SystemTool::Unzip];
        assert_eq!(
            PackageManager::Dnf.install_command(&all),
            "sudo dnf install -y java-21-openjdk-devel zip unzip"
        );
        assert_eq!(
            PackageManager::Apt.install_command(&all),
            "sudo apt-get install -y openjdk-17-jdk-headless zip unzip"
        );
        assert_eq!(
            PackageManager::Brew.install_command(&all),
            "brew install openjdk@21 zip unzip"
        );
        assert_eq!(
            system_install_hint(Some(PackageManager::Apt), &[SystemTool::Zip]),
            "`sudo apt-get install -y zip`"
        );

        // An unknown system is told each family's command.
        let unknown = system_install_hint(None, &[SystemTool::Jdk]);
        for command in [
            "`sudo dnf install -y java-21-openjdk-devel`",
            "`sudo apt-get install -y openjdk-17-jdk-headless`",
            "`brew install openjdk@21`",
        ] {
            assert!(unknown.contains(command), "{unknown}");
        }
        assert!(unknown.contains("$JAVA_HOME"), "{unknown}");
    }

    #[test]
    fn the_doctor_report_ends_with_the_system_install_command() {
        let check = |name: &'static str, found: bool| ToolCheck {
            name,
            found: found.then(|| format!("/usr/bin/{name}")),
            fix: String::new(),
            required: true,
        };
        let report = DoctorReport {
            checks: vec![
                check("javac", false),
                check("zip", true),
                check("unzip", false),
            ],
            system_missing: vec![SystemTool::Jdk, SystemTool::Unzip],
            package_manager: Some(PackageManager::Dnf),
        };
        assert!(!report.is_ready());
        assert!(
            report.summary().contains(
                "Install the system packages with `sudo dnf install -y java-21-openjdk-devel unzip`"
            ),
            "{}",
            report.summary()
        );
        assert_eq!(
            report.to_json()["system_install"].as_str(),
            report.system_install().as_deref()
        );
        let ready = DoctorReport {
            checks: vec![check("zip", true)],
            system_missing: Vec::new(),
            package_manager: None,
        };
        assert!(ready.system_install().is_none());
        assert!(!ready.summary().contains("Install the system packages"));
    }
}
