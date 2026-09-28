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
];

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
                if build.arch == "x86_64" {
                    matrix_entry(&mut out, build, "", runner);
                } else {
                    matrix_entry(&mut out, build, "# ", "your-aarch64-runner");
                }
            }
            out.push_str(
                "          # Windows and macOS need runners on those systems; register\n          \
                 # them and add entries here as above, with container: \"\".\n",
            );
            out.push_str("    runs-on: ${{ matrix.runner }}\n");
            let _ = writeln!(out, "    container: {BUILDER_IMAGE}");
        }
    }
    out.push_str("    steps:\n");
    steps_prelude(&mut out, lock);
    out.push_str(
        "      - name: Package\n        \
           env:\n          \
             TILO_PUBLISHER_KEY: ${{ secrets.TILO_PUBLISHER_KEY }}\n        \
           run: >-\n          \
             tilo-app package --platform ${{ matrix.platform }} --arch ${{ matrix.arch }}\n          \
             --output dist/${{ matrix.platform }}-${{ matrix.arch }}.tapp\n",
    );
    let _ = write!(
        out,
        "      - uses: {upload}\n        \
           with:\n          \
             name: tapp-${{{{ matrix.platform }}}}-${{{{ matrix.arch }}}}\n          \
             path: {dist}/${{{{ matrix.platform }}}}-${{{{ matrix.arch }}}}.tapp\n          \
             if-no-files-found: error\n\n"
    );

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

fn runner_for<'a>(build: &'a Build, linux_x86: &'a str) -> &'a str {
    if build.platform == "tilo" && build.arch == "x86_64" {
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
                "windows-x86_64",
                "windows-aarch64",
                "macos-aarch64",
                "macos-x86_64"
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
        assert_eq!(include.len(), 1, "only the x86_64 Tilo build is on");
        assert_eq!(include[0]["runner"].as_str(), Some("codeberg-small"));
        assert!(text.contains("https://code.forgejo.org/forgejo/upload-artifact@v4"));
        assert!(!text.contains("actions/upload-artifact"));
        assert_eq!(
            yaml["jobs"]["check"]["runs-on"].as_str(),
            Some("codeberg-small")
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
