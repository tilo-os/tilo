//! Where the bytes come from, and how to reach them.
//!
//! A [`Source`] resolves to a `GStreamer` URI for the decode backend and to a byte
//! reader for the prefetch path. A [`Source::Workspace`] path points **through** the
//! P2P FUSE mount at `/var/workspace`: reading it pulls the bytes down on demand, so a
//! consumer must never assume they are already local.

use std::io::Read;
use std::path::{Path, PathBuf};

pub const WORKSPACE_ROOT: &str = "/var/workspace";

/// A reference to playable media. Unified across the local disk, the P2P FUSE
/// workspace, and a remote stream URL so the engine and the prefetcher take the
/// same `Source` regardless of where the bytes live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    LocalFile(PathBuf),
    /// A path on the P2P FUSE mount (`/var/workspace`). May be given absolute
    /// (already under the mount) or workspace-relative; both resolve to the
    /// same absolute path. Reading it hydrates the content on demand — the
    /// bytes are **not** assumed local until a read succeeds.
    Workspace(PathBuf),
    /// A directly-playable stream URL — an `http(s)://` progressive file, or a
    /// InnerTube-resolved googlevideo URL. Streamed by the decode backend (its
    /// HTTP source element); the prefetch reader does not fetch it itself.
    Remote {
        url: String,
    },
}

impl Source {
    /// The FUSE mount root, honouring the `$TILO_WORKSPACE_ROOT` override so a
    /// test or development host without a mount can point at a scratch directory.
    pub fn workspace_root() -> PathBuf {
        std::env::var_os("TILO_WORKSPACE_ROOT")
            .map_or_else(|| PathBuf::from(WORKSPACE_ROOT), PathBuf::from)
    }

    /// The source for an absolute path: one under the FUSE mount becomes a
    /// [`Source::Workspace`] (read *through* the mount so it hydrates on
    /// demand), anything else a [`Source::LocalFile`].
    pub fn for_path(abs_path: &Path) -> Source {
        if abs_path.starts_with(Self::workspace_root()) || abs_path.starts_with(WORKSPACE_ROOT) {
            Source::Workspace(abs_path.to_path_buf())
        } else {
            Source::LocalFile(abs_path.to_path_buf())
        }
    }

    pub fn for_uri(uri: &str) -> Source {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = url::Url::parse(uri)
            .ok()
            .filter(|url| url.scheme() == "file")
            .and_then(|url| url.to_file_path().ok())
        {
            return Self::for_path(&path);
        }
        Source::Remote {
            url: uri.to_owned(),
        }
    }

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

    pub fn is_ready(&self) -> bool {
        match self {
            Source::Remote { .. } => true,
            #[cfg(target_arch = "wasm32")]
            _ => false,
            #[cfg(not(target_arch = "wasm32"))]
            _ => self.abs_path().is_some_and(|p| p.exists()),
        }
    }

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

    pub fn byte_len(&self) -> Option<u64> {
        let abs = self.abs_path()?;
        std::fs::metadata(abs).ok().map(|m| m.len())
    }

    pub fn from_contents(name: &str, bytes: &[u8]) -> std::io::Result<Source> {
        let extension = Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        contents_source(&extension, bytes)
    }
}

#[cfg(web_page)]
fn mime_for_extension(extension: &str) -> &'static str {
    match extension {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

#[cfg(web_page)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "shares the signature of the native variant, which writes a file"
)]
fn contents_source(extension: &str, bytes: &[u8]) -> std::io::Result<Source> {
    Ok(Source::Remote {
        url: crate::web::blob_url(bytes, mime_for_extension(extension)),
    })
}

#[cfg(not(web_page))]
fn contents_source(extension: &str, bytes: &[u8]) -> std::io::Result<Source> {
    let hash = blake3::hash(bytes).to_hex();
    let mut path =
        std::env::temp_dir().join(format!("tilo-media-{}", hash.get(..32).unwrap_or_default()));
    if !extension.is_empty() {
        path.set_extension(extension);
    }
    if std::fs::metadata(&path).map(|m| m.len()).ok() != Some(bytes.len() as u64) {
        let partial = path.with_extension("partial");
        std::fs::write(&partial, bytes)?;
        std::fs::rename(&partial, &path)?;
    }
    Ok(Source::LocalFile(path))
}

#[cfg(any(unix, windows))]
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

#[cfg(any(unix, windows))]
pub fn file_uri_to_path(uri: &str) -> std::io::Result<PathBuf> {
    let invalid = |message: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    url::Url::parse(uri)
        .map_err(|error| invalid(format!("Invalid file URI: {error}")))?
        .to_file_path()
        .map_err(|()| invalid("The file URI does not name a local file".into()))
}

#[cfg(not(any(unix, windows)))]
pub fn file_uri_to_path(_uri: &str) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "file URIs name no file on this host",
    ))
}

#[cfg(not(any(unix, windows)))]
pub fn path_to_file_uri(_path: &Path) -> std::io::Result<String> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "media paths have no file URL on this host",
    ))
}
