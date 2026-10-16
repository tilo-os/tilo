//! `tilo-app add ci`: a workflow for the forge an app is hosted on that
//! checks and packages every push and publishes tagged releases.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The builder the Tilo OS jobs run in: the toolchain, system libraries and
/// target flags Tilo itself is built with.
pub const BUILDER_IMAGE: &str = "registry.tilo.so/tilo/tilo-builder:latest";

const SDK_GIT: &str = "https://code.tilo.so/tilo/tilo.git";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
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
    pub reusable_path: Option<PathBuf>,
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
            "reusable_path": self.reusable_path,
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
    let reusable_path =
        (forge == Forge::Github).then(|| root.join(".github/workflows/tilo-build.yml"));
    for candidate in std::iter::once(&path).chain(reusable_path.iter()) {
        if candidate.exists() && !replace {
            bail!(
                "{} already exists; pass --force to replace it",
                candidate.display()
            );
        }
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
        if let Some(reusable) = &reusable_path {
            std::fs::write(reusable, github_reusable(runner.unwrap_or("ubuntu-latest")))
                .with_context(|| format!("write {}", reusable.display()))?;
            std::fs::write(&path, github_caller(&working_directory, &lock))
                .with_context(|| format!("write {}", path.display()))?;
        } else {
            std::fs::write(&path, workflow).with_context(|| format!("write {}", path.display()))?;
        }
    }
    Ok(CiOutcome {
        path,
        reusable_path,
        forge,
        working_directory,
        written: !dry_run,
    })
}

fn github_caller(working_directory: &str, lock: &str) -> String {
    format!(
        "# Written by `tilo-app add ci --forge github`; edit freely.\n\
         name: Tilo app\n\
         on:\n  push:\n    branches: [main]\n    tags: [\"v*\"]\n  pull_request:\n  workflow_dispatch:\n\
         permissions:\n  contents: read\n\
         jobs:\n  app:\n    uses: ./.github/workflows/tilo-build.yml\n    with:\n\
         \x20     working-directory: {directory}\n      lockfile: {lock}\n\
         \x20     publish: ${{{{ startsWith(github.ref, 'refs/tags/v') }}}}\n\
         \x20   secrets:\n      TILO_PUBLISHER_KEY: ${{{{ secrets.TILO_PUBLISHER_KEY }}}}\n\
         \x20     TILO_ANDROID_KEYSTORE: ${{{{ secrets.TILO_ANDROID_KEYSTORE }}}}\n\
         \x20     TILO_ANDROID_KEYSTORE_PASS: ${{{{ secrets.TILO_ANDROID_KEYSTORE_PASS }}}}\n",
        directory = serde_json::Value::String(working_directory.to_owned()),
        lock = serde_json::Value::String(lock.to_owned()),
    )
}

/// The same build matrix, callable from any repository. GitHub checks out
/// the caller's source, and installs the CLI from that source's SDK pin.
fn github_reusable(runner: &str) -> String {
    let mut out = format!(
        "# Written by `tilo-app add ci --forge github`; edit freely.\n\
         name: Build a Tilo app\n\
         on:\n  workflow_call:\n    inputs:\n\
         \x20     working-directory:\n        type: string\n        default: .\n\
         \x20     lockfile:\n        type: string\n        default: Cargo.lock\n\
         \x20     linux-runner:\n        type: string\n        default: {runner}\n\
         \x20     registry:\n        type: string\n        default: ''\n\
         \x20     publish:\n        type: boolean\n        default: false\n\
         \x20   secrets:\n      TILO_PUBLISHER_KEY:\n        required: false\n\
         \x20     TILO_ANDROID_KEYSTORE:\n        required: false\n\
         \x20     TILO_ANDROID_KEYSTORE_PASS:\n        required: false\n\
         permissions:\n  contents: read\n\
         defaults:\n  run:\n    shell: bash\n\
         jobs:\n",
        runner = serde_json::Value::String(runner.to_owned()),
    );
    let body = workflow_jobs(
        Forge::Github,
        "${{ inputs.linux-runner }}",
        "${{ inputs.working-directory }}",
        "${{ inputs.lockfile }}",
    );
    out.push_str(
        &body
            .replace(
                "if: startsWith(github.ref, 'refs/tags/v')",
                "if: inputs.publish && github.event_name != 'pull_request'",
            )
            .replace(
                "${{ vars.TILO_REGISTRY }}",
                "${{ inputs.registry || vars.TILO_REGISTRY }}",
            ),
    );
    for secret in [
        "TILO_PUBLISHER_KEY",
        "TILO_ANDROID_KEYSTORE",
        "TILO_ANDROID_KEYSTORE_PASS",
    ] {
        out = out.replace(
            &format!("${{{{ secrets.{secret} }}}}"),
            &format!("${{{{ inputs.publish && github.event_name != 'pull_request' && secrets.{secret} || '' }}}}"),
        );
    }
    out
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
    let mut out = String::new();
    let _ = write!(
        out,
        "# Written by `tilo-app add ci --forge {forge}`; edit freely.\n\
         #\n\
         # Every push and pull request is checked and packaged for each platform.\n\
         # Pushing a tag named v* also publishes the packages to the registry,\n\
         # which lists them once its owner has reviewed the request.\n\
         #\n\
         # Publishing needs the TILO_PUBLISHER_KEY secret: what\n\
         # `tilo-app keys export` prints. Packages built without it (pull\n\
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

    out.push_str(&workflow_jobs(forge, runner, working_directory, lock));
    out
}

fn workflow_jobs(forge: Forge, runner: &str, working_directory: &str, lock: &str) -> String {
    let (upload, download) = forge.artifact_actions();
    let dist = join_slash(working_directory, "dist");
    let mut out = String::new();
    out.push_str("  check:\n");
    let _ = writeln!(out, "    runs-on: {runner}");
    let _ = writeln!(out, "    container: {BUILDER_IMAGE}");
    job_directory(&mut out, working_directory);
    out.push_str("    steps:\n");
    steps_prelude(&mut out, working_directory, lock);
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
    job_directory(&mut out, working_directory);
    out.push_str("    steps:\n");
    steps_prelude(&mut out, working_directory, lock);
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
    job_directory(&mut out, working_directory);
    out.push_str("    steps:\n");
    steps_prelude(&mut out, working_directory, lock);
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

fn job_directory(out: &mut String, directory: &str) {
    let _ = writeln!(
        out,
        "    defaults:\n      run:\n        working-directory: {directory}"
    );
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
    let runner = serde_json::Value::String(runner.to_owned());
    let _ = writeln!(
        out,
        "          {prefix}- {{ platform: {}, arch: {}, runner: {runner}, container: \"{container}\" }}",
        build.platform, build.arch,
    );
}

/// Checkout, then the `tilo-app` built from the SDK revision `Cargo.lock`
/// pins, cached per revision. A lock no build has resolved yet (committed
/// straight after `tilo-app new`) names no SDK source, so the revision the
/// app's manifest pins comes next, and `main` only when neither names one.
fn steps_prelude(out: &mut String, working_directory: &str, lock: &str) {
    let lock_expression = if lock == "${{ inputs.lockfile }}" {
        "inputs.lockfile".to_owned()
    } else {
        format!("'{lock}'")
    };
    let manifest = join_slash(working_directory, "Cargo.toml");
    out.push_str("      - uses: actions/checkout@v4\n");
    let _ = write!(
        out,
        "      - name: Find the Tilo SDK revision\n        \
           id: sdk\n        \
           working-directory: .\n        \
           env:\n          \
             LOCK_FILE: {lock}\n          \
             APP_MANIFEST: {manifest}\n        \
           run: |\n          \
             rev=$(sed -n 's|^source = \"git+{SDK_GIT}?rev=\\([0-9a-f]\\{{40\\}}\\)#.*|\\1|p' \"$LOCK_FILE\" | head -n1)\n          \
             if [ -z \"$rev\" ] && [ -f \"$APP_MANIFEST\" ]; then\n            \
               rev=$(sed -n 's|.*git = \"{SDK_GIT}\", rev = \"\\([0-9a-f]\\{{40\\}}\\)\".*|\\1|p' \"$APP_MANIFEST\" | head -n1)\n          \
             fi\n          \
             echo \"rev=${{rev:-main}}\" >> \"$GITHUB_OUTPUT\"\n\
         \x20     - uses: actions/cache@v4\n        \
           with:\n          \
             path: .tilo-cli\n          \
             key: tilo-app-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-${{{{ steps.sdk.outputs.rev }}}}-${{{{ hashFiles({lock_expression}) }}}}\n\
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

    /// The revision step reads the SDK pin from a lock a build resolved, from
    /// the app's manifest when the lock is still the scaffold's seed (which
    /// names no SDK source), and falls back to `main` only when neither pins
    /// one: a CLI from another revision than the app's SDK may not build it.
    #[test]
    fn the_revision_step_reads_the_lock_then_the_app_manifest() {
        use crate::standalone::{Baseline, SdkSource, Standalone};

        let text = workflow(Forge::Forgejo, "docker", "apps/editor", "Cargo.lock");
        let yaml: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid YAML");
        let step = yaml["jobs"]["check"]["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find(|step| step["id"].as_str() == Some("sdk"))
            .expect("the revision step");
        assert_eq!(
            step["env"]["APP_MANIFEST"].as_str(),
            Some("apps/editor/Cargo.toml")
        );
        let script = step["run"].as_str().expect("its script");

        let pinned = "0123456789abcdef0123456789abcdef01234567";
        let scaffold = Standalone {
            source: SdkSource::Git {
                url: SDK_GIT.to_owned(),
                rev: Some(pinned.to_owned()),
            },
            baseline: Baseline::embedded().expect("the embedded baseline"),
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = temp.path().join("Cargo.toml");
        let lock = temp.path().join("Cargo.lock");
        let output = temp.path().join("output");
        let revision = |lock_text: &str| {
            std::fs::write(&lock, lock_text).expect("lock");
            std::fs::write(&output, "").expect("output");
            let status = std::process::Command::new("bash")
                .arg("-c")
                .arg(script)
                .env("LOCK_FILE", &lock)
                .env("APP_MANIFEST", &manifest)
                .env("GITHUB_OUTPUT", &output)
                .status()
                .expect("bash runs the step");
            assert!(status.success(), "the step failed: {status}");
            std::fs::read_to_string(&output).expect("step output")
        };

        std::fs::write(
            &manifest,
            format!(
                "[package]\nname = \"editor\"\n\n[dependencies]\n{}",
                scaffold.dependencies()
            ),
        )
        .expect("manifest");
        let seeded = "[[package]]\nname = \"tilo-app\"\nversion = \"0.1.0\"\n";
        assert_eq!(revision(seeded), format!("rev={pinned}\n"));

        let resolved_rev = "fedcba9876543210fedcba9876543210fedcba98";
        let resolved =
            format!("{seeded}source = \"git+{SDK_GIT}?rev={resolved_rev}#{resolved_rev}\"\n");
        assert_eq!(revision(&resolved), format!("rev={resolved_rev}\n"));

        std::fs::write(
            &manifest,
            "[dependencies]\nui = { package = \"tilo-app\", path = \"../tilo/crates/sdk/tilo-app\" }\n",
        )
        .expect("path manifest");
        assert_eq!(revision(seeded), "rev=main\n");
    }

    #[test]
    fn github_reusable_checks_the_callers_app_and_publishes_only_when_requested() {
        let text = github_reusable("ubuntu-latest");
        let yaml: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid YAML");
        let call = &yaml["on"]["workflow_call"];
        assert_eq!(call["inputs"]["publish"]["type"].as_str(), Some("boolean"));
        assert_eq!(call["inputs"]["publish"]["default"].as_bool(), Some(false));
        assert_eq!(
            call["secrets"]["TILO_PUBLISHER_KEY"]["required"].as_bool(),
            Some(false)
        );
        assert_eq!(yaml["permissions"]["contents"].as_str(), Some("read"));
        assert_eq!(
            yaml["jobs"]["check"]["defaults"]["run"]["working-directory"].as_str(),
            Some("${{ inputs.working-directory }}")
        );
        assert_eq!(
            yaml["jobs"]["publish"]["if"].as_str(),
            Some("inputs.publish && github.event_name != 'pull_request'")
        );
        assert_eq!(
            yaml["jobs"]["package"]["strategy"]["matrix"]["include"]
                .as_sequence()
                .expect("matrix")
                .len(),
            GITHUB_BUILDS.len()
        );
        assert!(text.contains("hashFiles(inputs.lockfile)"));
        assert!(!text.contains("hashFiles('${{"));
        assert!(text.contains("\"$LOCK_FILE\""));
        assert!(text.contains("${{ inputs.working-directory }}/dist"));
        assert!(text.contains("inputs.registry || vars.TILO_REGISTRY"));
        for secret in [
            "TILO_PUBLISHER_KEY",
            "TILO_ANDROID_KEYSTORE",
            "TILO_ANDROID_KEYSTORE_PASS",
        ] {
            assert!(text.contains(&format!("${{{{ inputs.publish && github.event_name != 'pull_request' && secrets.{secret} || '' }}}}")));
            assert!(!text.contains(&format!("${{{{ secrets.{secret} }}}}")));
        }
        assert!(yaml["defaults"]["run"]["working-directory"].is_null());
    }

    #[test]
    fn github_generator_writes_a_callable_pair_and_preserves_existing_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        // The app directory resolves through symlinks (macOS: /var is /private/var).
        let root = temp.path().canonicalize().expect("canonical tempdir");
        let app = root.join("apps/editor");
        std::fs::create_dir_all(root.join(".git")).expect("git");
        std::fs::create_dir_all(&app).expect("app");
        std::fs::write(root.join("Cargo.lock"), "").expect("lock");
        let reusable = root.join(".github/workflows/tilo-build.yml");
        let preview = add_ci(&app, Forge::Github, None, false, true).expect("preview");
        assert_eq!(preview.reusable_path.as_ref(), Some(&reusable));
        assert_eq!(preview.to_json()["written"], false);
        assert!(!preview.path.exists());
        assert!(!reusable.exists());
        std::fs::create_dir_all(reusable.parent().expect("parent")).expect("directory");
        std::fs::write(&reusable, "existing workflow").expect("existing");
        assert!(add_ci(&app, Forge::Github, None, false, false).is_err());
        assert!(
            !preview.path.exists(),
            "refusal must not create half the pair"
        );
        assert_eq!(
            std::fs::read_to_string(&reusable).expect("read"),
            "existing workflow"
        );
        let outcome = add_ci(&app, Forge::Github, None, true, false).expect("write pair");
        let caller: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&outcome.path).expect("caller"))
                .expect("YAML");
        assert_eq!(
            caller["jobs"]["app"]["uses"].as_str(),
            Some("./.github/workflows/tilo-build.yml")
        );
        assert_eq!(
            caller["jobs"]["app"]["with"]["working-directory"].as_str(),
            Some("apps/editor")
        );
        assert_eq!(
            caller["jobs"]["app"]["with"]["lockfile"].as_str(),
            Some("Cargo.lock")
        );
        assert_eq!(
            caller["jobs"]["app"]["with"]["publish"].as_str(),
            Some("${{ startsWith(github.ref, 'refs/tags/v') }}")
        );
        let reusable_document: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&reusable).expect("callee"))
                .expect("YAML");
        for secret in caller["jobs"]["app"]["secrets"]
            .as_mapping()
            .expect("secrets")
            .keys()
        {
            assert!(
                !reusable_document["on"]["workflow_call"]["secrets"][secret].is_null(),
                "caller secret must be declared by callee"
            );
        }
    }

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
        // The app directory resolves through symlinks (macOS: /var is /private/var).
        let root = temp.path().canonicalize().expect("canonical tempdir");
        let app = root.join("apps/editor");
        std::fs::create_dir_all(root.join(".git")).expect("git");
        std::fs::create_dir_all(&app).expect("app");
        std::fs::write(root.join("Cargo.lock"), "").expect("lock");
        let outcome = add_ci(&app, Forge::Gitea, None, false, false).expect("add ci");
        assert_eq!(outcome.path, root.join(".gitea/workflows/tilo.yml"));
        assert_eq!(outcome.working_directory, "apps/editor");
        let text = std::fs::read_to_string(&outcome.path).expect("written");
        assert!(text.contains("hashFiles('Cargo.lock')"));
        assert!(add_ci(&app, Forge::Gitea, None, false, false).is_err());
        assert!(add_ci(&app, Forge::Gitea, None, true, false).is_ok());
    }
}
