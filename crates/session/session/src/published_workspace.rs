//! The session's `workspace-id` file: the workspace in front, for a command started afterwards to
//! join.
//!
//! An app the launcher starts is told which workspace it opens in; one a person runs themselves —
//! from a terminal, by `tilo-app dev` — has no launcher to tell it, so it reads the workspace the
//! person is looking at ([`tilo_paths::login::WORKSPACE_ID_FILE`]) and joins it: its windows land
//! beside theirs rather than in none, and its file chooser is the one that workspace grants.
//!
//! Only a workspace switch changes what the file holds, and the pass that noticed the switch hands
//! over the id ([`Published::follow`]): nothing here reads the desk on its own clock. The file is
//! written without syncing — it lives in the runtime directory, which a crash empties, and a sync
//! of the file and its directory is a journal commit the frame loop cannot spare on a busy disk.

use std::path::{Path, PathBuf};

use tilo_desktop_model::WorkspaceId;

use crate::Session;

impl Session {
    /// Publish the workspace in front for as long as this session runs, once it serves `display`:
    /// the login's programs then join the workspace the person is looking at
    /// ([`Published::for_display`] decides whether this session is the one they reach).
    pub fn publish_workspace(&mut self, display: &str) {
        self.published_workspace = Published::for_display(display);
    }
}

/// The workspace in front as the login's programs read it, and where it is written.
pub(crate) struct Published {
    /// The file; `None` when this session is not the one the login's programs reach, whose
    /// workspace is therefore not this session's to name.
    file: Option<PathBuf>,
    /// The id the file holds now.
    written: Option<WorkspaceId>,
}

impl Published {
    /// Publishing nothing: a session whose display another running session published, which is
    /// the session a command started beside it joins.
    pub(crate) const fn none() -> Self {
        Self {
            file: None,
            written: None,
        }
    }

    /// The file for the session whose Wayland socket is `display`, when
    /// [`tilo_paths::login::WAYLAND_DISPLAY_FILE`] names that socket: the session the login's own
    /// programs find is the one that publishes.
    pub(crate) fn for_display(display: &str) -> Self {
        Self::for_display_in(std::env::var_os("XDG_RUNTIME_DIR"), display)
    }

    /// [`Self::for_display`] for the values it reads, so what it decides can be checked.
    fn for_display_in(runtime_dir: Option<std::ffi::OsString>, display: &str) -> Self {
        let Some(directory) = tilo_paths::login::session_dir(runtime_dir) else {
            return Self::none();
        };
        let reachable = std::fs::read(directory.join(tilo_paths::login::WAYLAND_DISPLAY_FILE))
            .is_ok_and(|published| published == display.as_bytes());
        if !reachable {
            return Self::none();
        }
        Self {
            file: Some(directory.join(tilo_paths::login::WORKSPACE_ID_FILE)),
            written: None,
        }
    }

    /// Name `workspace` from now on: the file reads its id, or is gone while the desk shows no
    /// workspace at all. Returns at once when nothing changed.
    pub(crate) fn follow(&mut self, workspace: Option<WorkspaceId>) {
        if self.written == workspace {
            return;
        }
        let Some(file) = &self.file else {
            return;
        };
        let result = match workspace {
            Some(id) => publish(file, id.to_string().as_bytes()),
            None => match std::fs::remove_file(file) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            },
        };
        match result {
            Ok(()) => self.written = workspace,
            // Not retried on its own: the next switch tries again.
            Err(error) => {
                tracing::warn!(%error, file = %file.display(), "the workspace in front is not published");
            }
        }
    }
}

impl Drop for Published {
    fn drop(&mut self) {
        // Only the id this session published: a session that took the file over is the one the
        // login's commands join now.
        let (Some(file), Some(written)) = (&self.file, self.written) else {
            return;
        };
        if std::fs::read(file).is_ok_and(|held| held == written.to_string().as_bytes()) {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// Replace `file` with `bytes`, creating the session directory 0700 when it is not there yet
/// (the session's other sockets share it) and the file 0600: not a secret, but nothing else
/// should write it.
fn publish(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    if let Some(directory) = file.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
    }
    tilo_files::native::atomic::write_regenerable_with_mode(file, bytes, 0o600)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    /// A runtime directory of this test's own, with the display file a session publishes.
    fn runtime(display: Option<&str>) -> tempfile::TempDir {
        let runtime = tempfile::tempdir().expect("temp dir");
        if let Some(display) = display {
            let session = runtime.path().join(tilo_paths::login::SESSION_DIR);
            std::fs::create_dir_all(&session).expect("session directory");
            std::fs::write(
                session.join(tilo_paths::login::WAYLAND_DISPLAY_FILE),
                display,
            )
            .expect("display file");
        }
        runtime
    }

    fn id(n: u8) -> WorkspaceId {
        WorkspaceId::from(uuid::Uuid::from_bytes([n; 16]))
    }

    fn held(published: &Published) -> Option<String> {
        let file = published.file.as_ref()?;
        std::fs::read_to_string(file).ok()
    }

    #[test]
    fn the_workspace_in_front_is_published_for_the_session_the_login_reaches() {
        let outside = runtime(Some("wayland-1"));
        let mut published =
            Published::for_display_in(Some(outside.path().as_os_str().to_owned()), "wayland-1");
        assert_eq!(held(&published), None, "nothing before the first switch");
        published.follow(Some(id(1)));
        assert_eq!(
            held(&published).as_deref(),
            Some(id(1).to_string().as_str())
        );
        let file = published.file.clone().expect("a published file");
        assert_eq!(
            std::fs::metadata(&file)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the workspace is the session's alone"
        );
        // A switch replaces it in place, and no workspace removes it.
        published.follow(Some(id(2)));
        assert_eq!(
            held(&published).as_deref(),
            Some(id(2).to_string().as_str())
        );
        published.follow(None);
        assert!(!file.exists(), "no workspace in front, no file");
        // The session's end removes what it wrote.
        published.follow(Some(id(3)));
        drop(published);
        assert!(!file.exists(), "the session's end removes the file");
    }

    #[test]
    fn a_session_whose_display_the_login_does_not_reach_publishes_nothing() {
        let outside = runtime(Some("wayland-1"));
        // A nested session beside a live one: the login's programs reach that one's display.
        let mut beside =
            Published::for_display_in(Some(outside.path().as_os_str().to_owned()), "wayland-2");
        beside.follow(Some(id(1)));
        assert_eq!(beside.file, None);
        assert_eq!(held(&beside), None);
        // A session whose display nobody published yet names no workspace either.
        let fresh = runtime(None);
        let mut unpublished =
            Published::for_display_in(Some(fresh.path().as_os_str().to_owned()), "wayland-1");
        unpublished.follow(Some(id(1)));
        assert_eq!(unpublished.file, None);
        assert_eq!(held(&unpublished), None);
    }

    #[test]
    fn a_sessions_end_keeps_a_workspace_another_session_published() {
        let outside = runtime(Some("wayland-1"));
        let mut published =
            Published::for_display_in(Some(outside.path().as_os_str().to_owned()), "wayland-1");
        published.follow(Some(id(1)));
        let file = published.file.clone().expect("a published file");
        // A session after it takes the file over for its own desktop.
        std::fs::write(&file, id(9).to_string()).expect("the later session's workspace");
        drop(published);
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            id(9).to_string(),
            "the session that took it over keeps it"
        );
    }
}
