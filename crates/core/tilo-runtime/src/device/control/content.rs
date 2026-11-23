//! The device-control content store: the bytes a caller of the device's steps puts before it
//! names them by hash (a layer, a `.tapp`, a worktree's files and manifest). One file per blake3
//! hex, written by the daemon and read by the root steps.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use tilo_actions::remote::{ActionCallError, Caller, ContentPut, ContentPuts};

/// Where the device keeps what callers put, which is where its steps read it.
pub const CONTENT_DIR: &str = "/var/lib/tilo/device-control/content";

/// A content store at a directory of its own.
pub struct DeviceContent {
    dir: PathBuf,
}

impl DeviceContent {
    #[must_use]
    pub fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn unavailable(what: &str, error: &std::io::Error) -> ActionCallError {
    ActionCallError::Unavailable {
        reason: format!("the device could not {what}: {error}"),
    }
}

impl ContentPuts for DeviceContent {
    fn begin(&self, _caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
        let incoming = self.dir.join(".incoming");
        fs::create_dir_all(&incoming).map_err(|e| unavailable("keep an upload", &e))?;
        let path = incoming.join(uuid::Uuid::new_v4().simple().to_string());
        let spool = fs::File::create_new(&path).map_err(|e| unavailable("keep an upload", &e))?;
        Ok(Box::new(Upload {
            dir: self.dir.clone(),
            path,
            spool,
            hasher: blake3::Hasher::new(),
            size,
            written: 0,
        }))
    }

    fn have(&self, _caller: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
        Ok(hashes
            .iter()
            .map(|hash| self.dir.join(hash).is_file())
            .collect())
    }
}

/// One upload: its bytes wait in a file beside the store until the last piece arrives, hashed as
/// they come. Dropped unfinished, the file goes with it.
struct Upload {
    dir: PathBuf,
    path: PathBuf,
    spool: fs::File,
    hasher: blake3::Hasher,
    size: u64,
    written: u64,
}

impl ContentPut for Upload {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        if offset != self.written {
            return Err(ActionCallError::InvalidRequest {
                reason: format!(
                    "the upload has {} bytes: append at offset {}",
                    self.written, self.written
                ),
            });
        }
        let after = self
            .written
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if after > self.size {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("the upload began with {} bytes", self.size),
            });
        }
        self.spool
            .write_all(chunk)
            .map_err(|error| ActionCallError::Refused {
                reason: format!("the device could not keep the bytes: {error}"),
            })?;
        self.hasher.update(chunk);
        self.written = after;
        Ok(self.written)
    }

    fn finish(self: Box<Self>) -> Result<String, ActionCallError> {
        let hash = self.hasher.finalize().to_hex().to_string();
        self.spool
            .sync_all()
            .and_then(|()| fs::rename(&self.path, self.dir.join(&hash)))
            .map_err(|error| unavailable("store the upload", &error))?;
        tilo_actions::content_resource(&hash).ok_or_else(|| ActionCallError::Refused {
            reason: "the store named no content".into(),
        })
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        // Gone once stored; otherwise the unfinished bytes go with the upload.
        let _ = fs::remove_file(&self.path);
    }
}
