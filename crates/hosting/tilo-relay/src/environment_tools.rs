//! Tool builds for members' environments, signed by the relay's worker.
//!
//! A member's supervisor runs as the member and never holds the relay's
//! device key; its `pkg install` asks the worker instead
//! (`env_tools::delegated`). For each member the worker listens on a socket
//! in the member's runtime directory that only the member's uid can reach.
//! On a request it:
//!
//! 1. runs `tilo-env-tools build-for` **as the member** — their uid, their
//!    cgroup (CPU, memory, processes), their area for scratch and caches,
//!    their image store for the runtime — so a package's scriptlets run
//!    with the member's authority and nothing more;
//! 2. reads the one image that build left (a single regular file, opened
//!    without following links), checks it is a `tool` image for the runtime
//!    asked for and this architecture, and signs it with the relay key in
//!    the worker's own process ([`env_tools::delegated::sign_delegated`]);
//! 3. leaves the signed image on the member's shelf, where their supervisor
//!    fetches it (trusting the relay's public key), and answers with its id.
//!
//! Nothing a member builds is shared with another member: each has their
//! own shelf, store and build cache.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use env_tools::delegated::{BuildReply, BuildRequest, MAX_LINE_BYTES, sign_delegated};
use tilo_app_package::{PackageId, SigningIdentity};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use crate::environment_host::MemberArea;

/// Packages one build may name.
const MAX_PACKAGES: usize = 64;
/// Longest package name.
const MAX_PACKAGE_BYTES: usize = 256;

/// One member, as their builds run.
#[derive(Clone, Debug)]
pub struct MemberRun {
    /// Their area.
    pub area: MemberArea,
    /// Their uid, when members are separated by uid.
    pub uid: Option<u32>,
    /// Their cgroup, when the worker has a delegated one.
    pub cgroup: Option<PathBuf>,
    /// The environment their supervisor runs with (stores, caches, image
    /// sources); builds see the same.
    pub env: Vec<(OsString, OsString)>,
}

impl MemberRun {
    /// Where the member's builds leave their unsigned images.
    #[must_use]
    pub fn builds(&self) -> PathBuf {
        self.area.root().join("builds")
    }

    /// Where the signed images are left for the member's supervisor.
    #[must_use]
    pub fn shelf(&self) -> PathBuf {
        self.area.root().join("shelf")
    }

    /// The socket the member's supervisor asks for builds on.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.area.runtime().join("builds.sock")
    }
}

/// Runs the unprivileged half of a build: resolve, install and seal, as the
/// member, leaving an image with a placeholder signature at `out`.
pub trait MemberBuild: Send + Sync + 'static {
    /// Build `request` for `member`; `progress` receives the build's output.
    ///
    /// # Errors
    ///
    /// Why the build failed, for the member's shell.
    fn build(
        &self,
        member: &MemberRun,
        request: &BuildRequest,
        publisher: [u8; 32],
        out: &Path,
        progress: &mut dyn FnMut(String),
    ) -> Result<(), String>;
}

/// [`MemberBuild`] by running `tilo-env-tools build-for` as the member.
#[derive(Clone, Debug)]
pub struct EnvToolsBuild {
    /// The `tilo-env-tools` binary.
    pub binary: PathBuf,
}

impl EnvToolsBuild {
    /// The command that builds `request` as `member`. It inherits nothing
    /// from the worker: no environment beyond the member's, no identity.
    #[must_use]
    pub fn command(
        &self,
        member: &MemberRun,
        request: &BuildRequest,
        publisher: [u8; 32],
        out: &Path,
    ) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .arg("build-for")
            .arg("--publisher")
            .arg(hex::encode(publisher))
            .arg("--runtime")
            .arg(&request.runtime)
            .arg("--out")
            .arg(out)
            .arg("--")
            .args(&request.packages)
            .env_clear()
            .envs(member.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(uid) = member.uid {
            use std::os::unix::process::CommandExt;
            command.uid(uid).gid(uid);
            // SAFETY: `setgroups` is async-signal-safe and allocates nothing.
            unsafe {
                command.pre_exec(|| {
                    if libc::setgroups(0, std::ptr::null()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        command
    }
}

impl MemberBuild for EnvToolsBuild {
    fn build(
        &self,
        member: &MemberRun,
        request: &BuildRequest,
        publisher: [u8; 32],
        out: &Path,
        progress: &mut dyn FnMut(String),
    ) -> Result<(), String> {
        let mut child = self
            .command(member, request, publisher, out)
            .spawn()
            .map_err(|error| format!("start the build: {error}"))?;
        if let Some(cgroup) = &member.cgroup
            && let Err(error) = std::fs::write(cgroup.join("cgroup.procs"), child.id().to_string())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("place the build in your quota: {error}"));
        }
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                progress(format!("{line}\n"));
            }
        }
        let status = child
            .wait()
            .map_err(|error| format!("wait for the build: {error}"))?;
        if !status.success() {
            return Err(format!("the build failed ({status})"));
        }
        Ok(())
    }
}

/// The relay's side of members' tool builds.
#[derive(Clone)]
pub struct ToolBuildService {
    identity: Arc<SigningIdentity>,
    builds: Arc<dyn MemberBuild>,
}

impl ToolBuildService {
    /// Builds run by `builds`, signed by `key`.
    #[must_use]
    pub fn new(key: ed25519_dalek::SigningKey, builds: Arc<dyn MemberBuild>) -> Self {
        Self {
            identity: Arc::new(SigningIdentity::from_signing_key(key)),
            builds,
        }
    }

    /// The relay's public key, which members' supervisors trust.
    #[must_use]
    pub fn publisher(&self) -> [u8; 32] {
        self.identity.public_key_bytes()
    }

    /// Listen for `member`'s supervisor on [`MemberRun::socket`], reachable
    /// only by the member's uid. Ends when the returned task is aborted.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be made, or there is no
    /// runtime to serve it on.
    pub fn listen(&self, member: MemberRun) -> std::io::Result<tokio::task::JoinHandle<()>> {
        use std::os::unix::fs::PermissionsExt;
        let runtime = tokio::runtime::Handle::try_current().map_err(std::io::Error::other)?;
        let socket = member.socket();
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        if let Some(uid) = member.uid {
            std::os::unix::fs::chown(&socket, Some(uid), Some(uid))?;
        }
        listener.set_nonblocking(true)?;
        let _entered = runtime.enter();
        let listener = tokio::net::UnixListener::from_std(listener)?;
        let service = self.clone();
        Ok(runtime.spawn(async move {
            let member = Arc::new(member);
            while let Ok((stream, _)) = listener.accept().await {
                let expected = member.uid.unwrap_or_else(|| {
                    // SAFETY: geteuid cannot fail.
                    unsafe { libc::geteuid() }
                });
                if stream.peer_cred().map(|cred| cred.uid()).ok() != Some(expected) {
                    continue;
                }
                let service = service.clone();
                let member = Arc::clone(&member);
                tokio::spawn(async move {
                    if let Err(error) = service.serve(&member, stream).await {
                        tracing::debug!(%error, "member tool build connection ended");
                    }
                });
            }
        }))
    }

    async fn serve(
        &self,
        member: &Arc<MemberRun>,
        stream: tokio::net::UnixStream,
    ) -> std::io::Result<()> {
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        tokio::io::BufReader::new(read)
            .take(u64::try_from(MAX_LINE_BYTES).unwrap_or(u64::MAX))
            .read_line(&mut line)
            .await?;
        let (progress_tx, mut progress) = tokio::sync::mpsc::unbounded_channel::<String>();
        let request = serde_json::from_str::<BuildRequest>(&line);
        let service = self.clone();
        let builder_member = Arc::clone(member);
        let build = tokio::task::spawn_blocking(move || match request {
            Ok(request) => service.build_blocking(&builder_member, &request, &mut |text| {
                let _ = progress_tx.send(text);
            }),
            Err(error) => Err(format!("malformed build request: {error}")),
        });
        while let Some(text) = progress.recv().await {
            send(&mut write, &BuildReply::Progress(text)).await?;
        }
        let reply = match build.await {
            Ok(Ok(id)) => BuildReply::Built(id.to_string()),
            Ok(Err(reason)) => BuildReply::Failed(reason),
            Err(_) => BuildReply::Failed("the build panicked".into()),
        };
        send(&mut write, &reply).await
    }

    /// Build, check and sign `request` for `member`; the signed image is on
    /// the member's shelf.
    ///
    /// # Errors
    ///
    /// Why there is no tool, for the member's shell.
    pub fn build_blocking(
        &self,
        member: &MemberRun,
        request: &BuildRequest,
        progress: &mut dyn FnMut(String),
    ) -> Result<PackageId, String> {
        validate(request)?;
        let runtime: PackageId = request
            .runtime
            .parse()
            .map_err(|error| format!("runtime {:?}: {error}", request.runtime))?;
        let arch = env_tools::image::host_arch().map_err(|error| error.to_string())?;
        let builds = member.builds();
        member_directory(&builds, member.uid).map_err(|error| format!("build area: {error}"))?;
        let out = builds.join(format!("{}.tapp", hex::encode(rand::random::<[u8; 16]>())));
        let built = self
            .builds
            .build(member, request, self.publisher(), &out, progress)
            .and_then(|()| {
                sign_delegated(&out, &runtime, arch, &self.identity, &member.shelf())
                    .map_err(|error| error.to_string())
            });
        let _ = std::fs::remove_file(&out);
        let id = built?;
        if let Some(uid) = member.uid {
            let path = member.shelf().join(format!("{}.tapp", id.to_hex()));
            for path in [member.shelf(), path] {
                std::os::unix::fs::chown(&path, Some(uid), Some(uid))
                    .map_err(|error| format!("hand the image over: {error}"))?;
            }
        }
        Ok(id)
    }
}

/// A request names a runtime and a few package names that can't be taken
/// for options.
fn validate(request: &BuildRequest) -> Result<(), String> {
    let plain = |package: &String| {
        !package.is_empty()
            && package.len() <= MAX_PACKAGE_BYTES
            && !package.starts_with('-')
            && !package.contains(['\0', '\n'])
    };
    if request.packages.is_empty()
        || request.packages.len() > MAX_PACKAGES
        || !request.packages.iter().all(plain)
    {
        return Err("a build names one to 64 package names".into());
    }
    Ok(())
}

fn member_directory(path: &Path, uid: Option<u32>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    if let Some(uid) = uid {
        std::os::unix::fs::chown(path, Some(uid), Some(uid))?;
    }
    Ok(())
}

async fn send(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    reply: &BuildReply,
) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(reply).map_err(std::io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line).await
}

/// Whether the key file at `path` is readable by its owner alone, the
/// worker's uid: what keeps members' uids from it.
///
/// # Errors
///
/// Returns an error when the file cannot be inspected.
pub fn key_file_private(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    // SAFETY: geteuid cannot fail.
    let worker = unsafe { libc::geteuid() };
    Ok(metadata.is_file() && metadata.uid() == worker && metadata.mode().trailing_zeros() >= 6)
}

#[cfg(test)]
mod tests;
