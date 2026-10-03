//! `tilo-app add ci`: a workflow for the forge an app is hosted on that
//! checks and packages every push and publishes tagged releases.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The builder the Tilo OS jobs run in: the toolchain, system libraries and
/// target flags Tilo itself is built with.
pub const BUILDER_IMAGE: &str = "registry.tilo.so/tilo/tilo-builder:latest";

const SDK_GIT: &str = "https://code.tilo.so/tilo/tilo.git";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forge {
    Github,
    Forgejo,
    Gitea,
}

impl Forge {
    fn name(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Forgejo => "forgejo",
            Self::Gitea => "gitea",
        }
    }

    fn workflow_path(self) -> &'static str {
        match self {
            Self::Github => ".github/workflows/tilo.yml",
            Self::Forgejo => ".forgejo/workflows/tilo.yml",
            Self::Gitea => ".gitea/workflows/tilo.yml",
        }
    }

    fn default_runner(self) -> &'static str {
        match self {
            Self::Github | Self::Gitea => "ubuntu-latest",
            Self::Forgejo => "docker",
        }
    }

    fn artifact_actions(self) -> (&'static str, &'static str) {
        match self {
            Self::Github => ("actions/upload-artifact@v4", "actions/download-artifact@v4"),
            Self::Forgejo | Self::Gitea => (
                "https://code.forgejo.org/forgejo/upload-artifact@v4",
                "https://code.forgejo.org/forgejo/download-artifact@v4",
            ),
        }
    }
}

struct Build {
    platform: &'static str,
    arch: &'static str,
    runner: &'static str,
    builder: bool,
}

const GITHUB_BUILDS: &[Build] = &[
    Build {
        platform: "tilo",
        arch: "x86_64",
        runner: "ubuntu-latest",
        builder: true,
    },
    Build {
        platform: "tilo",
        arch: "aarch64",
        runner: "ubuntu-24.04-arm",
        builder: true,
    },
    // Built outside the builder, whose Fedora glibc is newer than most
    // desktops': the oldest supported Ubuntu sets the floor instead.
    Build {
        platform: "linux",
        arch: "x86_64",
        runner: "ubuntu-22.04",
        builder: false,
    },
    Build {
        platform: "linux",
        arch: "aarch64",
        runner: "ubuntu-22.04-arm",
        builder: false,
    },
    Build {
        platform: "windows",
        arch: "x86_64",
        runner: "windows-latest",
        builder: false,
    },
    Build {
        platform: "windows",
        arch: "aarch64",
        runner: "windows-11-arm",
        builder: false,
    },
    Build {
        platform: "macos",
        arch: "aarch64",
        runner: "macos-latest",
        builder: false,
    },
    Build {
        platform: "macos",
        arch: "x86_64",
        runner: "macos-15-intel",
        builder: false,
    },
    // The web side module builds on Linux in the builder, which carries the
    // web toolchain (wasm-bindgen, wasm-opt, wasm-ld, Node.js, brotli).
    Build {
        platform: "web",
        arch: "wasm32",
        runner: "ubuntu-latest",
        builder: true,
    },
    // Cross-compiled on Linux x86_64 in the builder, which carries the NDK
    // (the NDK ships x86_64 host tools only). The Android SDK and a JDK are
    // not in the image: the job installs them with `tilo-app setup android`
    // and caches the SDK, so the image stays the size Tilo's own CI needs.
    Build {
        platform: "android",
        arch: "aarch64",
        runner: "ubuntu-latest",
        builder: true,
    },
];

/// Closes a Forgejo or Gitea matrix, which lists only the builder's builds.
const OTHER_RUNNERS_NOTE: &str = "          \
    # Windows and macOS need runners on those systems, and Linux\n          \
    # for other desktops a runner with Rust and an older glibc than\n          \
    # the builder's; register them and add entries here as above,\n          \
    # with container: \"\".\n";

impl Build {
    /// Built on the forge's Linux `x86_64` runner, the one every forge has.
    fn on_default_runner(&self) -> bool {
        self.builder && self.runner == "ubuntu-latest"
    }
}

/// What `tilo-app add ci` wrote, or would write.
#[derive(Debug)]
pub struct CiOutcome {
    pub path: PathBuf,
    pub forge: Forge,
    pub working_directory: String,
    pub written: bool,
}

impl CiOutcome {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "artifact": "ci",
            "forge": self.forge.name(),
            "path": self.path,
            "working_directory": self.working_directory,
            "written": self.written,
        })
    }
}

/// Write the workflow for `forge` at the root of the repository holding
/// the app at or above `start`. `runner` replaces the forge's default Linux runner label.
///
/// # Errors
///
/// When the workflow already exists without `force`, or cannot be written.
pub fn add_ci(
    start: &Path,
    forge: Forge,
    runner: Option<&str>,
    replace: bool,
    dry_run: bool,
) -> Result<CiOutcome> {
    let app_dir = &crate::resolve_app_dir(start)?;
    let root = repository_root(app_dir);
    let working_directory = relative_slash(&root, app_dir);
    let lock = lock_path(&root, app_dir);
    let path = root.join(forge.workflow_path());
    if path.exists() && !replace {
        bail!(
            "{} already exists; pass --force to replace it",
            path.display()
        );
    }
    let workflow = workflow(
        forge,
        runner.unwrap_or_else(|| forge.default_runner()),
        &working_directory,
        &lock,
    );
    if !dry_run {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&path, workflow).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(CiOutcome {
        path,
        forge,
        working_directory,
        written: !dry_run,
    })
}

fn repository_root(app_dir: &Path) -> PathBuf {
    app_dir
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(app_dir)
        .to_path_buf()
}

fn lock_path(root: &Path, app_dir: &Path) -> String {
    app_dir
        .ancestors()
        .take_while(|dir| dir.starts_with(root))
        .find(|dir| dir.join("Cargo.lock").exists())
        .map_or_else(
            || join_slash(&relative_slash(root, app_dir), "Cargo.lock"),
            |dir| join_slash(&relative_slash(root, dir), "Cargo.lock"),
        )
}

fn relative_slash(root: &Path, dir: &Path) -> String {
    let relative = dir.strip_prefix(root).unwrap_or_else(|_| Path::new(""));
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

fn join_slash(dir: &str, file: &str) -> String {
    if dir == "." {
        file.to_owned()
    } else {
        format!("{dir}/{file}")
    }
}

/// The workflow text. Lines use `${{ }}` expressions, so it is assembled
/// with `push_str` rather than `format!`.
fn workflow(forge: Forge, runner: &str, working_directory: &str, lock: &str) -> String {
    let (upload, download) = forge.artifact_actions();
    let dist = join_slash(working_directory, "dist");
    let mut out = String::new();
    let _ = write!(
        out,
        "# Written by `tilo-app add ci --forge {forge}`; edit freely.\n\
         #\n\
         # Every push and pull request is checked and packaged for each platform.\n\
         # Pushing a tag named v* also publishes the packages to the registry,\n\
         # which lists them once its owner has reviewed the request.\n\
         #\n\
         # Publishing needs the TILO_PUBLISHER_KEY secret: the contents of the\n\
         # file `tilo-app keys new` wrote. Packages built without it (pull\n\
         # requests from forks) are signed with a throwaway key. Set the\n\
         # TILO_REGISTRY variable to publish somewhere other than apps.tilo.so.\n\
         \n\
         name: Tilo app\n\
         \n\
         on:\n  \
           push:\n    \
             branches: [main]\n    \
             tags: [\"v*\"]\n  \
           pull_request:\n  \
           workflow_dispatch:\n\
         \n\
         defaults:\n  \
           run:\n    \
             shell: bash\n    \
             working-directory: {working_directory}\n\
         \n\
         jobs:\n",
        forge = forge.name(),
    );

    out.push_str("  check:\n");
    let _ = writeln!(out, "    runs-on: {runner}");
    let _ = writeln!(out, "    container: {BUILDER_IMAGE}");
    out.push_str("    steps:\n");
    steps_prelude(&mut out, lock);
    out.push_str("      - run: tilo-app check\n\n");

    out.push_str("  package:\n    needs: check\n");
    out.push_str("    strategy:\n      fail-fast: false\n      matrix:\n        include:\n");
    match forge {
        Forge::Github => {
            for build in GITHUB_BUILDS {
                matrix_entry(&mut out, build, "", runner_for(build, runner));
            }
            out.push_str("    runs-on: ${{ matrix.runner }}\n");
            out.push_str("    container: ${{ matrix.container }}\n");
        }
        Forge::Forgejo | Forge::Gitea => {
            for build in GITHUB_BUILDS.iter().filter(|build| build.builder) {
                if build.on_default_runner() {
                    matrix_entry(&mut out, build, "", runner);
                } else {
                    matrix_entry(&mut out, build, "# ", "your-aarch64-runner");
                }
            }
            out.push_str(OTHER_RUNNERS_NOTE);
            out.push_str("    runs-on: ${{ matrix.runner }}\n");
            let _ = writeln!(out, "    container: {BUILDER_IMAGE}");
        }
    }
    out.push_str("    steps:\n");
    steps_prelude(&mut out, lock);
    android_steps(&mut out);
    out.push_str(
        "      - name: Package\n        \
           env:\n          \
             TILO_PUBLISHER_KEY: ${{ secrets.TILO_PUBLISHER_KEY }}\n          \
             TILO_ANDROID_KEYSTORE_PASS: ${{ secrets.TILO_ANDROID_KEYSTORE_PASS }}\n        \
           run: >-\n          \
             tilo-app package --platform ${{ matrix.platform }} --arch ${{ matrix.arch }}\n          \
             --output dist/${{ matrix.platform }}-${{ matrix.arch }}.tapp\n",
    );
    upload_steps(&mut out, upload, &dist);

    out.push_str("  publish:\n    needs: package\n    if: startsWith(github.ref, 'refs/tags/v')\n");
    let _ = writeln!(out, "    runs-on: {runner}");
    let _ = writeln!(out, "    container: {BUILDER_IMAGE}");
    out.push_str("    steps:\n");
    steps_prelude(&mut out, lock);
    let _ = write!(
        out,
        "      - uses: {download}\n        \
           with:\n          \
             pattern: tapp-*\n          \
             merge-multiple: true\n          \
             path: {dist}\n\
         \x20     - name: Publish\n        \
           env:\n          \
             TILO_PUBLISHER_KEY: ${{{{ secrets.TILO_PUBLISHER_KEY }}}}\n          \
             TILO_REGISTRY: ${{{{ vars.TILO_REGISTRY }}}}\n        \
           run: tilo-app publish dist/*.tapp\n"
    );
    out
}

/// The package job's artifacts: every build's `.tapp`, and Android's `.apk`
/// and `.aab` for sideloading and Play.
fn upload_steps(out: &mut String, upload: &str, dist: &str) {
    let _ = write!(
        out,
        "      - uses: {upload}\n        \
           with:\n          \
             name: tapp-${{{{ matrix.platform }}}}-${{{{ matrix.arch }}}}\n          \
             path: {dist}/${{{{ matrix.platform }}}}-${{{{ matrix.arch }}}}.tapp\n          \
             if-no-files-found: error\n\
         \x20     - if: matrix.platform == 'android'\n        \
           uses: {upload}\n        \
           with:\n          \
             name: android-${{{{ matrix.arch }}}}\n          \
             path: |\n            \
               {dist}/android-${{{{ matrix.arch }}}}.apk\n            \
               {dist}/android-${{{{ matrix.arch }}}}.aab\n          \
             if-no-files-found: error\n\n"
    );
}

/// The Android SDK, build-tools and a JDK for the builder, which carries
/// only the NDK, and the release keystore from the `TILO_ANDROID_KEYSTORE`
/// secret (base64). Without that secret the release is signed with a key
/// kept beside the publisher key, which differs from run to run.
fn android_steps(out: &mut String) {
    out.push_str(
        "      - name: Install a JDK and zip for Android\n        \
           if: matrix.platform == 'android'\n        \
           run: |\n          \
             dnf install -y --setopt=install_weak_deps=False java-21-openjdk-devel zip unzip\n          \
             echo \"ANDROID_HOME=$GITHUB_WORKSPACE/.android-sdk\" >> \"$GITHUB_ENV\"\n\
         \x20     - if: matrix.platform == 'android'\n        \
           uses: actions/cache@v4\n        \
           with:\n          \
             path: .android-sdk\n          \
             key: android-sdk-${{ runner.os }}-${{ hashFiles('.tilo-cli/bin/tilo-app') }}\n\
         \x20     - name: Install the Android SDK\n        \
           if: matrix.platform == 'android'\n        \
           run: tilo-app setup android --accept-licenses\n\
         \x20     - name: Release keystore\n        \
           if: matrix.platform == 'android'\n        \
           env:\n          \
             KEYSTORE: ${{ secrets.TILO_ANDROID_KEYSTORE }}\n        \
           run: |\n          \
             if [ -n \"$KEYSTORE\" ]; then\n            \
               printf '%s' \"$KEYSTORE\" | base64 -d > \"$RUNNER_TEMP/release.keystore\"\n            \
               echo \"TILO_ANDROID_KEYSTORE=$RUNNER_TEMP/release.keystore\" >> \"$GITHUB_ENV\"\n          \
             fi\n",
    );
}

fn runner_for<'a>(build: &'a Build, linux_x86: &'a str) -> &'a str {
    if build.on_default_runner() {
        linux_x86
    } else {
        build.runner
    }
}

fn matrix_entry(out: &mut String, build: &Build, prefix: &str, runner: &str) {
    let container = if build.builder { BUILDER_IMAGE } else { "" };
    let _ = writeln!(
        out,
        "          {prefix}- {{ platform: {}, arch: {}, runner: {runner}, container: \"{container}\" }}",
        build.platform, build.arch,
    );
}

/// Checkout, then the `tilo-app` built from the SDK revision `Cargo.lock`
/// pins, cached per revision.
fn steps_prelude(out: &mut String, lock: &str) {
    out.push_str("      - uses: actions/checkout@v4\n");
    let _ = write!(
        out,
        "      - name: Find the Tilo SDK revision\n        \
           id: sdk\n        \
           working-directory: .\n        \
           run: |\n          \
             rev=$(sed -n 's|^source = \"git+{SDK_GIT}?rev=\\([0-9a-f]\\{{40\\}}\\)#.*|\\1|p' {lock} | head -n1)\n          \
             echo \"rev=${{rev:-main}}\" >> \"$GITHUB_OUTPUT\"\n\
         \x20     - uses: actions/cache@v4\n        \
           with:\n          \
             path: .tilo-cli\n          \
             key: tilo-app-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-${{{{ steps.sdk.outputs.rev }}}}-${{{{ hashFiles('{lock}') }}}}\n\
         \x20     - name: Install tilo-app\n        \
           run: |\n          \
             root=\"$GITHUB_WORKSPACE/.tilo-cli\"\n          \
             if [ ! -x \"$root/bin/tilo-app\" ] && [ ! -x \"$root/bin/tilo-app.exe\" ]; then\n            \
               rev='${{{{ steps.sdk.outputs.rev }}}}'\n            \
               if [ \"$rev\" = main ]; then pin=(--branch main); else pin=(--rev \"$rev\"); fi\n            \
               cargo install --locked --root \"$root\" --git {SDK_GIT} \"${{pin[@]}}\" tilo-app-cli\n          \
             fi\n          \
             echo \"$root/bin\" >> \"$GITHUB_PATH\"\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_workflow_builds_every_platform_and_publishes_tags_once() {
        let text = workflow(Forge::Github, "ubuntu-latest", "apps/editor", "Cargo.lock");
        let yaml: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid YAML");
        let include = &yaml["jobs"]["package"]["strategy"]["matrix"]["include"];
        let builds: Vec<String> = include
            .as_sequence()
            .expect("matrix")
            .iter()
            .map(|entry| {
                format!(
                    "{}-{}",
                    entry["platform"].as_str().unwrap_or_default(),
                    entry["arch"].as_str().unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            builds,
            [
                "tilo-x86_64",
                "tilo-aarch64",
                "linux-x86_64",
                "linux-aarch64",
                "windows-x86_64",
                "windows-aarch64",
                "macos-aarch64",
                "macos-x86_64",
                "web-wasm32",
                "android-aarch64"
            ]
        );
        assert_eq!(
            yaml["defaults"]["run"]["working-directory"].as_str(),
            Some("apps/editor")
        );
        let publish = &yaml["jobs"]["publish"];
        assert_eq!(
            publish["if"].as_str(),
            Some("startsWith(github.ref, 'refs/tags/v')")
        );
        let steps = publish["steps"].as_sequence().expect("steps");
        let runs: Vec<&str> = steps
            .iter()
            .filter_map(|step| step["run"].as_str())
            .collect();
        assert_eq!(
            runs.iter()
                .filter(|run| run.contains("tilo-app publish"))
                .count(),
            1
        );
        assert!(text.contains("path: apps/editor/dist/${{ matrix.platform }}"));
        assert!(text.contains(r"rev=\([0-9a-f]\{40\}\)#"));
    }

    #[test]
    fn forgejo_workflow_uses_forgejo_artifacts_and_one_runner_label() {
        let text = workflow(Forge::Forgejo, "codeberg-small", ".", "Cargo.lock");
        let yaml: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid YAML");
        let include = yaml["jobs"]["package"]["strategy"]["matrix"]["include"]
            .as_sequence()
            .expect("matrix");
        let builds: Vec<(&str, &str)> = include
            .iter()
            .map(|entry| {
                (
                    entry["platform"].as_str().unwrap_or_default(),
                    entry["runner"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            builds,
            [
                ("tilo", "codeberg-small"),
                ("web", "codeberg-small"),
                ("android", "codeberg-small")
            ],
            "the Linux x86_64 builds are on"
        );
        assert!(text.contains("# - { platform: tilo, arch: aarch64"));
        assert!(text.contains("# Windows and macOS need runners on those systems, and Linux"));
        assert!(text.contains("https://code.forgejo.org/forgejo/upload-artifact@v4"));
        assert!(!text.contains("actions/upload-artifact"));
        assert_eq!(
            yaml["jobs"]["check"]["runs-on"].as_str(),
            Some("codeberg-small")
        );
    }

    #[test]
    fn android_builds_in_the_builder_with_the_sdk_it_installs_and_ships_the_apk() {
        let text = workflow(Forge::Github, "ubuntu-latest", ".", "Cargo.lock");
        let yaml: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid YAML");
        let android = yaml["jobs"]["package"]["strategy"]["matrix"]["include"]
            .as_sequence()
            .expect("matrix")
            .iter()
            .find(|entry| entry["platform"].as_str() == Some("android"))
            .expect("an android build");
        assert_eq!(android["runner"].as_str(), Some("ubuntu-latest"));
        assert_eq!(android["container"].as_str(), Some(BUILDER_IMAGE));
        let steps = yaml["jobs"]["package"]["steps"]
            .as_sequence()
            .expect("steps");
        let android_only: Vec<&serde_yaml::Value> = steps
            .iter()
            .filter(|step| step["if"].as_str() == Some("matrix.platform == 'android'"))
            .collect();
        let runs: Vec<&str> = android_only
            .iter()
            .filter_map(|step| step["run"].as_str())
            .collect();
        assert!(runs.iter().any(|run| run.contains("java-21-openjdk-devel")));
        assert!(runs.contains(&"tilo-app setup android --accept-licenses"));
        assert!(
            runs.iter()
                .any(|run| run.contains("TILO_ANDROID_KEYSTORE="))
        );
        let setup = steps
            .iter()
            .position(|step| {
                step["run"].as_str() == Some("tilo-app setup android --accept-licenses")
            })
            .expect("setup step");
        let package = steps
            .iter()
            .position(|step| step["name"].as_str() == Some("Package"))
            .expect("package step");
        assert!(setup < package, "the SDK is installed before packaging");
        let uploads: Vec<&str> = android_only
            .iter()
            .filter_map(|step| step["with"]["path"].as_str())
            .collect();
        assert!(
            uploads
                .iter()
                .any(|path| path.contains("dist/android-${{ matrix.arch }}.apk")
                    && path.contains("dist/android-${{ matrix.arch }}.aab")),
            "{uploads:?}"
        );
    }

    #[test]
    fn workflow_goes_at_the_repository_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app = temp.path().join("apps/editor");
        std::fs::create_dir_all(temp.path().join(".git")).expect("git");
        std::fs::create_dir_all(&app).expect("app");
        std::fs::write(temp.path().join("Cargo.lock"), "").expect("lock");
        let outcome = add_ci(&app, Forge::Gitea, None, false, false).expect("add ci");
        assert_eq!(outcome.path, temp.path().join(".gitea/workflows/tilo.yml"));
        assert_eq!(outcome.working_directory, "apps/editor");
        let text = std::fs::read_to_string(&outcome.path).expect("written");
        assert!(text.contains("hashFiles('Cargo.lock')"));
        assert!(add_ci(&app, Forge::Gitea, None, false, false).is_err());
        assert!(add_ci(&app, Forge::Gitea, None, true, false).is_ok());
    }
}
