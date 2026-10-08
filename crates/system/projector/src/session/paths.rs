//! The projection runtime directory and the files a session keeps in it.

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
    pub(crate) stop: PathBuf,
    pub(crate) ready: PathBuf,
    pub(crate) error: PathBuf,
    pub(crate) pid: PathBuf,
    pub(crate) profile: PathBuf,
}

impl SessionPaths {
    pub(crate) fn resolve() -> Result<Self, ProjectorError> {
        let root = std::env::var_os("TILO_PROJECTOR_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| xdg_paths::xdg::runtime_dir().map(|dir| dir.join("tilo-projector")))
            .ok_or_else(|| {
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
            stop: root.join("stop"),
            ready: root.join("ready"),
            error: root.join("error"),
            pid: root.join("session.pid"),
            profile: root.join("capture-profile.json"),
            root,
        })
    }

    pub(crate) fn clear_markers(&self) {
        for path in [
            &self.video_socket,
            &self.playlist,
            &self.stop,
            &self.ready,
            &self.error,
            &self.pid,
            &self.profile,
        ] {
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
