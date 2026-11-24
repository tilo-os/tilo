//! A window's remote pictures on this device: the bytes kept from an earlier look, else fetched
//! and kept, under a directory the app's own launch grants it.
//!
//! Two places, because they are two promises. What the window looked at is a cache: bounded, and
//! the oldest of it goes as room is needed. What the person saved for offline use is theirs until
//! they clear it: kept whole, and never evicted by the cache's bound, so a trip with no network
//! shows the area they chose. A picture asked for reads the saved one first, then the cache, then
//! the network.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tilo_net::http;

/// The most the pictures a window looked at may take on this device: beyond it the least recently
/// looked at go. What the person saved is kept outside this bound.
pub const CACHE_BYTES: u64 = 256 * 1024 * 1024;

/// A request's longest wait: a tile server that does not answer in this time is not reachable
/// from here now, and the map draws what it has.
const TIMEOUT: Duration = Duration::from_secs(15);

/// The largest single picture the cache keeps: a tile is a few tens of kilobytes, and anything
/// that is not one is not a map's picture.
pub const MOST_BYTES: u64 = 4 * 1024 * 1024;

/// The tile servers' own usage policy asks a client to say who it is.
pub const USER_AGENT: &str = concat!(
    "tilo/",
    env!("CARGO_PKG_VERSION"),
    " (Tilo maps; +https://tilo.sh)"
);

/// The pictures one window shows from the network, kept under `root`.
pub struct Tiles {
    root: PathBuf,
    client: Result<http::BlockingClient, String>,
}

impl Tiles {
    /// The cache a window's pictures are kept in, under the app's own cache directory `root`.
    #[must_use]
    pub fn open(root: PathBuf) -> Self {
        let client = http::blocking_client()
            .timeout(TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| error.to_string());
        Self { root, client }
    }

    /// A window's pictures under the app's cache directory called `app`, which `AppDirs` names
    /// (`tilo_paths::AppDirs::new("maps")`).
    #[must_use]
    pub fn of_app(app: &str) -> Option<Self> {
        let root = tilo_paths::AppDirs::new(app).cache()?;
        Some(Self::open(root))
    }

    /// The bytes of the picture at `url`: what the person saved for offline use, the copy an
    /// earlier look kept, or the network's — which it keeps for the next look. `None` where
    /// nothing reaches it, which the window draws as a picture still on its way.
    #[must_use]
    pub fn bytes(&self, url: &str) -> Option<Vec<u8>> {
        if let Some(bytes) = read(&Self::path_in(&self.saved_dir(), url)) {
            return Some(bytes);
        }
        let cached = Self::path_in(&self.cache_dir(), url);
        if let Some(bytes) = read(&cached) {
            touch(&cached);
            return Some(bytes);
        }
        let bytes = self.fetch(url)?;
        self.keep_in(&cached, &bytes);
        Some(bytes)
    }

    /// Fetch the picture at `url` and keep it for the person's offline use, answering its bytes.
    /// What `keep_area` asks per tile of an area; also what a person opening a picture asks for.
    pub fn save(&self, url: &str) -> Option<Vec<u8>> {
        let saved = Self::path_in(&self.saved_dir(), url);
        if let Some(bytes) = read(&saved) {
            return Some(bytes);
        }
        let bytes = match read(&Self::path_in(&self.cache_dir(), url)) {
            Some(bytes) => bytes,
            None => self.fetch(url)?,
        };
        self.keep_in(&saved, &bytes);
        Some(bytes)
    }

    /// How many bytes the saved pictures take.
    #[must_use]
    pub fn saved_bytes(&self) -> u64 {
        directory_bytes(&self.saved_dir())
    }

    /// Give up every picture saved for offline use, answering how many bytes that freed.
    pub fn clear_saved(&self) -> u64 {
        let dir = self.saved_dir();
        let freed = directory_bytes(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        freed
    }

    /// Give up every picture the window looked at, answering how many bytes that freed.
    pub fn clear_cache(&self) -> u64 {
        let dir = self.cache_dir();
        let freed = directory_bytes(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        freed
    }

    fn fetch(&self, url: &str) -> Option<Vec<u8>> {
        let client = self.client.as_ref().map_err(Clone::clone).ok()?;
        let answer = client.get(url).send().ok()?;
        if !answer.status().is_success() {
            tracing::debug!(url, status = answer.status().as_u16(), "a picture did not come");
            return None;
        }
        let bytes = answer.bytes().ok()?.to_vec();
        if bytes.is_empty() || bytes.len() as u64 > MOST_BYTES {
            return None;
        }
        Some(bytes)
    }

    fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    fn saved_dir(&self) -> PathBuf {
        self.root.join("offline")
    }

    /// Where a picture's bytes are kept: named by what the address is, never by it as a path,
    /// since an address holds characters and lengths a file name may not.
    fn path_in(dir: &Path, url: &str) -> PathBuf {
        dir.join(format!("{}.img", blake3::hash(url.as_bytes()).to_hex()))
    }

    fn keep_in(&self, path: &Path, bytes: &[u8]) {
        if path
            .parent()
            .is_some_and(|dir| std::fs::create_dir_all(dir).is_err())
        {
            return;
        }
        // Written beside and moved into place, so a window reading while another writes never
        // reads half a picture.
        let beside = path.with_extension("part");
        if std::fs::write(&beside, bytes).is_err() || std::fs::rename(&beside, path).is_err() {
            let _ = std::fs::remove_file(&beside);
            return;
        }
        self.evict();
    }

    /// Go past the cache's bound, least recently looked at first. What was saved for offline use
    /// lives elsewhere and is never touched here.
    fn evict(&self) {
        let dir = self.cache_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut kept: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        let mut total = 0_u64;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            total = total.saturating_add(meta.len());
            kept.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len(), path));
        }
        if total <= CACHE_BYTES {
            return;
        }
        kept.sort_by_key(|(used, _, _)| *used);
        for (_, size, path) in kept {
            if total <= CACHE_BYTES {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
}

/// A kept picture's bytes, when it is there and whole.
fn read(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok().filter(|bytes| !bytes.is_empty())
}

/// Say a kept picture was looked at now, so the cache keeps it over what was not.
fn touch(path: &Path) {
    if let Ok(file) = std::fs::File::options().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// How many bytes every kept picture under `dir` takes.
fn directory_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0_u64;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The saved pictures are the person's: clearing the cache frees the looks, never them.
    #[test]
    fn the_cache_goes_and_what_was_saved_stays() {
        let dir = std::env::temp_dir().join(format!("tilo-tiles-{}", std::process::id()));
        let tiles = Tiles::open(dir.clone());
        let saved = Tiles::path_in(&tiles.saved_dir(), "https://tiles.test/1/0/0.png");
        let cached = Tiles::path_in(&tiles.cache_dir(), "https://tiles.test/2/1/1.png");
        tiles.keep_in(&saved, b"tile");
        tiles.keep_in(&cached, b"tile");
        assert!(tiles.saved_bytes() > 0);
        assert!(tiles.clear_cache() > 0);
        assert!(read(&saved).is_some(), "what the person saved is kept");
        assert!(read(&cached).is_none(), "what they only looked at goes");
        assert!(tiles.clear_saved() > 0);
        assert!(read(&saved).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An address names its picture by what it is: two addresses that differ are two pictures,
    /// and one address is the same picture every time.
    #[test]
    fn an_address_names_one_picture() {
        let tiles = Tiles::open(std::env::temp_dir());
        let first = Tiles::path_in(&tiles.cache_dir(), "https://tiles.test/1/0/0.png");
        let second = Tiles::path_in(&tiles.cache_dir(), "https://tiles.test/1/0/1.png");
        assert_ne!(first, second);
        assert_eq!(
            first,
            Tiles::path_in(&tiles.cache_dir(), "https://tiles.test/1/0/0.png")
        );
    }
}
