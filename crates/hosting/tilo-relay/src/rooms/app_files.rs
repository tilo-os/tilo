//! The package files of the app versions a room runs, kept by its host.
//!
//! A developer's own build is in no registry: whoever installs it into a
//! room hands its `.tapp` to the room's host first, in pieces, as the
//! room's `upload-app-file` action. The host keeps the file under its
//! BLAKE3 hash and answers members asking the room for that hash with it,
//! as it answers for any content the room's records reference
//! ([`super::content`]). A member installs it only once the bytes hash to
//! what the room's version names, and only bound to that version's package
//! root and publisher.

use std::io::Write as _;
use std::path::PathBuf;

use super::Rooms;

/// The largest package file a room keeps: what the host answers a member
/// with at most.
pub(crate) const MAX_APP_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// What one upload piece carries at most, before hex.
pub const APP_FILE_PIECE_BYTES: usize = 256 * 1024;

/// Where a piece left the upload.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Uploaded {
    /// More pieces are due; this many bytes are in.
    Partial(u64),
    /// Every byte is in and hashes to the name it was sent under.
    Complete,
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Rooms {
    fn app_files(&self, room: &str) -> PathBuf {
        self.inner.dir.join("app-files").join(room)
    }

    fn app_file_path(&self, room: &str, file: &str) -> Option<PathBuf> {
        (valid_hash(file) && uuid::Uuid::parse_str(room).is_ok())
            .then(|| self.app_files(room).join(format!("{file}.tapp")))
    }

    /// Whether this host keeps the package file `file` for `room`.
    pub(crate) fn keeps_app_file(&self, room: &str, file: &str) -> bool {
        self.app_file_path(room, file)
            .is_some_and(|path| path.is_file())
    }

    /// The package file `file` this host keeps for `room`, checked against
    /// its hash.
    pub(crate) fn app_file(&self, room: &str, file: &str) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.app_file_path(room, file)?).ok()?;
        (blake3::hash(&bytes).to_hex().as_str() == file).then_some(bytes)
    }

    /// Take `piece`, the bytes at `offset` of the package file `file` of
    /// `total` bytes, into `room`'s upload of it. Pieces come in order; a
    /// piece at offset 0 starts the upload again.
    ///
    /// # Errors
    /// A malformed name or size, a piece out of order, bytes that do not
    /// hash to `file`, or a file this host could not write.
    pub(crate) fn upload_app_file(
        &self,
        room: &str,
        file: &str,
        offset: u64,
        total: u64,
        piece: &[u8],
    ) -> Result<Uploaded, String> {
        let path = self
            .app_file_path(room, file)
            .ok_or("`file` must be 64 lowercase hex characters")?;
        if total == 0 || total > MAX_APP_FILE_BYTES {
            return Err(format!(
                "a package file is 1 byte to {} MiB",
                MAX_APP_FILE_BYTES / 1024 / 1024
            ));
        }
        if path.is_file() {
            return Ok(Uploaded::Complete);
        }
        let partial = path.with_extension("part");
        let have = if offset == 0 {
            0
        } else {
            std::fs::metadata(&partial).map_or(0, |meta| meta.len())
        };
        if have != offset {
            return Err(format!(
                "the host has {have} bytes of this file; send the piece at that offset"
            ));
        }
        let end = offset
            .checked_add(piece.len() as u64)
            .filter(|end| *end <= total)
            .ok_or("the piece runs past the file's size")?;
        std::fs::create_dir_all(self.app_files(room)).map_err(|error| error.to_string())?;
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(offset != 0)
            .truncate(offset == 0)
            .open(&partial)
            .map_err(|error| error.to_string())?;
        out.write_all(piece).map_err(|error| error.to_string())?;
        drop(out);
        if end < total {
            return Ok(Uploaded::Partial(end));
        }
        let bytes = std::fs::read(&partial).map_err(|error| error.to_string())?;
        if blake3::hash(&bytes).to_hex().as_str() != file {
            let _ = std::fs::remove_file(&partial);
            return Err("the file's bytes do not hash to the name it was sent under".into());
        }
        std::fs::rename(&partial, &path).map_err(|error| error.to_string())?;
        Ok(Uploaded::Complete)
    }
}
