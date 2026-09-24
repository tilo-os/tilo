//! Where the bytes come from, and how to reach them.
//!
//! A [`Source`] is a codec-agnostic reference to playable media. It resolves to
//! a `GStreamer` URI for the decode backend, and to a plain byte reader for the
//! prefetch path. A [`Source::Workspace`] path points **through** the
//! P2P FUSE mount at `/var/workspace` — reading it pulls the bytes down on
//! demand (projection-first, hydration-on-read), so a consumer must never
//! assume the bytes are already local just because the path resolves.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Root of the Tilo P2P FUSE filesystem. A [`Source::Workspace`] path is
/// resolved against this (or `$TILO_WORKSPACE_ROOT` when set for tests and development).
pub const WORKSPACE_ROOT: &str = "/var/workspace";

/// A reference to playable media. Unified across the local disk, the P2P FUSE
/// workspace, and a remote stream URL so the engine and the prefetcher take the
/// same `Source` regardless of where the bytes live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An ordinary file on the local disk.
    LocalFile(PathBuf),
    /// A path on the P2P FUSE mount (`/var/workspace`). May be given absolute
    /// (already under the mount) or workspace-relative; both resolve to the
    /// same absolute path. Reading it hydrates the content on demand — the
    /// bytes are **not** assumed local until a read succeeds.
    Workspace(PathBuf),
    /// A directly-playable stream URL — an `http(s)://` progressive file, or a
    /// InnerTube-resolved googlevideo URL. Streamed by the decode backend (its
    /// HTTP source element); the prefetch reader does not fetch it itself.
    Remote { url: String },
}

impl Source {
    /// The FUSE mount root, honouring the `$TILO_WORKSPACE_ROOT` override so a
    /// test or development host without a mount can point at a scratch directory.
    pub fn workspace_root() -> PathBuf {
        std::env::var_os("TILO_WORKSPACE_ROOT")
            .map_or_else(|| PathBuf::from(WORKSPACE_ROOT), PathBuf::from)
    }

    /// The absolute on-disk path these bytes resolve to, for the local and
    /// workspace variants. `None` for [`Source::Remote`] (no local path).
    pub fn abs_path(&self) -> Option<PathBuf> {
        match self {
            Source::LocalFile(p) => Some(p.clone()),
            Source::Workspace(p) => {
                if p.is_absolute() {
                    Some(p.clone())
                } else {
                    Some(Self::workspace_root().join(p))
                }
            }
            Source::Remote { .. } => None,
        }
    }

    /// Whether the content can be read/decoded right now. Remote is always
    /// "ready" (the network fetch happens in the pipeline); a file variant is
    /// ready once its resolved path exists on disk — for a workspace path that
    /// means the FUSE has hydrated (or is projecting a cached) file.
    pub fn is_ready(&self) -> bool {
        match self {
            Source::Remote { .. } => true,
            _ => self.abs_path().is_some_and(|p| p.exists()),
        }
    }

    /// The URI to hand a `GStreamer` `playbin`. File variants become `file://`
    /// URIs on their resolved absolute path; a remote source passes through.
    ///
    /// # Errors
    /// Returns an error if a local path cannot be resolved or represented as a file URL.
    pub fn to_uri(&self) -> std::io::Result<String> {
        if let Source::Remote { url } = self {
            Ok(url.clone())
        } else {
            let abs = self.abs_path().unwrap_or_default();
            path_to_file_uri(&abs)
        }
    }

    /// Open a blocking byte reader over the resolved bytes, for the prefetch
    /// path. File variants read through `std::fs` (which, for a workspace path,
    /// is the FUSE mount — the read is what triggers hydration). Remote returns
    /// an error: it is streamed by the decode pipeline, not byte-copied here.
    ///
    /// # Errors
    ///
    /// Returns an error for a [`Source::Remote`] (streamed elsewhere) or if the
    /// resolved file cannot be opened.
    pub fn open_reader(&self) -> std::io::Result<Box<dyn Read + Send>> {
        if let Source::Remote { .. } = self {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "remote sources are streamed by the decode pipeline, not read here",
            ));
        }
        let abs = self.abs_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "source has no local path to read",
            )
        })?;
        let f = std::fs::File::open(abs)?;
        Ok(Box::new(f))
    }

    /// The total byte length of a file source, if known (a stat on the mount).
    /// `None` for remote streams and unhydrated / missing files.
    pub fn byte_len(&self) -> Option<u64> {
        let abs = self.abs_path()?;
        std::fs::metadata(abs).ok().map(|m| m.len())
    }
}

/// Turn a native filesystem path into a percent-encoded file URL.
/// Resolving lexically avoids requiring the file to be hydrated or even exist.
///
/// # Errors
/// Returns an error when the current directory cannot be resolved or the path
/// names a host device that cannot be represented as a file URL.
pub fn path_to_file_uri(path: &Path) -> std::io::Result<String> {
    let absolute = std::path::absolute(path)?;
    url::Url::from_file_path(absolute)
        .map(String::from)
        .map_err(|()| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "media path cannot be represented as a file URL",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_becomes_an_absolute_file_uri_not_a_hostname() {
        // `file://clips/a.mp4` parses `clips` as a host and is rejected by
        // every GStreamer source element, so the media never plays.
        let uri = Source::LocalFile(PathBuf::from("clips/a.mp4"))
            .to_uri()
            .unwrap();
        assert!(
            uri.starts_with("file:///"),
            "must be an absolute file URI, got {uri}"
        );
        assert!(uri.ends_with("/clips/a.mp4"), "path preserved: {uri}");
    }

    #[test]
    #[cfg(unix)]
    fn local_file_uri_is_percent_encoded() {
        let s = Source::LocalFile(PathBuf::from("/home/a/My Clip.mp4"));
        assert_eq!(s.to_uri().unwrap(), "file:///home/a/My%20Clip.mp4");
        assert!(s.abs_path().unwrap().ends_with("My Clip.mp4"));
    }

    #[test]
    #[cfg(windows)]
    fn windows_drive_unc_and_verbatim_paths_become_file_urls() {
        for (path, expected) in [
            (
                r"C:\Media\日本語.webm",
                "file:///C:/Media/%E6%97%A5%E6%9C%AC%E8%AA%9E.webm",
            ),
            (
                r"C:\Media\My Clip #1%.mp4",
                "file:///C:/Media/My%20Clip%20%231%25.mp4",
            ),
            (
                r"\\server\share\My Clip.mp4",
                "file://server/share/My%20Clip.mp4",
            ),
            (
                r"\\?\C:\Media\My Clip.mp4",
                "file:///C:/Media/My%20Clip.mp4",
            ),
        ] {
            assert_eq!(path_to_file_uri(Path::new(path)).unwrap(), expected);
        }
    }

    #[test]
    #[cfg(windows)]
    fn windows_device_paths_are_rejected_as_media_urls() {
        let error = path_to_file_uri(Path::new(r"\\.\COM1")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    #[cfg(unix)]
    fn unix_file_url_preserves_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/media/clip\xff #%.mp4"));
        let uri = path_to_file_uri(path).unwrap();
        assert_eq!(uri, "file:///media/clip%FF%20%23%25.mp4");
        assert_eq!(url::Url::parse(&uri).unwrap().to_file_path().unwrap(), path);
    }

    #[test]
    fn remote_uri_passes_through_and_has_no_path() {
        let s = Source::Remote {
            url: "https://cdn.test/v.webm".into(),
        };
        assert_eq!(s.to_uri().unwrap(), "https://cdn.test/v.webm");
        assert!(s.abs_path().is_none());
        assert!(s.is_ready(), "remote is always ready to stream");
        assert!(s.open_reader().is_err(), "remote is not byte-read here");
    }

    #[test]
    fn workspace_relative_resolves_under_the_mount_root() {
        // Point the mount at a scratch dir so the test needs no real FUSE.
        let dir = std::env::temp_dir().join(format!("tilo-media-src-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ROOT", &dir);

        let rel = Source::Workspace(PathBuf::from("clips/a.mp4"));
        assert_eq!(rel.abs_path().unwrap(), dir.join("clips/a.mp4"));
        assert!(!rel.is_ready(), "not hydrated yet — the byte is not local");

        // Hydrate it (write the file) and it reports ready + readable.
        std::fs::create_dir_all(dir.join("clips")).unwrap();
        std::fs::write(dir.join("clips/a.mp4"), b"hello bytes").unwrap();
        assert!(rel.is_ready());
        assert_eq!(rel.byte_len(), Some(11));
        let mut buf = Vec::new();
        rel.open_reader().unwrap().read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"hello bytes");

        drop(env);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workspace_absolute_path_is_left_as_is() {
        let path = std::env::temp_dir().join("workspace/media/x.mov");
        assert!(path.is_absolute());
        let abs = Source::Workspace(path.clone());
        assert_eq!(abs.abs_path().unwrap(), path);
    }
}
