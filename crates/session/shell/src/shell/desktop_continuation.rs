//! Read-only readiness for the installed native folder importer. Identity
//! provisioning stays in trusted shell chrome; this service exposes no keys.
//! The socket belongs to this Wayland socket inode and is never unlinked on
//! startup. A crashed import process leaves the saved continuation untouched.
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

/// Registry identity of the Desktop folder continuation, as produced by the
/// XDG scan; the installed file itself retains its `.desktop` suffix.
pub(crate) const LAUNCH_DESKTOP_ID: &str = "tilo-import-folders";

/// The ordinary host-app request queued after trusted onboarding completes.
///
/// # Errors
///
/// [`crate::shell::launcher::LaunchError::HostAppNotFound`] when the entry is
/// not installed on this host.
pub(crate) fn launch_request()
-> Result<crate::shell::host_apps::QueuedLaunch, crate::shell::launcher::LaunchError> {
    let app = crate::shell::host_apps::find(LAUNCH_DESKTOP_ID).ok_or_else(|| {
        crate::shell::launcher::LaunchError::HostAppNotFound {
            desktop_id: LAUNCH_DESKTOP_ID.to_string(),
        }
    })?;
    Ok(crate::shell::host_apps::QueuedLaunch {
        app,
        placement: crate::shell::tab_binding::Placement::NewGroup,
        portal_workspace_id: None,
        requested_at: std::time::Instant::now(),
    })
}

#[derive(Default)]
struct LaunchLatch {
    queued: bool,
}
impl LaunchLatch {
    fn should_queue(&mut self, onboarding_active: bool, pending: bool) -> bool {
        if onboarding_active || !pending || self.queued {
            return false;
        }
        self.queued = true;
        true
    }
}

pub(crate) struct SessionContinuation {
    listener: UnixListener,
    socket_path: PathBuf,
    socket_identity: fs::Metadata,
    runtime: fs::File,
    runtime_path: PathBuf,
    latch: LaunchLatch,
    pending: bool,
}

impl SessionContinuation {
    pub(crate) fn bind(
        runtime: &Path,
        display: &OsStr,
        uid: u32,
        pending: bool,
    ) -> io::Result<Self> {
        let runtime_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(runtime)?;
        let metadata = runtime_file.metadata()?;
        if !runtime.is_absolute() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "import readiness requires a private user runtime directory",
            ));
        }
        let bytes = display.as_bytes();
        if bytes.is_empty() || bytes.len() > 32 || bytes.contains(&b'/') {
            return Err(io::Error::other("invalid Wayland display name"));
        }
        let wayland = fs::symlink_metadata(runtime.join(display))?;
        if !wayland.file_type().is_socket() || wayland.uid() != uid {
            return Err(io::Error::other("invalid Wayland socket identity"));
        }
        let key = hex::encode(bytes);
        let socket_path = runtime.join(format!("tilo-import-{key}-{}.sock", wayland.ino()));
        if socket_path.as_os_str().as_bytes().len() >= 104 {
            return Err(io::Error::other("import readiness socket path too long"));
        }
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let socket_identity = fs::symlink_metadata(&socket_path)?;
        Ok(Self {
            listener,
            socket_path,
            socket_identity,
            runtime: runtime_file,
            runtime_path: runtime.to_owned(),
            latch: LaunchLatch::default(),
            pending,
        })
    }

    /// Bounded nonblocking pump: no client can stall a compositor frame. This
    /// response is generated from the trusted current state, never a CLI flag.
    pub(crate) fn pump(&mut self, onboarding_active: bool) -> bool {
        if !self.runtime_unchanged() {
            return false;
        }
        for _ in 0..8 {
            let Ok((mut client, _)) = self.listener.accept() else {
                break;
            };
            if client.set_nonblocking(true).is_err() {
                continue;
            }
            let response = if onboarding_active {
                b"tilo-import-wait--v1\n"
            } else {
                b"tilo-import-ready-v1\n"
            };
            let _ = client.write_all(response);
        }
        self.latch.should_queue(onboarding_active, self.pending)
    }

    // Keep the POSIX group/other permission mask explicit for this security check.
    #[allow(
        clippy::verbose_bit_mask,
        reason = "The explicit POSIX 0o077 mask verifies that the pinned runtime directory grants no group or other permissions."
    )]
    fn runtime_unchanged(&self) -> bool {
        match (
            self.runtime.metadata(),
            fs::symlink_metadata(&self.runtime_path),
        ) {
            (Ok(pinned), Ok(current)) => {
                current.is_dir()
                    && !current.file_type().is_symlink()
                    && current.uid() == pinned.uid()
                    && current.mode() & 0o077 == 0
                    && current.dev() == pinned.dev()
                    && current.ino() == pinned.ino()
            }
            _ => false,
        }
    }
}

impl Drop for SessionContinuation {
    fn drop(&mut self) {
        if self.runtime_unchanged()
            && fs::symlink_metadata(&self.socket_path).is_ok_and(|current| {
                current.file_type().is_socket()
                    && current.dev() == self.socket_identity.dev()
                    && current.ino() == self.socket_identity.ino()
            })
        {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

/// This is only a launch hint. The importer independently opens private state
/// under its exclusive store lock and validates the complete document.
pub(crate) fn pending_review(path: &Path, uid: u32) -> bool {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    else {
        return false;
    };
    let Ok(m) = file.metadata() else {
        return false;
    };
    if !m.is_file() || m.uid() != uid || m.mode() & 0o077 != 0 || m.len() > 256 * 1024 {
        return false;
    }
    let mut bytes = Vec::new();
    if file.take(256 * 1024 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > 256 * 1024 {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("show_on_login").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn desktop_continuations_resolve_launch_once_and_verify_readiness() {
        fn continuation_request_resolves_the_installed_desktop_entry() {
            let temp = tempfile::tempdir().unwrap();
            let executable = temp.path().join("tilo-import-folders");
            fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(
                temp.path().join("tilo-import-folders.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName=Add folders to Tilo\n\
                     Exec={} --import-folders\nTryExec={}\n",
                    executable.display(),
                    executable.display(),
                ),
            )
            .unwrap();

            let scan = desktop_entry::scan_dirs(
                &[temp.path().to_path_buf()],
                &[desktop_entry::scan::CURRENT_DESKTOP.to_string()],
            );
            let request_id = LAUNCH_DESKTOP_ID;

            assert_eq!(scan.stats.files_seen, 1);
            assert!(
                scan.find(request_id).is_some(),
                "continuation launch id {:?} must resolve against registry ids {:?}",
                request_id,
                scan.entries
                    .iter()
                    .map(|entry| entry.id.as_str())
                    .collect::<Vec<_>>()
            );
        }

        fn trusted_completion_launches_once_and_never_acks_durable_state() {
            let mut latch = LaunchLatch::default();
            assert!(!latch.should_queue(true, true));
            assert!(!latch.should_queue(false, false));
            assert!(latch.should_queue(false, true));
            assert!(!latch.should_queue(false, true));
            assert!(LaunchLatch::default().should_queue(false, true));
        }

        fn readiness_socket_reports_wait_then_ready_and_refuses_duplicate_or_substituted_runtime() {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
            let uid = fs::metadata(root).unwrap().uid();
            let _wayland = UnixListener::bind(root.join("wayland-1")).unwrap();
            let mut gate =
                SessionContinuation::bind(root, OsStr::new("wayland-1"), uid, true).unwrap();
            assert!(SessionContinuation::bind(root, OsStr::new("wayland-1"), uid, true).is_err());
            let mut client = UnixStream::connect(&gate.socket_path).unwrap();
            assert!(!gate.pump(true));
            let mut bytes = [0; 21];
            client.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"tilo-import-wait--v1\n");
            let mut client = UnixStream::connect(&gate.socket_path).unwrap();
            assert!(gate.pump(false));
            client.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"tilo-import-ready-v1\n");
            assert!(!gate.pump(false));
            assert!(SessionContinuation::bind(root, OsStr::new("../escape"), uid, true).is_err());
            fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(!gate.pump(false));
            assert!(SessionContinuation::bind(root, OsStr::new("wayland-1"), uid, true).is_err());
            fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        }

        continuation_request_resolves_the_installed_desktop_entry();
        trusted_completion_launches_once_and_never_acks_durable_state();
        readiness_socket_reports_wait_then_ready_and_refuses_duplicate_or_substituted_runtime();
    }
}
