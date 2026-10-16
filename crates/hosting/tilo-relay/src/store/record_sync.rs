//! Which retained channels a joining device is replayed, by how their record
//! syncs ([`neighbour_wire::record_sync`]): an on-demand record, or a part of
//! a chained record behind its chain's head, is left out of every replay and
//! sent to a device only when it opens the record
//! ([`WorkspaceStore::record_frames`]).
//!
//! A channel's record is read once, from the route of any frame retained on
//! it, and remembered: a route never changes its channel. The heads of the
//! chains are the highest parts this store has seen.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use neighbour_wire::RecordSync;
use tilo_lock::MutexRecover as _;

use super::{RETAINED_DIR, StoreError, WorkspaceStore};
use crate::transport::Envelope;

/// How a channel's record syncs, as this store knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Class {
    /// Replayed and followed live by every device.
    Eager,
    /// Sent only to a device that opened it.
    OnDemand,
    /// Part `index` of chain `base`.
    Part { base: String, index: u64 },
}

/// How one retained frame's record reaches devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Every device that follows the room.
    Everyone,
    /// The devices that hold the record: its chain's head, which every
    /// device follows and keeps once it is a later part.
    Head([u8; 32]),
    /// Only the devices that opened the record.
    Opened([u8; 32]),
}

#[derive(Default)]
struct Classes {
    channels: HashMap<[u8; 32], Class>,
    /// Each chain's head: its highest part and that part's channel.
    heads: HashMap<String, (u64, [u8; 32])>,
    /// Lineages already read for their channel's record.
    lineages: HashSet<OsString>,
}

impl Classes {
    fn learn(&mut self, channel: [u8; 32], record: &str) {
        if self.channels.contains_key(&channel) {
            return;
        }
        let class = match neighbour_wire::record_sync(record) {
            RecordSync::Eager => Class::Eager,
            RecordSync::OnDemand => Class::OnDemand,
            RecordSync::Part { base, index } => {
                let head = self
                    .heads
                    .entry(base.to_owned())
                    .or_insert((index, channel));
                if index > head.0 {
                    *head = (index, channel);
                }
                Class::Part {
                    base: base.to_owned(),
                    index,
                }
            }
        };
        self.channels.insert(channel, class);
    }

    fn reach(&self, channel: &[u8; 32]) -> Option<Reach> {
        Some(match self.channels.get(channel)? {
            Class::Eager => Reach::Everyone,
            Class::OnDemand => Reach::Opened(*channel),
            Class::Part { base, index } => {
                if self.heads.get(base).is_some_and(|(head, _)| head > index) {
                    Reach::Opened(*channel)
                } else {
                    Reach::Head(*channel)
                }
            }
        })
    }
}

/// What a retained lineage's frames say about its record.
enum LineageRecord {
    /// No frame retained yet.
    Empty,
    /// The record its document frames name.
    Record(String),
    /// Known already, or no document frame.
    Other,
}

/// The record classes of one store, shared by every handle on it.
#[derive(Default)]
pub(crate) struct RecordClasses(Mutex<Classes>);

/// The channel a retained lineage directory `<author>-<channel>` holds.
fn lineage_channel(name: &OsStr) -> Option<[u8; 32]> {
    let (_, channel) = name.to_str()?.split_once('-')?;
    hex::decode(channel).ok()?.try_into().ok()
}

impl WorkspaceStore {
    fn classes(&self) -> std::sync::MutexGuard<'_, Classes> {
        self.capture_leases.records.0.lock_recover()
    }

    /// Who a retained frame on `channel`, whose co-edit body is `body`, goes
    /// to. A channel this store has not seen is learned from the body's
    /// route; a body that names none reaches everyone.
    pub(crate) fn frame_reach(&self, channel: &[u8; 32], body: &[u8]) -> Reach {
        if let Some(reach) = self.classes().reach(channel) {
            return reach;
        }
        let Some((route, _)) = self.payloads.open_document(body) else {
            return Reach::Everyone;
        };
        let mut classes = self.classes();
        classes.learn(*channel, &route.record_id);
        classes.reach(channel).unwrap_or(Reach::Everyone)
    }

    /// The channels of the chains' heads: what a device that subscribes now
    /// is replayed of each chain, and keeps following once a later part
    /// starts.
    pub(crate) fn chain_heads(&self) -> Vec<[u8; 32]> {
        self.classes()
            .heads
            .values()
            .map(|(_, channel)| *channel)
            .collect()
    }

    /// Whether a replay leaves the retained lineage `name` out: its record
    /// is on-demand, or a part behind its chain's head. Every lineage of the
    /// store is read for its record before the first answer, so a chain's
    /// head is known whichever part a capture lists first.
    pub(super) fn leaves_out_on_demand(&self, name: &OsStr) -> Result<bool, StoreError> {
        let Some(channel) = lineage_channel(name) else {
            return Ok(false);
        };
        if !self.classes().lineages.contains(name) {
            self.read_lineage_records()?;
        }
        Ok(matches!(
            self.classes().reach(&channel),
            Some(Reach::Opened(_))
        ))
    }

    /// Learn the record of every retained lineage not read yet.
    fn read_lineage_records(&self) -> Result<(), StoreError> {
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        let entries = match fs::read_dir(&v2) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            if self.classes().lineages.contains(&name) {
                continue;
            }
            let channel = lineage_channel(&name);
            let read = match channel {
                Some(channel) if !self.classes().channels.contains_key(&channel) => {
                    self.lineage_record(&entry.path())?
                }
                _ => LineageRecord::Other,
            };
            let mut classes = self.classes();
            match (read, channel) {
                // Read again next time: its first frame is still being
                // retained.
                (LineageRecord::Empty, _) => continue,
                (LineageRecord::Record(record), Some(channel)) => classes.learn(channel, &record),
                _ => {}
            }
            classes.lineages.insert(name);
        }
        Ok(())
    }

    /// The record of the route a frame of `lineage` names.
    fn lineage_record(&self, lineage: &Path) -> Result<LineageRecord, StoreError> {
        let Some(active) = super::retained::current(lineage)? else {
            return Ok(LineageRecord::Empty);
        };
        let Some(frame) = super::compaction::record_names(&active)?.into_iter().next() else {
            return Ok(LineageRecord::Empty);
        };
        let Some(bytes) = tilo_relay_store::frames::frame_bytes(&active.join(frame))? else {
            return Ok(LineageRecord::Other);
        };
        Ok(super::retained::body_of(&bytes, self.workspace_id)
            .and_then(|body| self.payloads.open_document(&body))
            .map_or(LineageRecord::Other, |(route, _)| {
                LineageRecord::Record(route.record_id)
            }))
    }

    /// Every frame retained for `channel`, over all its authors, each
    /// lineage's checkpoint before its tail: what a device that opens the
    /// channel's record is sent. Reads only immutable files and needs no
    /// owner.
    ///
    /// # Errors
    ///
    /// Returns an error when a lineage cannot be read.
    pub(crate) fn record_frames(&self, channel: &[u8; 32]) -> Result<Vec<Envelope>, StoreError> {
        let mut frames = Vec::new();
        for (lineage, _) in self.channel_lineages(channel)? {
            let Some(active) = super::retained::current(&lineage)? else {
                continue;
            };
            for name in super::compaction::record_names(&active)? {
                let Some(bytes) = tilo_relay_store::frames::frame_bytes(&active.join(&name))?
                else {
                    continue;
                };
                if let Some((envelope @ Envelope::Delta { workspace_id, .. }, [])) =
                    tilo_relay_store::frames::framed_envelope(&bytes)
                    && workspace_id == self.workspace_id
                {
                    frames.push(envelope);
                }
            }
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_replays_its_head_and_keeps_earlier_parts_for_who_opens_them() {
        let mut classes = Classes::default();
        let (eager, on_demand, first, second) = ([1; 32], [2; 32], [3; 32], [4; 32]);
        classes.learn(eager, "general");
        classes.learn(on_demand, &neighbour_wire::on_demand_record("uuid:1"));
        classes.learn(first, &neighbour_wire::record_part("chat", 0));
        assert_eq!(classes.reach(&eager), Some(Reach::Everyone));
        assert_eq!(classes.reach(&on_demand), Some(Reach::Opened(on_demand)));
        assert_eq!(classes.reach(&first), Some(Reach::Head(first)));
        classes.learn(second, &neighbour_wire::record_part("chat", 1));
        assert_eq!(classes.reach(&first), Some(Reach::Opened(first)));
        assert_eq!(classes.reach(&second), Some(Reach::Head(second)));
        // Learning the earlier part again moves nothing back.
        classes.learn(first, &neighbour_wire::record_part("chat", 0));
        assert_eq!(classes.reach(&second), Some(Reach::Head(second)));
        assert_eq!(classes.reach(&[9; 32]), None);
    }
}
