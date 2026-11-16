//! Process lifecycle of the native MCP host: its profile, its apps and its stdio connection.

use serde::Deserialize;
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use crate::RuntimeServices;
use tilo_mcp::lines::{mcp_answer, serve_lines, spawn_reader};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppManifest {
    pub apps: Vec<AppSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    pub id: String,
    pub executable: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug)]
pub struct ValidatedManifest {
    pub apps: Vec<AppSpec>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Args {
    pub profile: PathBuf,
    pub apps: PathBuf,
}

/// Parse the `--profile PATH --apps MANIFEST` command line.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an unknown or repeated
/// argument, a flag without its path, or a missing required flag.
pub(crate) fn parse_args(args: impl IntoIterator<Item = OsString>) -> io::Result<Args> {
    let mut profile = None;
    let mut apps = None;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--profile") if profile.is_none() => {
                profile = Some(PathBuf::from(args.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--profile requires a path")
                })?));
            }
            Some("--apps") if apps.is_none() => {
                apps = Some(PathBuf::from(args.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--apps requires a path")
                })?));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unexpected argument: {}", argument.to_string_lossy()),
                ));
            }
        }
    }
    Ok(Args {
        profile: profile
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing --profile PATH"))?,
        apps: apps.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "missing --apps MANIFEST")
        })?,
    })
}

/// Check that every manifest entry names an existing executable by absolute path.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an empty app id, a relative
/// executable path or a path that is not an executable file, and the
/// underlying error kind when the executable cannot be inspected.
pub fn validate_manifest(manifest: AppManifest) -> io::Result<ValidatedManifest> {
    for (index, app) in manifest.apps.iter().enumerate() {
        if app.id.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("apps[{index}].id must not be empty"),
            ));
        }
        if !app.executable.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("apps[{index}].executable must be absolute"),
            ));
        }
        let metadata = std::fs::metadata(&app.executable).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("apps[{index}].executable is unavailable: {error}"),
            )
        })?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("apps[{index}].executable is not an executable file"),
            ));
        }
    }
    Ok(ValidatedManifest {
        apps: manifest.apps,
    })
}

/// Read, parse and validate the app manifest at `path`.
///
/// # Errors
///
/// Returns the read error, [`io::ErrorKind::InvalidInput`] for malformed
/// JSON, or any [`validate_manifest`] error.
pub(crate) fn load_manifest(path: &Path) -> io::Result<ValidatedManifest> {
    let bytes = std::fs::read(path)?;
    let manifest = serde_json::from_slice(bytes.as_slice()).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid app manifest: {error}"),
        )
    })?;
    validate_manifest(manifest)
}

/// Create private profile directories and return the startup environment.
///
/// This function does not mutate process-global state. The native executable
/// applies the returned values at its single-threaded startup boundary.
pub(crate) fn prepare_profile_environment(
    profile: &Path,
) -> io::Result<[(&'static str, PathBuf); 5]> {
    if !profile.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile path must be absolute",
        ));
    }
    crate::unix::private_directory(profile)?;
    let paths = profile_environment(profile);
    for (_, path) in &paths {
        crate::unix::private_directory(path)?;
    }
    Ok(paths)
}

fn profile_environment(profile: &Path) -> [(&'static str, PathBuf); 5] {
    [
        ("XDG_CONFIG_HOME", profile.join("config")),
        ("XDG_DATA_HOME", profile.join("data")),
        ("XDG_CACHE_HOME", profile.join("cache")),
        ("XDG_STATE_HOME", profile.join("state")),
        ("TILO_IDENTITY_ROOT", profile.join("identity")),
    ]
}

/// An admission profile is prepared by its canonical owner unit. Verify its
/// existing private directories without chmod or provisioning; some contain
/// read-only projections of the authoritative identity and selections.
pub(crate) fn prepare_admission_profile_environment(
    profile: &Path,
) -> io::Result<[(&'static str, PathBuf); 5]> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    if !profile.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "admission profile must be absolute",
        ));
    }
    let paths = profile_environment(profile);
    let uid = rustix::process::geteuid().as_raw();
    for path in std::iter::once(profile).chain(paths.iter().map(|(_, path)| path.as_path())) {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir()
            || metadata.uid() != uid
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "admission profile must already have private account-owned directories",
            ));
        }
    }
    Ok(paths)
}

struct OwnedChild {
    id: String,
    child: Child,
}

#[derive(Default)]
struct OwnedChildren(Vec<OwnedChild>);

impl OwnedChildren {
    fn check(&mut self) -> io::Result<()> {
        for owned in &mut self.0 {
            if let Some(status) = owned.child.try_wait()? {
                return Err(io::Error::other(format!(
                    "application {} (pid {}) exited: {status}",
                    owned.id,
                    owned.child.id()
                )));
            }
        }
        Ok(())
    }
}

impl Drop for OwnedChildren {
    fn drop(&mut self) {
        for owned in &mut self.0 {
            let _ = owned.child.kill();
            let _ = owned.child.wait();
        }
    }
}

fn log_file(profile: &Path, index: usize, app_id: &str) -> io::Result<File> {
    let logs = profile.join("logs");
    crate::unix::private_directory(&logs)?;
    let safe_id: String = app_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(logs.join(format!("{}-{index:03}-{safe_id}.log", std::process::id())))
}

fn launch_apps<S: RuntimeServices>(
    session: &S,
    profile: &Path,
    manifest: ValidatedManifest,
) -> io::Result<OwnedChildren> {
    let mut children = OwnedChildren::default();
    for (index, app) in manifest.apps.into_iter().enumerate() {
        let log = log_file(profile, index, &app.id)?;
        let mut command = Command::new(&app.executable);
        command
            .args(&app.args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let child = session.launch(&app.id, command).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to launch apps[{index}] {}: {error}", app.id),
            )
        })?;
        children.0.push(OwnedChild { id: app.id, child });
    }
    Ok(children)
}

/// Own one MCP connection, one production service session and its app children.
///
/// The caller supplies the process environment before invoking this reusable
/// serving layer. This function never mutates process-global environment.
///
/// Returns `Ok(())` when the input reaches end of file.
pub fn serve<S, R, W>(
    profile: &Path,
    manifest: ValidatedManifest,
    reader: R,
    mut writer: W,
) -> io::Result<()>
where
    S: RuntimeServices,
    R: Read + Send + 'static,
    W: Write,
{
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let session = S::start_reporting(profile, &mut |_| {})?;
    let server = session.mcp_session_factory(session.workspace())?()?;
    let mut children = launch_apps(&session, profile, manifest)?;
    let input = spawn_reader(reader)?;
    let server = std::sync::Arc::new(server);
    serve_lines(
        runtime,
        input,
        &mut writer,
        |request| mcp_answer(&server, request),
        Some(&mut || children.check()),
    )
}

/// Load a validated manifest and serve with the caller's configured profile environment.
pub(crate) fn run<S: RuntimeServices>(profile: &Path, manifest_path: &Path) -> io::Result<()> {
    // The complete startup configuration is parsed and validated before
    // profile services or any application process is started.
    let manifest = load_manifest(manifest_path)?;
    serve::<S, _, _>(profile, manifest, std::io::stdin(), std::io::stdout())
}

#[cfg(test)]
#[path = "mcp_process_tests.rs"]
mod tests;
