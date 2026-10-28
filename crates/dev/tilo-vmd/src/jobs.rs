//! Long jobs run as `ssh <vm> tilo-vmd build` or `tilo-vmd ci-layer`:
//! the job arrives on stdin, every tool's output streams to stderr (the
//! host shows it live), and the outcome is one [`JobLine`] on stdout.
//!
//! `build` compiles in the synced worktree `<root>/src/<worktree>` with the
//! dev image's toolchain and the shared sccache, lays the component out with
//! the manifest installer from that same tree, and imports the result
//! straight into the layer store: no bytes travel back to the host.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use tilo_vm_protocol::manifest::{Component, dev_override_features, valid_profile};
use tilo_vm_protocol::{
    AppInstallResult, BuildJob, BuildTarget, CiJob, JobLine, JobOutput, LayerSource, Manifest,
    Provenance,
};

use crate::build_user::{self, BuildUser};
use crate::daemon::{Daemon, on_path};
use crate::error::DaemonError;
use crate::fsutil;

/// Shared compiler cache for every worktree's in-VM builds.
pub const SCCACHE_DIR: &str = "/var/cache/tilo-vm/sccache";
/// bootc's registry credentials, used for upgrades and CI layers alike. Tilo
/// images are public; only a private registry needs an entry here.
pub const AUTH_FILE: &str = "/etc/ostree/auth.json";

/// Run `command`, streaming its stdout and stderr to our stderr (stdout
/// carries only the job line), and keep the tail of its stderr.
///
/// # Errors
/// The program could not be started.
pub fn stream(step: &str, command: &mut Command) -> Result<(bool, String), DaemonError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DaemonError::ToolMissing {
                    tool: command.get_program().display().to_string(),
                    purpose: step.to_owned(),
                }
            } else {
                DaemonError::Job {
                    step: step.to_owned(),
                    detail: e.to_string(),
                }
            }
        })?;
    let stdout = child.stdout.take();
    let forward = std::thread::spawn(move || {
        if let Some(mut out) = stdout {
            let _ = std::io::copy(&mut out, &mut std::io::stderr());
        }
    });
    let mut tail: Vec<u8> = Vec::new();
    if let Some(mut err) = child.stderr.take() {
        let mut buffer = [0_u8; 8192];
        loop {
            let n = match err.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = buffer.get(..n).unwrap_or_default();
            let _ = std::io::stderr().write_all(chunk);
            tail.extend_from_slice(chunk);
            let excess = tail.len().saturating_sub(16 * 1024);
            tail.drain(..excess);
        }
    }
    let _ = forward.join();
    let status = child.wait().map_err(|e| DaemonError::Job {
        step: step.to_owned(),
        detail: e.to_string(),
    })?;
    Ok((
        status.success(),
        String::from_utf8_lossy(&tail).into_owned(),
    ))
}

/// Run `command` and fail the job when it fails.
fn step(name: &str, command: &mut Command) -> Result<(), DaemonError> {
    let (ok, tail) = stream(name, command)?;
    if ok {
        Ok(())
    } else {
        Err(DaemonError::Job {
            step: name.to_owned(),
            detail: last_line(&tail),
        })
    }
}

fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("see the output above")
        .to_owned()
}

fn capture(command: &mut Command) -> Option<String> {
    let output = command.stderr(Stdio::null()).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `(hits, misses)` of the shared sccache. Asked as the build user: asking
/// starts the server when none runs, and a server started as root would
/// write root-owned cache entries for every later build.
fn sccache_totals(user: Option<&BuildUser>) -> Option<(u64, u64)> {
    let mut stats = Command::new("sccache");
    stats
        .args(["--show-stats", "--stats-format=json"])
        .env("SCCACHE_DIR", SCCACHE_DIR);
    as_user(user, &mut stats);
    let text = capture(&mut stats)?;
    let stats: serde_json::Value = serde_json::from_str(&text).ok()?;
    let sum = |key: &str| -> Option<u64> {
        let counts = stats.get("stats")?.get(key)?.get("counts")?.as_object()?;
        Some(
            counts
                .values()
                .filter_map(serde_json::Value::as_u64)
                .fold(0, u64::saturating_add),
        )
    };
    Some((sum("cache_hits")?, sum("cache_misses")?))
}

/// Environment of every cargo run in a job.
pub fn cargo_env(command: &mut Command, color: bool, sccache: bool) {
    if sccache && std::env::var_os("RUSTC_WRAPPER").is_none() {
        command.env("RUSTC_WRAPPER", "sccache");
    }
    if std::env::var_os("SCCACHE_DIR").is_none() {
        command.env("SCCACHE_DIR", SCCACHE_DIR);
    }
    if color {
        command
            .env("CARGO_TERM_COLOR", "always")
            .env("CARGO_TERM_PROGRESS_WHEN", "always")
            .env("CARGO_TERM_PROGRESS_WIDTH", "100");
    }
}

/// Toolchain and target triple of the VM's `rustc`.
fn toolchain() -> (Option<String>, Option<String>) {
    let version = capture(Command::new("rustc").arg("--version"))
        .map(|v| v.lines().next().unwrap_or_default().trim().to_owned());
    let target = capture(Command::new("rustc").arg("-vV")).and_then(|v| {
        v.lines()
            .find_map(|l| l.strip_prefix("host: "))
            .map(|t| t.trim().to_owned())
    });
    (version, target)
}

struct Timer(BTreeMap<String, f64>);

impl Timer {
    fn time<T>(&mut self, name: &str, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let value = f();
        let seconds = (start.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
        self.0.insert(name.to_owned(), seconds);
        value
    }
}

/// Apps a synced tree offers: `apps/<dir>/tilo-app.toml`.
pub fn apps_in(src: &Path) -> Vec<String> {
    let mut apps: Vec<String> = std::fs::read_dir(src.join("apps"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("tilo-app.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    apps.sort();
    apps
}

impl Daemon {
    /// Run a [`BuildJob`].
    ///
    /// # Errors
    /// Missing sources, tools or components, and failing build steps.
    pub fn build_job(&mut self, job: BuildJob) -> Result<JobOutput, DaemonError> {
        let src = self.targets.src(&job.worktree);
        self.targets.touch(&job.worktree, job.owner.clone())?;
        if !src.join("Cargo.toml").is_file() {
            return Err(DaemonError::SourceMissing(job.worktree));
        }
        let _build = self.locks.src(&job.worktree)?;
        if !on_path("cargo") {
            return Err(DaemonError::ToolMissing {
                tool: "cargo".into(),
                purpose: "build in the VM".into(),
            });
        }
        let user = BuildUser::for_build_root(self.targets.dir())?;
        if let Some(user) = &user {
            prepare_user_build(user, &src)?;
        }
        let result = match &job.target {
            BuildTarget::Component { name, profile } => {
                self.build_component(&job, &src, name, profile.as_deref(), user.as_ref())
            }
            BuildTarget::App { dir, slot } => self.build_app(&job, &src, dir, slot, user.as_ref()),
        };
        // Record the build dir's size even after a failed build: it still costs disk.
        self.targets.measure(&job.worktree)?;
        if let Some(user) = &user {
            user.own(&self.targets.record_path(&job.worktree))?;
        }
        result
    }

    /// The provenance of a component built in the VM: the job's, completed
    /// with the builder, base image, toolchain and the build's choices.
    fn vm_build_provenance(
        &self,
        job: &BuildJob,
        component: &Component,
        extra: &[String],
    ) -> Provenance {
        let mut provenance = job.provenance.clone();
        provenance.component.clone_from(&component.name);
        provenance.builder = Some("vm".into());
        provenance
            .builder_image_digest
            .clone_from(&self.base.builder_image_digest);
        provenance
            .base_image_digest
            .clone_from(&self.base.image_digest);
        let (version, target) = toolchain();
        provenance.toolchain = version;
        provenance.target = target;
        provenance.cargo_profile = Some(component.profile.clone());
        provenance.features = component.build_features(extra);
        provenance
    }

    fn build_component(
        &mut self,
        job: &BuildJob,
        src: &Path,
        name: &str,
        profile: Option<&str>,
        user: Option<&BuildUser>,
    ) -> Result<JobOutput, DaemonError> {
        if let Some(profile) = profile
            && !valid_profile(profile)
        {
            return Err(DaemonError::InvalidParams(format!(
                "'{profile}' is not a cargo profile name"
            )));
        }
        let manifest_path = src.join("os/components.toml");
        let manifest = Manifest::load(&manifest_path)
            .map_err(|e| DaemonError::InvalidParams(e.to_string()))?
            .ok_or_else(|| DaemonError::SourceMissing(job.worktree.clone()))?;
        let component = manifest
            .get(name)
            .ok_or_else(|| DaemonError::UnknownComponent {
                name: name.to_owned(),
                known: manifest.components.keys().cloned().collect(),
            })?
            .clone()
            .with_profile(profile);
        let mut metadata = Command::new("cargo");
        metadata
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(src);
        as_user(user, &mut metadata);
        let metadata = capture(&mut metadata)
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(serde_json::Value::Null);
        let extra = dev_override_features(&metadata, &component.packages);
        let sccache = on_path("sccache");
        let mut timer = Timer(BTreeMap::new());
        let before = sccache.then(|| sccache_totals(user)).flatten();
        let mut cargo = Command::new("cargo");
        cargo.args(component.cargo_args(&extra)).current_dir(src);
        cargo_env(&mut cargo, job.color, sccache);
        as_user(user, &mut cargo);
        if matches!(component.profile.as_str(), "dev" | "test")
            && let (_, Some(target)) = toolchain()
        {
            // The driver lives beside the build dirs, where the build user
            // can run it; the daemon's own state is root's alone.
            crate::linker::configure(
                &mut cargo,
                &self.targets.dir().join(".linkers"),
                &target,
                user,
            )?;
        }
        eprintln!(
            "tilo-vmd: cargo {} (in {})",
            component.cargo_args(&extra).join(" "),
            src.display()
        );
        timer.time("build", || step("cargo build", &mut cargo))?;
        let after = sccache.then(|| sccache_totals(user)).flatten();

        let staging = self
            .root
            .join("staging")
            .join(format!("build-{}", uuid::Uuid::new_v4().simple()));
        let tree = staging.join("tree");
        let from = format!("target/{}", component.profile_dir());
        let assembled = timer.time("assemble", || {
            std::fs::create_dir_all(&tree).map_err(DaemonError::io("create build staging"))?;
            step(
                "install-components.py",
                Command::new("python3")
                    .args(component.installer_args(&from, &tree.display().to_string()))
                    .current_dir(src),
            )
        });
        let provenance = self.vm_build_provenance(job, &component, &extra);
        let imported = assembled.and_then(|()| {
            timer.time("import", || {
                let _vm = self.locks.vm_shared()?;
                let apply = &mut self.apply;
                self.layers
                    .import_path(&tree, &provenance, &mut |l| apply.label_layer(l))
            })
        });
        let _ = std::fs::remove_dir_all(&staging);
        let (hash, existed) = imported?;
        let mut cache = BTreeMap::new();
        if let (Some((h0, m0)), Some((h1, m1))) = (before, after) {
            cache.insert("sccache_hits".to_owned(), h1.saturating_sub(h0));
            cache.insert("sccache_misses".to_owned(), m1.saturating_sub(m0));
        }
        Ok(JobOutput::Layer {
            hash,
            existed,
            provenance: Box::new(provenance),
            phases: timer.0,
            cache,
        })
    }

    fn build_app(
        &mut self,
        job: &BuildJob,
        src: &Path,
        dir: &str,
        slot: &str,
        user: Option<&BuildUser>,
    ) -> Result<JobOutput, DaemonError> {
        let apps = apps_in(src);
        if !apps.iter().any(|a| a == dir) {
            return Err(DaemonError::UnknownApp {
                dir: dir.to_owned(),
                known: apps,
            });
        }
        // Refuse before minutes of building for a slot that cannot take it.
        self.user_slot(slot)?;
        let sccache = on_path("sccache");
        let mut timer = Timer(BTreeMap::new());
        // The package is built output: it lands in the tree's own target/,
        // which the build user can write, and installing reads it from there.
        let staging = src
            .join("target/tilo-vm")
            .join(format!("app-{}", uuid::Uuid::new_v4().simple()));
        let tapp = staging.join(format!("{dir}.tapp"));
        let built = timer.time("build", || {
            let mut tools = Command::new("cargo");
            tools.args(app_tool_args()).current_dir(src);
            cargo_env(&mut tools, job.color, sccache);
            as_user(user, &mut tools);
            step("cargo build (tilo-app)", &mut tools)?;
            create_dir_as(user, &staging)?;
            let mut package = Command::new(src.join("target/release/tilo-app"));
            package
                .args(app_package_args(dir, &tapp))
                .env("TILO_PKG", src.join("target/release/tilo-app-package"))
                .current_dir(src);
            cargo_env(&mut package, job.color, sccache);
            as_user(user, &mut package);
            step("tilo-app package", &mut package)
        });
        let installed = built.and_then(|()| {
            timer.time("install", || {
                let _locks = self.slot_locks(slot)?;
                self.install_app(slot, LayerSource::Path(tapp.display().to_string()))
            })
        });
        let _ = std::fs::remove_dir_all(&staging);
        let result: AppInstallResult = installed?;
        Ok(JobOutput::App {
            result,
            phases: timer.0,
        })
    }

    /// Run a [`CiJob`]: pull the image, copy the component's paths out of
    /// it, and import them as a layer with `authority = "ci"`.
    ///
    /// # Errors
    /// Missing podman, registry refusals, paths the image lacks.
    pub fn ci_job(&mut self, job: &CiJob) -> Result<JobOutput, DaemonError> {
        if !on_path("podman") {
            return Err(DaemonError::ToolMissing {
                tool: "podman".into(),
                purpose: "pull CI images in the VM".into(),
            });
        }
        let mut timer = Timer(BTreeMap::new());
        let mut pull = Command::new("podman");
        pull.arg("pull");
        if Path::new(AUTH_FILE).is_file() {
            pull.args(["--authfile", AUTH_FILE]);
        }
        pull.arg(&job.image);
        let (ok, tail) = timer.time("pull", || stream("podman pull", &mut pull))?;
        if !ok {
            return Err(pull_error(&job.image, &tail));
        }
        let mount = capture(Command::new("podman").args(["image", "mount", &job.image]))
            .map(|m| PathBuf::from(m.trim()))
            .ok_or_else(|| DaemonError::Job {
                step: "podman image mount".into(),
                detail: format!("could not mount {}", job.image),
            })?;
        let staging = self
            .root
            .join("staging")
            .join(format!("ci-{}", uuid::Uuid::new_v4().simple()));
        let copied = timer.time("extract", || {
            extract_paths(&mount, &staging.join("tree"), &job.paths, &job.image)
        });
        let digest = capture(Command::new("podman").args([
            "image",
            "inspect",
            "--format",
            "{{.Digest}}",
            &job.image,
        ]))
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty());
        let revision = capture(Command::new("podman").args([
            "image",
            "inspect",
            "--format",
            "{{index .Labels \"org.opencontainers.image.revision\"}}",
            &job.image,
        ]))
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty() && r != "<no value>");
        let _ = Command::new("podman")
            .args(["image", "unmount", &job.image])
            .output();
        let _ = Command::new("podman").args(["rmi", &job.image]).output();
        let provenance = ci_provenance(job, digest, revision);
        let imported = copied.and_then(|()| {
            timer.time("import", || {
                let _vm = self.locks.vm_shared()?;
                let apply = &mut self.apply;
                self.layers
                    .import_path(&staging.join("tree"), &provenance, &mut |l| {
                        apply.label_layer(l)
                    })
            })
        });
        let _ = std::fs::remove_dir_all(&staging);
        let (hash, existed) = imported?;
        Ok(JobOutput::Layer {
            hash,
            existed,
            provenance: Box::new(provenance),
            phases: timer.0,
            cache: BTreeMap::new(),
        })
    }
}

fn as_user(user: Option<&BuildUser>, command: &mut Command) {
    if let Some(user) = user {
        user.apply(command);
    }
}

/// Create `dir` (and its missing parents) for the build user.
fn create_dir_as(user: Option<&BuildUser>, dir: &Path) -> Result<(), DaemonError> {
    let mut missing = Vec::new();
    let mut probe = dir;
    while !probe.exists() {
        missing.push(probe.to_owned());
        let Some(parent) = probe.parent() else { break };
        probe = parent;
    }
    std::fs::create_dir_all(dir).map_err(DaemonError::io(format!("create {}", dir.display())))?;
    if let Some(user) = user {
        for path in missing {
            user.own(&path)?;
        }
    }
    Ok(())
}

/// Before a build as `user`: take back what root-run builds left in the
/// tree and the compiler cache, and stop a root compiler-cache server, so
/// the build (and the owner's own builds after it) can write everything
/// they touch.
fn prepare_user_build(user: &BuildUser, src: &Path) -> Result<(), DaemonError> {
    let mut reclaimed = user.reclaim(&src.join("target"))?;
    let cache =
        std::env::var_os("SCCACHE_DIR").map_or_else(|| PathBuf::from(SCCACHE_DIR), PathBuf::from);
    if std::fs::metadata(&cache).is_ok_and(|meta| {
        use std::os::unix::fs::MetadataExt;
        meta.uid() == user.uid
    }) {
        reclaimed = reclaimed.saturating_add(user.reclaim(&cache)?);
    }
    if reclaimed > 0 {
        eprintln!(
            "tilo-vmd: gave {reclaimed} root-owned build files back to {}",
            user.name
        );
    }
    let port = build_user::sccache_port();
    if on_path("sccache") && build_user::root_listens_on(port) {
        // A server started by an earlier root build: the build user's
        // compiles would go through it and write root-owned cache entries.
        // `--stop-server` asks exactly the server on this port to exit.
        eprintln!("tilo-vmd: stopping the root compiler-cache server on port {port}");
        let _ = Command::new("sccache")
            .arg("--stop-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    Ok(())
}

/// `cargo` arguments that build the app packaging tools.
pub fn app_tool_args() -> Vec<String> {
    [
        "build",
        "--locked",
        "--release",
        "-p",
        "tilo-app-cli",
        "-p",
        "tilo-app-package-cli",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// `tilo-app` arguments that package `apps/<dir>` as a self-signed `.tapp`.
pub fn app_package_args(dir: &str, out: &Path) -> Vec<String> {
    vec![
        "package".into(),
        format!("apps/{dir}"),
        "--output".into(),
        out.display().to_string(),
        "--force".into(),
    ]
}

pub fn ci_provenance(job: &CiJob, digest: Option<String>, revision: Option<String>) -> Provenance {
    Provenance::from_ci(&job.provenance, std::env::consts::ARCH, digest, revision)
}

/// What a failed `podman pull` means.
pub fn pull_error(image: &str, output: &str) -> DaemonError {
    let lower = output.to_ascii_lowercase();
    if lower.contains("401")
        || lower.contains("unauthorized")
        || output.contains("reqPackageAccess")
    {
        DaemonError::RegistryAuth {
            image: image.to_owned(),
        }
    } else if lower.contains("manifest unknown")
        || lower.contains("not found")
        || lower.contains("no image found in manifest list")
    {
        DaemonError::ImageNotFound {
            image: image.to_owned(),
        }
    } else {
        DaemonError::Job {
            step: "podman pull".into(),
            detail: last_line(output),
        }
    }
}

/// Copy each absolute `path` (a trailing `/` copies a tree) from `root`
/// into `dest`, keeping symlinks and modes.
///
/// # Errors
/// A path the image lacks, and I/O failures.
pub fn extract_paths(
    root: &Path,
    dest: &Path,
    paths: &[String],
    image: &str,
) -> Result<(), DaemonError> {
    for path in paths {
        let relative = path.trim_start_matches('/').trim_end_matches('/');
        let from = root.join(relative);
        let to = dest.join(relative);
        let meta = std::fs::symlink_metadata(&from).map_err(|_| DaemonError::MissingInImage {
            image: image.to_owned(),
            path: path.clone(),
        })?;
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(DaemonError::io(format!("create {}", parent.display())))?;
        }
        let copied = if meta.file_type().is_symlink() {
            std::fs::read_link(&from).and_then(|target| fsutil::symlink(&target, &to))
        } else if meta.is_dir() {
            std::fs::create_dir_all(&to).and_then(|()| fsutil::copy_tree(&from, &to))
        } else {
            std::fs::copy(&from, &to).map(drop)
        };
        copied.map_err(DaemonError::io(format!("copy {path} out of the image")))?;
    }
    Ok(())
}

/// Read a job from `input`, run it, and write its [`JobLine`] to `output`.
/// Returns whether the job succeeded.
pub fn run_job<J: serde::de::DeserializeOwned>(
    input: impl Read,
    mut output: impl Write,
    run: impl FnOnce(J) -> Result<JobOutput, DaemonError>,
) -> bool {
    let line = match serde_json::from_reader::<_, J>(input) {
        Ok(job) => match run(job) {
            Ok(result) => JobLine {
                result: Some(result),
                error: None,
            },
            Err(error) => JobLine {
                result: None,
                error: Some(error.to_rpc()),
            },
        },
        Err(e) => JobLine {
            result: None,
            error: Some(DaemonError::InvalidParams(format!("bad job: {e}")).to_rpc()),
        },
    };
    let ok = line.error.is_none();
    let _ = writeln!(output, "{}", line.render());
    let _ = output.flush();
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_failures_are_classified_and_reported() {
        {
            assert!(matches!(
                pull_error(
                    "i",
                    "Error: reading manifest: unauthorized: reqPackageAccess"
                ),
                DaemonError::RegistryAuth { .. }
            ));
            assert!(matches!(
                pull_error("i", "manifest unknown: manifest unknown"),
                DaemonError::ImageNotFound { .. }
            ));
            assert!(matches!(
                pull_error(
                    "i",
                    "Error: choosing an image from manifest list docker://i: no image found in manifest list for architecture \"arm64\", variant \"v8\", OS \"linux\""
                ),
                DaemonError::ImageNotFound { .. }
            ));
            assert!(matches!(
                pull_error("i", "no space left on device\n"),
                DaemonError::Job { .. }
            ));
        }
        {
            let mut out = Vec::new();
            let ok = run_job::<CiJob>(&b"not json"[..], &mut out, |_| {
                Err(DaemonError::HelloRequired)
            });
            assert!(!ok);
            let line = JobLine::find(&String::from_utf8(out).unwrap()).unwrap();
            assert!(line.error.unwrap().message.contains("bad job"));
        }
    }

    #[test]
    fn job_extraction_layers_and_packaging_commands() {
        {
            let image = tempfile::tempdir().unwrap();
            let root = image.path();
            std::fs::create_dir_all(root.join("usr/bin")).unwrap();
            std::fs::write(root.join("usr/bin/app"), b"bin").unwrap();
            fsutil::symlink(Path::new("app"), &root.join("usr/bin/store")).unwrap();
            std::fs::create_dir_all(root.join("usr/lib/tilo-runtime/sub")).unwrap();
            std::fs::write(root.join("usr/lib/tilo-runtime/sub/lib.so"), b"so").unwrap();
            let dest = tempfile::tempdir().unwrap();
            extract_paths(
                root,
                dest.path(),
                &[
                    "/usr/bin/app".into(),
                    "/usr/bin/store".into(),
                    "/usr/lib/tilo-runtime/".into(),
                ],
                "img",
            )
            .unwrap();
            assert_eq!(
                std::fs::read(dest.path().join("usr/bin/app")).unwrap(),
                b"bin"
            );
            assert_eq!(
                std::fs::read_link(dest.path().join("usr/bin/store")).unwrap(),
                Path::new("app")
            );
            assert!(
                dest.path()
                    .join("usr/lib/tilo-runtime/sub/lib.so")
                    .is_file()
            );
            let missing = extract_paths(root, dest.path(), &["/usr/bin/nope".into()], "img");
            assert!(matches!(missing, Err(DaemonError::MissingInImage { .. })));
        }
        {
            let job = CiJob {
                image: "registry.tilo.so/tilo/tilo:main-abc1234".into(),
                component: "shell".into(),
                paths: vec!["/usr/bin/shell".into()],
                provenance: Provenance::local("shell"),
            };
            let p = ci_provenance(&job, Some("sha256:d".into()), Some("abc".into()));
            assert_eq!(p.authority, "ci");
            assert_eq!(p.builder.as_deref(), Some("ci"));
            assert_eq!(p.source_commit.as_deref(), Some("abc"));
            assert_eq!(p.base_image_digest.as_deref(), Some("sha256:d"));
            assert_eq!(
                p.target,
                Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH))
            );
            // The host's extraction for an aarch64 device writes the same record.
            let host =
                Provenance::from_ci(&job.provenance, "aarch64", Some("sha256:d".into()), None);
            assert_eq!(host.target.as_deref(), Some("aarch64-unknown-linux-gnu"));
            assert_eq!(
                (host.authority.as_str(), host.builder.as_deref()),
                ("ci", Some("ci"))
            );
        }
        {
            assert_eq!(
                app_tool_args().join(" "),
                "build --locked --release -p tilo-app-cli -p tilo-app-package-cli"
            );
            assert_eq!(
                app_package_args("notes", Path::new("/s/notes.tapp")).join(" "),
                "package apps/notes --output /s/notes.tapp --force"
            );
        }
    }
}
