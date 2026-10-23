use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use uuid::Uuid;

/// Where sandboxed apps' output lands, relative to `$XDG_RUNTIME_DIR`.
///
/// The runtime directory is owned by the user, mode 0700 and cleared at
/// logout: these logs are for diagnosing a live session, not for keeping.
const APP_LOG_SUBDIR: &str = "tilo/app-logs";
/// Logs kept per session; the oldest are removed before a new one is created.
const MAX_APP_LOGS: usize = 16;
/// Bytes of output kept per app. The runtime directory is RAM-backed, so a
/// chatty client (`WAYLAND_DEBUG` is on) must not be able to fill it. The
/// head is what explains a failed start, so that is what is kept.
const MAX_APP_LOG_BYTES: u64 = 4 * 1024 * 1024;
const LOG_SUFFIX: &str = ".log";

/// The private directory for sandboxed apps' logs under `runtime_dir`, or
/// `None` when there is no absolute runtime directory to put it in.
#[must_use]
pub fn app_log_dir_in(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    runtime_dir
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(APP_LOG_SUBDIR))
}

/// The private directory for sandboxed apps' logs in this session.
pub fn app_log_dir() -> io::Result<PathBuf> {
    app_log_dir_in(std::env::var_os("XDG_RUNTIME_DIR")).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "XDG_RUNTIME_DIR is not an absolute path; refusing to log app output elsewhere",
        )
    })
}

/// Exclusively create the log for `tab_id` in the private directory `dir`.
///
/// `dir` is created 0700 if missing and must be a real directory owned by
/// this user; the file is created 0600 with `O_EXCL | O_NOFOLLOW`, so a link
/// or file planted at either path is refused rather than followed or reused.
/// Older logs are pruned so at most `MAX_APP_LOGS` remain.
pub fn create_app_log(dir: &Path, tab_id: Uuid) -> io::Result<(PathBuf, File)> {
    tilo_files::native::private::directory(dir)?;
    prune_app_logs(dir, MAX_APP_LOGS.saturating_sub(1));
    let path = dir.join(format!("{tab_id}{LOG_SUFFIX}"));
    let file = tilo_files::native::private::create_new(&path)?;
    Ok((path, file))
}

/// Remove the oldest logs in `dir` until at most `keep` remain. Best effort:
/// a log that cannot be removed only delays its own cleanup.
fn prune_app_logs(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(LOG_SUFFIX))
        .filter_map(|entry| {
            // Of the entry itself: a link is removed, never followed.
            let metadata = entry.path().symlink_metadata().ok()?;
            let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
            Some((modified, entry.path()))
        })
        .collect();
    let excess = logs.len().saturating_sub(keep);
    if excess == 0 {
        return;
    }
    logs.sort();
    for (_, path) in logs.iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// Copy `source` into `log` until `limit` bytes are kept, then note the
/// truncation and keep draining so the app never blocks on a full pipe.
fn copy_capped(mut source: impl Read, mut log: impl Write, limit: u64) -> io::Result<()> {
    let mut kept = 0_u64;
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = match source.read(&mut buffer) {
            Ok(0) => return log.flush(),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if truncated {
            continue;
        }
        let room = usize::try_from(limit.saturating_sub(kept)).unwrap_or(usize::MAX);
        let chunk = buffer.get(..read.min(room)).unwrap_or_default();
        log.write_all(chunk)?;
        kept = kept.saturating_add(chunk.len() as u64);
        if kept >= limit {
            log.write_all(b"\n[tilo] log truncated; further output discarded\n")?;
            log.flush()?;
            truncated = true;
        }
    }
}

/// A sandboxed app and the private log collecting its output.
pub struct SandboxedApp {
    pub child: Child,
    pub log_path: PathBuf,
}

pub struct Container {
    pub project_path: String,
    pub allow_network: bool,
}

impl Container {
    pub fn new(project_path: &str) -> Self {
        Self {
            project_path: project_path.to_string(),
            allow_network: false,
        }
    }

    pub fn build_command(&self, app_command: &str, wayland_socket: &str) -> Command {
        let mut cmd = Command::new("bwrap");

        // 1. Identity & Isolation
        cmd.arg("--unshare-user");
        cmd.arg("--unshare-ipc");
        cmd.arg("--unshare-pid");

        // 2. Base System (Fedora/Bazzite compatibility)
        cmd.arg("--ro-bind").arg("/usr").arg("/usr");
        cmd.arg("--ro-bind").arg("/etc").arg("/etc");
        cmd.arg("--symlink").arg("usr/lib").arg("/lib");
        cmd.arg("--symlink").arg("usr/lib64").arg("/lib64");
        cmd.arg("--symlink").arg("usr/bin").arg("/bin");
        cmd.arg("--symlink").arg("usr/sbin").arg("/sbin");

        if Path::new("/usr/share").exists() {
            cmd.arg("--ro-bind").arg("/usr/share").arg("/usr/share");
        }

        cmd.arg("--dev").arg("/dev");
        cmd.arg("--proc").arg("/proc");

        if Path::new("/sys").exists() {
            cmd.arg("--ro-bind").arg("/sys").arg("/sys");
        }

        if Path::new("/dev/dri").exists() {
            cmd.arg("--dev-bind").arg("/dev/dri").arg("/dev/dri");
        }
        if Path::new("/run/udev").exists() {
            cmd.arg("--ro-bind").arg("/run/udev").arg("/run/udev");
        }
        if Path::new("/dev/shm").exists() {
            cmd.arg("--bind").arg("/dev/shm").arg("/dev/shm");
        }

        // 4. Filesystem Setup
        cmd.arg("--tmpfs").arg("/tmp");
        cmd.arg("--bind").arg(&self.project_path).arg("/project");
        cmd.arg("--chdir").arg("/project");

        // 5. Wayland Socket
        let host_runtime_dir =
            tilo_paths::xdg::runtime_dir().unwrap_or_else(|| "/run/user/1000".into());
        let host_socket_path = format!("{}/{wayland_socket}", host_runtime_dir.display());

        let sandbox_runtime_dir = "/tmp/runtime";
        cmd.arg("--dir").arg(sandbox_runtime_dir);

        if Path::new(&host_socket_path).exists() {
            cmd.arg("--bind")
                .arg(&host_socket_path)
                .arg(format!("{sandbox_runtime_dir}/{wayland_socket}"));
        }

        cmd.env("XDG_RUNTIME_DIR", sandbox_runtime_dir);
        cmd.env("WAYLAND_DISPLAY", wayland_socket);
        cmd.env("XDG_SESSION_TYPE", "wayland");
        cmd.env("WAYLAND_DEBUG", "1");

        // Robust environment for Wayland apps
        cmd.env("QT_QPA_PLATFORM", "wayland");
        cmd.env("GDK_BACKEND", "wayland");
        cmd.env("MOZ_ENABLE_WAYLAND", "1");
        cmd.env("WINIT_UNIX_BACKEND", "wayland");
        cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        cmd.env("XDG_CURRENT_DESKTOP", "tilo");

        if !self.allow_network {
            cmd.arg("--unshare-net");
        }

        cmd.arg("--tmpfs").arg("/home/user");
        cmd.env("HOME", "/home/user");

        // Wrapper to ensure we capture startup errors
        let wrapped_script = format!(
            "echo '[WRAPPER] Date: $(date)'; \
             echo '[WRAPPER] Socket: {wayland_socket}'; \
             ls -l {sandbox_runtime_dir}/{wayland_socket} || echo '[WRAPPER] Socket NOT FOUND'; \
             echo '[WRAPPER] Executing: {app_command}'; \
             exec {app_command} 2>&1"
        );

        cmd.arg("sh").arg("-c").arg(wrapped_script);

        cmd
    }

    /// Spawn `app_command` in the sandbox, logging its output privately to
    /// [`app_log_dir`].
    ///
    /// # Errors
    ///
    /// Returns `Err` if there is no private log directory, the log cannot be
    /// created there (see [`create_app_log`]), or `bwrap` fails to spawn.
    pub fn spawn(
        &self,
        app_command: &str,
        wayland_socket: &str,
        tab_id: Uuid,
    ) -> io::Result<SandboxedApp> {
        self.spawn_logging_to(&app_log_dir()?, app_command, wayland_socket, tab_id)
    }

    /// [`Self::spawn`], with the log in `log_dir` instead of the session's
    /// runtime directory.
    ///
    /// # Errors
    ///
    /// As [`Self::spawn`].
    pub fn spawn_logging_to(
        &self,
        log_dir: &Path,
        app_command: &str,
        wayland_socket: &str,
        tab_id: Uuid,
    ) -> io::Result<SandboxedApp> {
        let mut cmd = self.build_command(app_command, wayland_socket);

        let (log_path, log_file) = create_app_log(log_dir, tab_id)?;
        let (reader, writer) = io::pipe()?;
        cmd.stdout(Stdio::from(writer.try_clone()?));
        cmd.stderr(Stdio::from(writer));
        let child = cmd.spawn()?;
        // Only the child holds the write end now, so the copy ends with it.
        drop(cmd);
        std::thread::Builder::new()
            .name(format!("tilo-app-log-{tab_id}"))
            .spawn(move || {
                if let Err(error) = copy_capped(reader, log_file, MAX_APP_LOG_BYTES) {
                    tracing::warn!(%tab_id, %error, "sandboxed app log stopped");
                }
            })?;

        tracing::debug!(%tab_id, log = %log_path.display(), "spawned sandboxed app");
        Ok(SandboxedApp { child, log_path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn container_logs_are_private_fresh_and_symlink_safe() {
        assert_eq!(
            app_log_dir_in(Some("/run/user/1000".into())),
            Some(PathBuf::from("/run/user/1000/tilo/app-logs"))
        );
        assert_eq!(app_log_dir_in(Some("relative/run".into())), None);
        assert_eq!(app_log_dir_in(None), None);
        {
            let runtime = tempfile::tempdir().unwrap();
            let dir = app_log_dir_in(Some(runtime.path().into())).unwrap();
            let tab_id = Uuid::new_v4();

            let (path, mut file) = create_app_log(&dir, tab_id).unwrap();
            file.write_all(b"hello").unwrap();

            assert_eq!(path, dir.join(format!("{tab_id}.log")));
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(dir.parent().unwrap()), 0o700);
            assert_eq!(mode(&path), 0o600);
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            assert!(metadata.is_file());
            assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        }
        {
            let runtime = tempfile::tempdir().unwrap();
            let dir = runtime.path().join("app-logs");
            std::fs::create_dir(&dir).unwrap();
            let victim = runtime.path().join("victim");
            std::fs::write(&victim, "untouched").unwrap();
            let tab_id = Uuid::new_v4();
            let planted = dir.join(format!("{tab_id}.log"));
            std::os::unix::fs::symlink(&victim, &planted).unwrap();
            let dangling = Uuid::new_v4();
            let dangling_target = runtime.path().join("would-be-created");
            std::os::unix::fs::symlink(&dangling_target, dir.join(format!("{dangling}.log")))
                .unwrap();

            assert!(create_app_log(&dir, tab_id).is_err());
            assert!(create_app_log(&dir, dangling).is_err());
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
            assert!(!dangling_target.exists());
            assert!(planted.symlink_metadata().unwrap().file_type().is_symlink());
        }
        {
            let runtime = tempfile::tempdir().unwrap();
            let elsewhere = runtime.path().join("elsewhere");
            std::fs::create_dir(&elsewhere).unwrap();
            let dir = runtime.path().join("app-logs");
            std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();

            let error = create_app_log(&dir, Uuid::new_v4()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        }
        {
            let runtime = tempfile::tempdir().unwrap();
            let dir = runtime.path().join("app-logs");
            let tab_id = Uuid::new_v4();
            let (path, _) = create_app_log(&dir, tab_id).unwrap();
            std::fs::write(&path, "earlier").unwrap();

            let error = create_app_log(&dir, tab_id).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "earlier");
        }
    }

    #[test]
    fn container_logs_are_pruned_and_capped() {
        {
            let runtime = tempfile::tempdir().unwrap();
            let dir = runtime.path().join("app-logs");
            let mut created: Vec<PathBuf> = Vec::new();
            for age in 0..MAX_APP_LOGS as u64 + 4 {
                let (path, file) = create_app_log(&dir, Uuid::new_v4()).unwrap();
                file.set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + age),
                )
                .unwrap();
                created.push(path);
            }
            std::fs::write(dir.join("unrelated"), "kept").unwrap();

            let remaining: Vec<PathBuf> = created.iter().filter(|p| p.exists()).cloned().collect();
            assert_eq!(remaining.len(), MAX_APP_LOGS);
            assert_eq!(
                remaining.as_slice(),
                &created[created.len() - MAX_APP_LOGS..]
            );
            assert!(dir.join("unrelated").exists());
        }
        {
            let output = vec![b'x'; 10_000];
            let mut log = Vec::new();
            copy_capped(output.as_slice(), &mut log, 4_096).unwrap();

            let (kept, marker) = log.split_at(4_096);
            assert!(kept.iter().all(|b| *b == b'x'));
            assert_eq!(
                marker,
                b"\n[tilo] log truncated; further output discarded\n"
            );

            let mut small = Vec::new();
            copy_capped(&b"short"[..], &mut small, 4_096).unwrap();
            assert_eq!(small, b"short");
        }
    }
}
