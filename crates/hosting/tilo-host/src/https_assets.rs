//! Static application assets, separate from the native delivery owner.

use crate::https::Lease;
use bytes::Bytes;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

pub(crate) const READ_BYTES: usize = 32 * 1024;

/// A canonical, operator-selected immutable application artifact directory.
/// The directory is trusted deployment input, never a workspace/state directory.
#[derive(Clone)]
pub struct AssetRoot(Arc<PathBuf>, Arc<Encodings>, Option<Arc<str>>);

pub(crate) struct Asset {
    pub file: AssetFile,
    pub length: u64,
    pub content_type: &'static str,
    /// Requested through the root's content-addressed `/v/<digest>/` prefix:
    /// the bytes can never change for that URL, so it may be cached forever.
    pub immutable: bool,
    /// Validator and optional brotli variant, once the background pass has
    /// hashed this immutable file. Absent until then; the file is still served.
    pub encoded: Option<Encoded>,
}

/// Per-file content validator and precompressed representation. The asset root
/// is immutable deployment input, so both are computed once per process.
#[derive(Clone)]
pub(crate) struct Encoded {
    /// Strong validator of the identity bytes (blake3 prefix), without quotes.
    pub tag: Arc<str>,
    /// Brotli bytes when they are at least 10% smaller than the identity bytes.
    pub brotli: Option<Bytes>,
    length: u64,
}

#[derive(Default)]
struct Encodings {
    started: OnceLock<()>,
    entries: Mutex<HashMap<String, Encoded>>,
}

/// Files above this size are served unencoded; the whole cache is bounded.
const ENCODE_FILE_LIMIT: u64 = 64 * 1024 * 1024;
const ENCODE_CACHE_LIMIT: usize = 192 * 1024 * 1024;

/// The packaged product inventory names its own content digest; the directory
/// is named by it as well. Only that exact digest enables the versioned prefix.
fn version_digest(root: &Path) -> Option<Arc<str>> {
    let name = root.file_name()?.to_str()?;
    let bytes = std::fs::read(root.join("assets.json")).ok()?;
    if bytes.len() > 4 * 1024 * 1024 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let digest = value.get("digest")?.as_str()?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && digest == name)
        .then(|| digest.into())
}

fn compressible(relative: &str) -> Option<u8> {
    // Lower first: the runtime and module graph before fonts and data.
    match Path::new(relative).extension().and_then(|s| s.to_str())? {
        "wasm" => Some(0),
        "js" | "mjs" | "html" | "css" => Some(1),
        "json" | "svg" => Some(2),
        "ttf" | "ttc" | "otf" => Some(3),
        _ => None,
    }
}

/// The descriptor and its charge move together; neither is cloned for reads.
pub(crate) struct AssetFile {
    file: File,
    _lease: Arc<Lease>,
}
// Tuple drop order releases the buffer before the file's final lease, including
// a completed result whose cancelled consumer will never poll its JoinHandle.
pub(crate) type AssetRead = tokio::task::JoinHandle<io::Result<(Vec<u8>, AssetFile)>>;
impl AssetFile {
    pub(crate) fn read_chunk(mut self, length: usize) -> AssetRead {
        tokio::task::spawn_blocking(move || {
            let mut bytes = vec![0; length.min(READ_BYTES)];
            let count = self.file.read(&mut bytes)?;
            bytes.truncate(count);
            // Both a running worker and its unconsumed result retain the charge.
            Ok((bytes, self))
        })
    }
}

impl AssetRoot {
    pub fn open(path: &Path) -> io::Result<Self> {
        let root = path.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::other("asset root is not a directory"));
        }
        let digest = version_digest(&root);
        let root = Self(Arc::new(root), Arc::default(), digest);
        // Encode ahead of the first client: a cold load should find the
        // compressed runtime already prepared.
        root.start_encoding();
        Ok(root)
    }

    /// Starts (once) the background pass that hashes every servable file and
    /// brotli-encodes compressible ones. Requests never wait for it.
    fn start_encoding(&self) {
        if crate::action_pump_cadence::latency_baseline() || self.1.started.set(()).is_err() {
            return;
        }
        let root = self.clone();
        let _ = std::thread::Builder::new()
            .name("qos-asset-encode".into())
            .spawn(move || root.encode_all());
    }

    fn encode_all(&self) {
        let mut files = Vec::new();
        let mut pending = vec![String::new()];
        while let Some(prefix) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(self.0.join(&prefix)) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let relative = format!("{prefix}{name}");
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    pending.push(format!("{relative}/"));
                } else if kind.is_file()
                    && let Some(priority) = compressible(&relative)
                    && let Ok(metadata) = entry.metadata()
                    && metadata.len() <= ENCODE_FILE_LIMIT
                {
                    files.push((priority, metadata.len(), relative));
                }
            }
        }
        files.sort();
        let mut cached = 0usize;
        for (_, _, relative) in files {
            // Same validation as a request (symlinks, canonical containment,
            // content type), but outside any request's byte lease.
            let Ok((mut file, length, _)) = self.open_checked(&relative) else {
                continue;
            };
            let mut bytes = Vec::with_capacity(length as usize);
            if file.read_to_end(&mut bytes).is_err() || bytes.len() as u64 != length {
                continue;
            }
            let tag: Arc<str> = hex::encode(&blake3::hash(&bytes).as_bytes()[..16]).into();
            let mut brotli = None;
            if bytes.len() >= 1024 && cached < ENCODE_CACHE_LIMIT {
                let mut output = Vec::with_capacity(bytes.len() / 3);
                // Quality 5 encodes the ~29 MB runtime in well under a second
                // at a ratio within ~5% of quality 9 (measured 9.2 vs 8.7 MB).
                let params = brotli::enc::BrotliEncoderParams {
                    quality: 5,
                    lgwin: 22,
                    ..Default::default()
                };
                if brotli::BrotliCompress(&mut &bytes[..], &mut output, &params).is_ok()
                    && output.len() * 10 <= bytes.len() * 9
                {
                    cached += output.len();
                    brotli = Some(Bytes::from(output));
                }
            }
            self.1.entries.lock().expect("asset encodings").insert(
                relative,
                Encoded {
                    tag,
                    brotli,
                    length,
                },
            );
        }
    }

    fn encoded(&self, relative: &str, length: u64) -> Option<Encoded> {
        self.1
            .entries
            .lock()
            .ok()?
            .get(relative)
            .filter(|value| value.length == length)
            .cloned()
    }

    pub(crate) async fn resolve(
        &self,
        path: &str,
        navigation: bool,
        lease: Arc<Lease>,
    ) -> io::Result<Asset> {
        if path.len() > 1024
            || !path.starts_with('/')
            || path.contains('%')
            || path.contains('\\')
            || path.bytes().any(|c| !c.is_ascii() || c.is_ascii_control())
        {
            return Err(not_found());
        }
        // `/v/<digest>/<path>` addresses this exact immutable version.
        let (path, immutable) = match path.strip_prefix("/v/") {
            Some(versioned) => {
                let (digest, rest) = versioned.split_once('/').ok_or_else(not_found)?;
                if self.2.as_deref() != Some(digest)
                    || rest.is_empty()
                    || crate::action_pump_cadence::latency_baseline()
                {
                    return Err(not_found());
                }
                (&versioned[digest.len()..], true)
            }
            None => (path, false),
        };
        let navigation = navigation && !immutable;
        let relative = path.trim_start_matches('/');
        if path.starts_with("//")
            || relative
                .split('/')
                .any(|part| part.starts_with('.') || part.is_empty() && !relative.is_empty())
            || matches!(
                relative.split('/').next(),
                Some("peer" | "qos-peer" | "api")
            )
        {
            return Err(not_found());
        }
        let relative = if relative.is_empty() {
            "index.html"
        } else {
            relative
        };
        let relative = relative.to_owned();
        let root = self.clone();
        root.start_encoding();
        tokio::task::spawn_blocking(move || {
            // Cancellation may detach this job. Capture the actual request lease
            // inside it, covering every stat/canonicalize/open and the result.
            let (served, asset) = match root.open_asset(&relative, &lease) {
                Ok(asset) => (relative, asset),
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && navigation
                        && !relative.contains('.') =>
                {
                    (
                        "index.html".to_owned(),
                        root.open_asset("index.html", &lease)?,
                    )
                }
                Err(error) => return Err(error),
            };
            let encoded = root.encoded(&served, asset.length);
            Ok(Asset {
                encoded,
                immutable,
                ..asset
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    fn open_asset(&self, relative: &str, lease: &Arc<Lease>) -> io::Result<Asset> {
        let (file, length, content_type) = self.open_checked(relative)?;
        Ok(Asset {
            file: AssetFile {
                file,
                _lease: lease.clone(),
            },
            length,
            content_type,
            immutable: false,
            encoded: None,
        })
    }

    fn open_checked(&self, relative: &str) -> io::Result<(File, u64, &'static str)> {
        // Do not let a missing script, private key, ready file or unknown file type
        // become an HTML success response. Deployment artifacts stay immutable.
        let content_type = match Path::new(relative).extension().and_then(|s| s.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js" | "mjs") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("wasm") => "application/wasm",
            Some("json") => "application/json",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("webp") => "image/webp",
            Some("gif") => "image/gif",
            Some("ico") => "image/x-icon",
            Some("woff") => "font/woff",
            Some("woff2") => "font/woff2",
            Some("ttf") => "font/ttf",
            _ => return Err(not_found()),
        };
        let mut path = self.0.as_ref().clone();
        for component in relative.split('/') {
            path.push(component);
            if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "asset symlink denied",
                ));
            }
        }
        let canonical = std::fs::canonicalize(&path)?;
        if !canonical.starts_with(self.0.as_path()) {
            return Err(not_found());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let file = options.open(canonical)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(not_found());
        }
        Ok((file, metadata.len(), content_type))
    }
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "asset unavailable")
}

#[cfg(test)]
mod tests;
