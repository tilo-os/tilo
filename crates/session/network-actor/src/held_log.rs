//! The sealed writes this device holds for a room's hosts, kept on disk in
//! a file of the room's own until a host keeps them
//! ([`super::sealed_outbox`]).
//!
//! Each room's file is a log: every write held appends its entry, and every
//! answer that a host kept writes appends theirs, each made durable before
//! the next. Reading the log back replays it, so a device that shut down
//! with writes held holds them again when it starts, in the order they were
//! written. The file grows with what is held, with no bound of its own; once
//! nothing is held any more it goes. An entry cut short by a crash at the end
//! of the log was never made durable: it is read as never written, and the
//! log's writer cuts it off before it appends ([`HeldLog::repair`]), so the
//! next entry follows the last whole one.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use neighbour_wire::Retention;
use serde::{Deserialize, Serialize};
use tilo_desktop_model::RoomId;

/// A sealed write held for a room's hosts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldWrite {
    pub op: [u8; 16],
    pub channel: [u8; 32],
    pub retention: Retention,
    pub bytes: Vec<u8>,
}

/// One entry of a room's log.
#[derive(Serialize, Deserialize)]
enum Entry {
    /// A write held, with whether it is retained as the channel's
    /// checkpoint rather than a delta.
    Held {
        op: [u8; 16],
        channel: [u8; 32],
        checkpoint: bool,
        bytes: Vec<u8>,
    },
    /// Hosts kept these writes.
    Kept { ops: Vec<[u8; 16]> },
}

/// The directory of every room's log.
#[derive(Clone, Debug)]
pub struct HeldLog {
    dir: PathBuf,
}

impl HeldLog {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, room: RoomId) -> PathBuf {
        self.dir.join(format!("{room}.log"))
    }

    /// The writes `room`'s log holds, oldest first.
    ///
    /// # Errors
    /// The log exists but cannot be read.
    pub fn read(&self, room: RoomId) -> std::io::Result<Vec<HeldWrite>> {
        Ok(self.replay(room)?.0)
    }

    /// Cut off an entry left cut short at the end of `room`'s log. Only the
    /// log's writer does, before its first entry.
    ///
    /// # Errors
    /// The log cannot be read or cut.
    pub(super) fn repair(&self, room: RoomId) -> std::io::Result<()> {
        let (_, whole, length) = self.replay(room)?;
        if whole < length {
            let file = OpenOptions::new().write(true).open(self.path(room))?;
            file.set_len(whole as u64)?;
            file.sync_data()?;
        }
        Ok(())
    }

    /// The writes `room`'s log holds, how many of its bytes are whole
    /// entries, and how many bytes it has.
    fn replay(&self, room: RoomId) -> std::io::Result<(Vec<HeldWrite>, usize, usize)> {
        let mut bytes = Vec::new();
        match File::open(self.path(room)) {
            Ok(mut file) => {
                file.read_to_end(&mut bytes)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), 0, 0));
            }
            Err(error) => return Err(error),
        }
        let mut held: Vec<HeldWrite> = Vec::new();
        let mut rest = bytes.as_slice();
        let mut whole = 0_usize;
        while let Some((length, after)) = rest.split_first_chunk::<4>() {
            let length = u32::from_le_bytes(*length) as usize;
            let Some((body, after)) = after.split_at_checked(length) else {
                // Cut short at the end: never made durable, never held.
                break;
            };
            rest = after;
            whole = whole.saturating_add(4).saturating_add(length);
            match postcard::from_bytes::<Entry>(body) {
                Ok(Entry::Held {
                    op,
                    channel,
                    checkpoint,
                    bytes,
                }) => {
                    if !held.iter().any(|write| write.op == op) {
                        held.push(HeldWrite {
                            op,
                            channel,
                            retention: if checkpoint {
                                Retention::Checkpoint
                            } else {
                                Retention::Delta
                            },
                            bytes,
                        });
                    }
                }
                Ok(Entry::Kept { ops }) => held.retain(|write| !ops.contains(&write.op)),
                Err(error) => {
                    tracing::warn!(%room, %error, "a held sealed write's log entry does not read");
                }
            }
        }
        Ok((held, whole, bytes.len()))
    }

    /// Append `write` to `room`'s log, durably.
    ///
    /// # Errors
    /// The log cannot be written, the disk being full among others.
    pub(super) fn hold(&self, room: RoomId, write: &HeldWrite) -> std::io::Result<()> {
        self.append(
            room,
            &Entry::Held {
                op: write.op,
                channel: write.channel,
                checkpoint: matches!(write.retention, Retention::Checkpoint),
                bytes: write.bytes.clone(),
            },
        )
    }

    /// Append that hosts kept `ops` to `room`'s log, durably; with nothing
    /// held any more (`empty`), the log goes instead.
    ///
    /// # Errors
    /// The log cannot be written or removed.
    pub(super) fn kept(
        &self,
        room: RoomId,
        ops: Vec<[u8; 16]>,
        empty: bool,
    ) -> std::io::Result<()> {
        if empty {
            return match std::fs::remove_file(self.path(room)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => sync_dir(&self.dir),
            };
        }
        self.append(room, &Entry::Kept { ops })
    }

    fn append(&self, room: RoomId, entry: &Entry) -> std::io::Result<()> {
        let body = postcard::to_allocvec(entry).map_err(std::io::Error::other)?;
        let length = u32::try_from(body.len()).map_err(std::io::Error::other)?;
        let created = !self.path(room).exists();
        if created {
            std::fs::create_dir_all(&self.dir)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(room))?;
        let mut framed = Vec::with_capacity(body.len().saturating_add(4));
        framed.extend_from_slice(&length.to_le_bytes());
        framed.extend_from_slice(&body);
        file.write_all(&framed)?;
        file.sync_data()?;
        if created {
            sync_dir(&self.dir)?;
        }
        Ok(())
    }
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(index: u32) -> HeldWrite {
        let mut op = [0; 16];
        op[..4].copy_from_slice(&index.to_le_bytes());
        HeldWrite {
            op,
            channel: [7; 32],
            retention: if index.is_multiple_of(2) {
                Retention::Delta
            } else {
                Retention::Checkpoint
            },
            bytes: vec![u8::try_from(index % 251).unwrap(); 64],
        }
    }

    #[test]
    fn a_log_keeps_every_write_held_until_kept_and_goes_once_none_is() {
        let dir = tempfile::tempdir().unwrap();
        let log = HeldLog::new(dir.path().join("held"));
        let room = RoomId::new();
        assert_eq!(log.read(room).unwrap(), [], "nothing held, no log");
        // More than a room ever held before: nothing has a bound of its own.
        let writes: Vec<_> = (0..5000).map(write).collect();
        for held in &writes {
            log.hold(room, held).unwrap();
        }
        log.hold(room, &writes[3]).unwrap();
        // Read again, as a device that started again reads it.
        let reopened = HeldLog::new(dir.path().join("held"));
        assert_eq!(
            reopened.read(room).unwrap(),
            writes,
            "every write, in order, once"
        );
        let kept: Vec<_> = writes[..4000].iter().map(|write| write.op).collect();
        reopened.kept(room, kept, false).unwrap();
        assert_eq!(reopened.read(room).unwrap(), writes[4000..]);
        let rest = writes[4000..].iter().map(|write| write.op).collect();
        reopened.kept(room, rest, true).unwrap();
        assert!(!reopened.path(room).exists(), "nothing held, the log goes");
        assert_eq!(reopened.read(room).unwrap(), []);
    }

    #[test]
    fn a_write_cut_short_at_the_end_of_the_log_was_never_held() {
        let dir = tempfile::tempdir().unwrap();
        let log = HeldLog::new(dir.path().to_path_buf());
        let room = RoomId::new();
        log.hold(room, &write(1)).unwrap();
        log.hold(room, &write(2)).unwrap();
        let path = log.path(room);
        let length = std::fs::metadata(&path).unwrap().len();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(length - 10).unwrap();
        assert_eq!(log.read(room).unwrap(), [write(1)]);
        // Its writer cuts it off: the next write follows the last whole entry.
        log.repair(room).unwrap();
        log.hold(room, &write(3)).unwrap();
        assert_eq!(log.read(room).unwrap(), [write(1), write(3)]);
    }
}
