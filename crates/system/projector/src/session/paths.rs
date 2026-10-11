//! The projection runtime directory and the media files a session keeps in
//! it: the compositor's video socket, the HLS playlist and its segments.
//!
//! Session state (readiness, errors, stop requests, the capture profile)
//! travels as control-socket events, never as files here.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::ProjectorError;

#[derive(Clone)]
pub(crate) struct SessionPaths {
    pub(crate) root: PathBuf,
    pub(crate) video_socket: PathBuf,
    pub(crate) playlist: PathBuf,
}

impl SessionPaths {
    pub(crate) fn resolve() -> Result<Self, ProjectorError> {
        let root = tilo_projector::control::runtime_dir().ok_or_else(|| {
            ProjectorError::Unavailable("XDG_RUNTIME_DIR is unavailable".to_owned())
        })?;
        Self::for_root(root)
    }

    pub(crate) fn for_root(root: PathBuf) -> Result<Self, ProjectorError> {
        fs::create_dir_all(&root).map_err(ProjectorError::io(
            "cannot create projection runtime directory",
        ))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(
            ProjectorError::io("cannot secure projection runtime directory"),
        )?;
        Ok(Self {
            video_socket: root.join("video.sock"),
            playlist: root.join("live.m3u8"),
            root,
        })
    }

    /// Removes the previous session's media files. The control socket stays.
    pub(crate) fn clear(&self) {
        for path in [&self.video_socket, &self.playlist] {
            let _ = fs::remove_file(path);
        }
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_segment =
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("segment-")
                                && Path::new(name).extension() == Some(OsStr::new("ts"))
                        });
                if is_segment {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
}
