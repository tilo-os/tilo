//! Where the bytes come from, and how to reach them.
//!
//! A [`Source`] is a codec-agnostic reference to playable media. It resolves to
//! a `GStreamer` URI for the decode backend, and to a plain byte reader for the
//! prefetch path. Crucially a [`Source::Workspace`] path points **through** the
//! P2P FUSE mount at `/var/workspace` — reading it pulls the bytes down on
//! demand (projection-first, hydration-on-read), so a consumer must never
//! assume the bytes are already local just because the path resolves.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Root of the qOS P2P FUSE filesystem. A [`Source::Workspace`] path is
/// resolved against this (or `$QOS_WORKSPACE_ROOT` when set, for tests / dev).
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
    /// yt-dlp-resolved googlevideo URL. Streamed by the decode backend (its
    /// HTTP source element); the prefetch reader does not fetch it itself.
    Remote { url: String },
}

impl Source {
    /// The FUSE mount root, honouring the `$QOS_WORKSPACE_ROOT` override so a
    /// test / dev host without a real mount can point at a scratch directory.
    pub fn workspace_root() -> PathBuf {
        std::env::var_os("QOS_WORKSPACE_ROOT")
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
    pub fn to_uri(&self) -> String {
        if let Source::Remote { url } = self {
            url.clone()
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

/// Turn a path into a `file://` URI with percent-encoded path segments.
/// Hand-rolled (no `url` dep) because the encoding rules here are narrow: keep
/// the unreserved set + `/`, percent-encode everything else.
///
/// A relative path is resolved against the current directory first. `file://`
/// requires an absolute path — the text after the `//` is a *hostname*, so
/// `file://clips/a.mp4` names a host called `clips`, which every `GStreamer`
/// source element rejects ("the hostname of the URI is invalid"). Resolving
/// here turns a silently unplayable source into a working one.
pub fn path_to_file_uri(path: &Path) -> String {
    let absolute;
    let path = if path.is_absolute() {
        path
    } else {
        absolute = std::env::current_dir().map_or_else(|_| path.to_path_buf(), |d| d.join(path));
        &absolute
    };
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len().saturating_add(8));
    out.push_str("file://");
    for b in s.as_bytes() {
        let c = *b;
        let keep = c.is_ascii_alphanumeric() || matches!(c, b'/' | b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(c as char);
        } else {
            out.push('%');
            out.push(hex_digit(c >> 4));
            out.push(hex_digit(c & 0x0f));
        }
    }
    out
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0'.saturating_add(n)) as char,
        _ => (b'A'.saturating_add(n.saturating_sub(10))) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_becomes_an_absolute_file_uri_not_a_hostname() {
        // `file://clips/a.mp4` parses `clips` as a host and is rejected by
        // every GStreamer source element, so the media never plays.
        let uri = Source::LocalFile(PathBuf::from("clips/a.mp4")).to_uri();
        assert!(
            uri.starts_with("file:///"),
            "must be an absolute file URI, got {uri}"
        );
        assert!(uri.ends_with("/clips/a.mp4"), "path preserved: {uri}");
    }

    #[test]
    fn local_file_uri_is_percent_encoded() {
        let s = Source::LocalFile(PathBuf::from("/home/a/My Clip.mp4"));
        assert_eq!(s.to_uri(), "file:///home/a/My%20Clip.mp4");
        assert!(s.abs_path().unwrap().ends_with("My Clip.mp4"));
    }

    #[test]
    fn remote_uri_passes_through_and_has_no_path() {
        let s = Source::Remote {
            url: "https://cdn.test/v.webm".into(),
        };
        assert_eq!(s.to_uri(), "https://cdn.test/v.webm");
        assert!(s.abs_path().is_none());
        assert!(s.is_ready(), "remote is always ready to stream");
        assert!(s.open_reader().is_err(), "remote is not byte-read here");
    }

    #[test]
    fn workspace_relative_resolves_under_the_mount_root() {
        // Point the mount at a scratch dir so the test needs no real FUSE.
        let dir = std::env::temp_dir().join(format!("qos-media-src-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Safety: single-threaded test mutation of process env.
        unsafe { std::env::set_var("QOS_WORKSPACE_ROOT", &dir) };

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

        unsafe { std::env::remove_var("QOS_WORKSPACE_ROOT") };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workspace_absolute_path_is_left_as_is() {
        let abs = Source::Workspace(PathBuf::from("/var/workspace/media/x.mov"));
        assert_eq!(
            abs.abs_path().unwrap(),
            PathBuf::from("/var/workspace/media/x.mov")
        );
    }
}
