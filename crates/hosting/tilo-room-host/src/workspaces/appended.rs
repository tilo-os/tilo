//! What a governed workspace's record holds that grows with every change, in
//! files of their own beside it, appended to.
//!
//! The record is written whole with every change. It held the workspace's
//! publications since its last base (the base its whole governance) and the
//! receipt of every join it ever completed, so admitting one person cost a
//! write of everyone admitted before. Those lists are now each a file
//! (`<record>.published`, `<record>.joins`) the record names a count of: a
//! write appends what was added since the last, and the record then names the
//! new count. A list that was not only added to since (a base started over, a
//! person's receipts taken back) is written anew, which is once per many
//! changes.
//!
//! Each entry is a frame: its length (four bytes, little-endian), then its
//! postcard bytes. An entry appended before a record write that did not
//! happen is past the count the record names; opening reads that many and
//! cuts the rest.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// One list's file beside a workspace record, as written.
pub(super) struct Appended<T> {
    path: PathBuf,
    /// How many entries the file holds.
    filed: usize,
    /// The last of them: a list that no longer holds it there was changed
    /// otherwise than added to, and is written anew.
    last: Option<T>,
}

impl<T: Serialize + DeserializeOwned + Clone + PartialEq> Appended<T> {
    /// The file with `extension` beside the workspace record at `room_path`,
    /// and the first `filed` entries it holds, which the record names.
    /// Entries past them were appended for a record write that did not
    /// happen, and are cut off.
    pub(super) fn open(
        room_path: &Path,
        extension: &str,
        filed: u64,
    ) -> Result<(Self, Vec<T>), String> {
        let path = room_path.with_extension(extension);
        let at = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
        let filed = usize::try_from(filed).map_err(|error| at(&error))?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && filed == 0 => Vec::new(),
            Err(error) => return Err(at(&error)),
        };
        let mut entries = Vec::with_capacity(filed);
        let mut rest: &[u8] = &bytes;
        while entries.len() < filed {
            let (len, tail) = rest
                .split_first_chunk::<4>()
                .ok_or_else(|| at(&"fewer entries than its record names"))?;
            let len = usize::try_from(u32::from_le_bytes(*len)).map_err(|error| at(&error))?;
            let (frame, tail) = tail
                .split_at_checked(len)
                .ok_or_else(|| at(&"an entry is cut short"))?;
            entries.push(postcard::from_bytes::<T>(frame).map_err(|error| at(&error))?);
            rest = tail;
        }
        if !rest.is_empty() {
            let kept = bytes.len().saturating_sub(rest.len()) as u64;
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_len(kept))
                .map_err(|error| at(&error))?;
        }
        let last = entries.last().cloned();
        Ok((Self { path, filed, last }, entries))
    }

    /// Make the file hold `entries`, in order, before the record naming how
    /// many it holds is written: what was added since the last write is
    /// appended, and the file is written anew when the list changed otherwise.
    /// Answers how many it holds.
    pub(super) fn file(&mut self, entries: &[T]) -> Result<u64, String> {
        let at = |error: &dyn std::fmt::Display| format!("{}: {error}", self.path.display());
        let added_to = self.filed > 0
            && entries.len() >= self.filed
            && entries.get(self.filed.wrapping_sub(1)) == self.last.as_ref();
        if added_to && entries.len() == self.filed {
            return u64::try_from(self.filed).map_err(|error| at(&error));
        }
        if added_to {
            let mut appended = Vec::new();
            for entry in entries.get(self.filed..).unwrap_or_default() {
                frame(entry, &mut appended)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&self.path)
                .map_err(|error| at(&error))?;
            file.write_all(&appended)
                .and_then(|()| file.sync_data())
                .map_err(|error| at(&error))?;
        } else {
            let mut whole = Vec::new();
            for entry in entries {
                frame(entry, &mut whole)?;
            }
            tilo_files::native::atomic::write(&self.path, &whole).map_err(|error| at(&error))?;
        }
        self.filed = entries.len();
        self.last = entries.last().cloned();
        u64::try_from(self.filed).map_err(|error| at(&error))
    }

    /// Forget what the file was known to hold, so the next write writes it
    /// whole: a record write that failed may leave it past what the record
    /// names.
    pub(super) fn unsure(&mut self) {
        self.filed = 0;
        self.last = None;
    }
}

/// Append `entry`'s frame to `out`.
fn frame<T: Serialize>(entry: &T, out: &mut Vec<u8>) -> Result<(), String> {
    let bytes = postcard::to_stdvec(entry).map_err(|error| error.to_string())?;
    let len = u32::try_from(bytes.len()).map_err(|error| error.to_string())?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each write appends what was added since the last; a list changed
    /// otherwise is written anew; the record's count is what opening reads.
    #[test]
    fn entries_are_appended_and_read_back_as_the_record_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let room = dir.path().join("w.room");
        let (mut file, held) = Appended::<Vec<u8>>::open(&room, "list", 0).unwrap();
        assert!(held.is_empty());
        let mut list = vec![vec![1; 100], vec![2; 100]];
        assert_eq!(file.file(&list).unwrap(), 2);
        let written = || {
            std::fs::metadata(room.with_extension("list"))
                .unwrap()
                .len()
        };
        let first = written();
        list.push(vec![3; 100]);
        assert_eq!(file.file(&list).unwrap(), 3);
        assert_eq!(
            written() - first,
            first / 2,
            "the third entry was appended alone"
        );
        assert_eq!(Appended::<Vec<u8>>::open(&room, "list", 3).unwrap().1, list);
        // An entry taken back: the file is written anew.
        list.remove(0);
        assert_eq!(file.file(&list).unwrap(), 2);
        assert_eq!(Appended::<Vec<u8>>::open(&room, "list", 2).unwrap().1, list);
        // Started over from one entry.
        list = vec![vec![9; 10]];
        assert_eq!(file.file(&list).unwrap(), 1);
        assert_eq!(Appended::<Vec<u8>>::open(&room, "list", 1).unwrap().1, list);
    }

    /// Entries appended for a record write that did not happen are past the
    /// count the record names: opening reads the count and cuts the rest, so
    /// the next append follows the entries the record names.
    #[test]
    fn entries_past_the_records_count_are_cut_off() {
        let dir = tempfile::tempdir().unwrap();
        let room = dir.path().join("w.room");
        let (mut file, _) = Appended::<u32>::open(&room, "list", 0).unwrap();
        file.file(&[1, 2, 3]).unwrap();
        let (mut reopened, held) = Appended::<u32>::open(&room, "list", 2).unwrap();
        assert_eq!(held, vec![1, 2]);
        reopened.file(&[1, 2, 4]).unwrap();
        assert_eq!(
            Appended::<u32>::open(&room, "list", 3).unwrap().1,
            vec![1, 2, 4]
        );
        assert!(
            Appended::<u32>::open(&room, "list", 4).is_err(),
            "the file holds three"
        );
    }
}
