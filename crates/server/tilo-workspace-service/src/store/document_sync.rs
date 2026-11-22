//! Which retained channels a joining device is replayed, by how their record
//! syncs ([`neighbour_wire::document_sync`]): an on-demand record, or a part of
//! a chained record behind its chain's head, is left out of every replay and
//! sent to a device only when it opens the record
//! ([`WorkspaceStore::record_frames`]).
//!
//! A channel's record is read once, from the route of any frame retained on
//! it, and remembered: a route never changes its channel. The heads of the
//! chains are the highest parts this store has seen.
//!
//! A workspace can also load its records when opened as a whole
//! ([`WorkspaceStore::set_documents_when_opened`]): a public workspace, which
//! anyone may walk into, sends a newcomer its governance and its shared
//! desktop, and every other record only once the newcomer's page opens it.
//! A chain's head too: a device opens the chain's head
//! ([`neighbour_wire::chain_head`]), is sent whichever part that is, and
//! follows the chain's later heads from then on ([`Reach::ChainHead`]).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use neighbour_wire::DocumentSync;
use tilo_lock::MutexRecover as _;

use super::{RETAINED_DIR, StoreError, WorkspaceStore};
use crate::payloads::Route;
use crate::transport::Envelope;

/// How a channel's record syncs, as this store knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Class {
    /// Replayed and followed live by every device.
    Eager,
    /// Sent only to a device that opened it.
    OnDemand,
    /// Part `index` of `app`'s chain `base`.
    Part {
        app: String,
        base: String,
        index: u64,
    },
    /// A chunk of a record split into chunks: its manifest names it, and a
    /// device is sent it once it opens it.
    Chunk,
}

/// How one retained frame's record reaches devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    Everyone,
    /// The devices that hold the record: its chain's head, which every
    /// device follows and keeps once it is a later part.
    Head([u8; 32]),
    /// The head `head` of a chain in a workspace that sends records when
    /// opened: only the devices that opened the chain's head (`chain`, the
    /// channel of [`neighbour_wire::chain_head`]) or hold the part follow
    /// it, and keep it once it is a later part.
    ChainHead {
        head: [u8; 32],
        chain: [u8; 32],
    },
    /// Only the devices that opened the record.
    Opened([u8; 32]),
}

/// A chain of an app's records: the app and the chain's base record.
type ChainKey = (String, String);

#[derive(Default)]
pub(super) struct Classes {
    channels: HashMap<[u8; 32], Class>,
    /// Every record but the shared desktop and the chains' heads reaches
    /// only the devices that opened it.
    when_opened: bool,
    /// Each chain's head: its highest part and that part's channel.
    heads: HashMap<ChainKey, (u64, [u8; 32])>,
    /// The people (their user root keys) who read each channel's resource
    /// store document read by a set of people: only their devices are sent
    /// it. A set its frames never named, or named unlike its id, is no one.
    readers: HashMap<[u8; 32], BTreeSet<[u8; 32]>>,
    /// The chain each chain's head channel ([`neighbour_wire::chain_head`])
    /// names, for every chain with a part.
    chains: HashMap<[u8; 32], ChainKey>,
    /// Lineages already read for their channel's record.
    lineages: HashSet<OsString>,
    /// Lineages whose first frame is still being retained: read again on the next look, without
    /// reading the whole directory again.
    pending: BTreeSet<OsString>,
    /// When the retained directory last changed as of the last read of its
    /// lineages: it is read again only once a lineage was added or removed.
    scanned: Option<std::time::SystemTime>,
}

impl Classes {
    fn learn(&mut self, channel: [u8; 32], route: &Route) {
        let (app, record) = (route.app_id.as_str(), route.document_id.as_str());
        // The set a frame names is taken only when it is the set the id names
        // by its digest: no frame widens a document past the people its id
        // was placed for.
        if let Some(named) = collab_types::store_document_readers(record)
            && named != collab_types::EVERY_MEMBER
            && neighbour_wire::app_document_group(app, record) == channel
        {
            let people = route
                .readers
                .as_deref()
                .filter(|readers| collab_types::reader_set_name(readers) == named)
                .and_then(collab_types::Readers::parse);
            if let Some(collab_types::Readers::People(people)) = people {
                self.readers.insert(channel, people);
            } else {
                self.readers.entry(channel).or_default();
            }
        }
        if self.channels.contains_key(&channel) {
            return;
        }
        let class = match neighbour_wire::document_sync(record) {
            // Nothing is published under a chain's head: it names one.
            DocumentSync::Eager | DocumentSync::ChainHead { .. } => Class::Eager,
            DocumentSync::OnDemand => Class::OnDemand,
            DocumentSync::Part { base, index } => {
                let chain = (app.to_owned(), base.to_owned());
                self.chains.insert(
                    neighbour_wire::app_document_group(app, &neighbour_wire::chain_head(base)),
                    chain.clone(),
                );
                let head = self.heads.entry(chain).or_insert((index, channel));
                if index > head.0 {
                    *head = (index, channel);
                }
                Class::Part {
                    app: app.to_owned(),
                    base: base.to_owned(),
                    index,
                }
            }
            DocumentSync::Chunk { .. } => Class::Chunk,
        };
        self.channels.insert(channel, class);
    }

    /// Learn `app`'s `record`, of no member, as a frame on `channel` names it.
    #[cfg(test)]
    fn learn_plain(&mut self, channel: [u8; 32], app: &str, record: &str) {
        self.learn(
            channel,
            &Route {
                app_id: app.to_owned(),
                cluster_id: String::new(),
                document_id: record.to_owned(),
                readers: None,
            },
        );
    }

    /// Whether a replay leaves out the lineages of this document: it is
    /// on-demand, or a part behind its chain's head.
    pub(super) fn leaves_out(&self, document: &[u8; 32]) -> bool {
        matches!(
            self.reach(document),
            Some(Reach::Opened(_) | Reach::ChainHead { .. })
        )
    }

    fn reach(&self, channel: &[u8; 32]) -> Option<Reach> {
        Some(match self.channels.get(channel)? {
            Class::Eager if self.when_opened && *channel != desktop_channel() => {
                Reach::Opened(*channel)
            }
            Class::Eager => Reach::Everyone,
            Class::OnDemand | Class::Chunk => Reach::Opened(*channel),
            Class::Part { app, base, index } => {
                let chain = (app.clone(), base.clone());
                if self.heads.get(&chain).is_some_and(|(head, _)| head > index) {
                    Reach::Opened(*channel)
                } else if self.when_opened {
                    Reach::ChainHead {
                        head: *channel,
                        chain: neighbour_wire::app_document_group(
                            app,
                            &neighbour_wire::chain_head(base),
                        ),
                    }
                } else {
                    Reach::Head(*channel)
                }
            }
        })
    }

    /// Who alone may be sent the document `channel` carries: its reader set's people. `None`
    /// when every member may.
    fn readers_of(&self, channel: &[u8; 32]) -> Option<BTreeSet<[u8; 32]>> {
        self.readers.get(channel).cloned()
    }

    /// Whether a device of `person` may be sent the document `channel` carries.
    fn reaches(&self, channel: &[u8; 32], person: &[u8; 32]) -> bool {
        self.readers
            .get(channel)
            .is_none_or(|people| people.contains(person))
    }

    /// The channel of the head of the chain whose head channel is `chain`
    /// ([`neighbour_wire::chain_head`]); `None` for any other channel, and
    /// for a chain with no part yet.
    fn head_of(&self, chain: &[u8; 32]) -> Option<[u8; 32]> {
        self.heads
            .get(self.chains.get(chain)?)
            .map(|(_, head)| *head)
    }
}

/// What a retained lineage's frames say about its record.
enum LineageRecord {
    /// No frame retained yet.
    Empty,
    /// The route its document frames name.
    Record(Route),
    /// Known already, or no document frame.
    Other,
}

/// The record classes of one store, shared by every handle on it.
#[derive(Default)]
pub(crate) struct DocumentClasses(Mutex<Classes>);

/// The retained channel of the workspace's shared desktop record: what a
/// joining page shows first, sent to every device whichever way the
/// workspace loads its records.
fn desktop_channel() -> [u8; 32] {
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
    neighbour_wire::retention::app_document_group(DESKTOP_APP, DESKTOP_DOCUMENT)
}

/// The channel a retained lineage directory `<author>-<channel>` holds.
pub(super) fn lineage_channel(name: &OsStr) -> Option<[u8; 32]> {
    let (_, channel) = name.to_str()?.split_once('-')?;
    tilo_failure::ignored!(
        "an entry not named `<author>-<channel>` is not a lineage",
        hex::decode(channel).ok()?.try_into().ok()
    )
}

impl WorkspaceStore {
    pub(super) fn classes(&self) -> std::sync::MutexGuard<'_, Classes> {
        self.capture_leases.records.0.lock_recover()
    }

    /// Load every record of this workspace but its shared desktop (and the
    /// heads of its chains) only for the devices that open it, or, with
    /// `false`, replay them to every device again. A public workspace does
    /// the first: a newcomer is sent its governance and desktop, not
    /// everything its crowd ever wrote. Captures list lineages afresh after
    /// a change, so the next replay follows it.
    pub(crate) fn set_documents_when_opened(&self, when_opened: bool) {
        let changed = {
            let mut classes = self.classes();
            std::mem::replace(&mut classes.when_opened, when_opened) != when_opened
        };
        if changed {
            self.forget_listed(None);
        }
    }

    /// Whether this workspace loads its records only when a device opens
    /// them ([`Self::set_documents_when_opened`]).
    pub(crate) fn documents_when_opened(&self) -> bool {
        self.classes().when_opened
    }

    /// The channel of the current head of the chain whose head channel
    /// ([`neighbour_wire::chain_head`]) `channel` is: what a device that
    /// opens the chain's head is sent. `None` for any other record, and for
    /// a chain this store has no part of. Every retained lineage is read for
    /// its record first, so the head is the newest part kept.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained lineages cannot be read.
    pub(crate) fn chain_head_channel(
        &self,
        channel: &[u8; 32],
    ) -> Result<Option<[u8; 32]>, StoreError> {
        // Every lineage, not only the ones a modification time says are new:
        // two parts of a chain written inside one tick leave that time equal,
        // so the cached read misses the later part. The answer is the chain a
        // device then follows, and "no part yet" is one it can never correct.
        self.read_lineages_again()?;
        Ok(self.classes().head_of(channel))
    }

    /// Who a retained frame on `channel`, whose co-edit body is `body`, goes
    /// to. A channel this store has not seen is learned from the body's
    /// route; a body that names none reaches everyone.
    pub(crate) fn frame_reach(&self, channel: &[u8; 32], body: &[u8]) -> Reach {
        let known = self.classes().reach(channel);
        if let Some(reach) = known {
            return reach;
        }
        let Some((route, _)) = self.payloads.open_document(body) else {
            return Reach::Everyone;
        };
        let mut classes = self.classes();
        classes.learn(*channel, &route);
        classes.reach(channel).unwrap_or(Reach::Everyone)
    }

    /// Whether a device of the person whose user root key is `person` may be
    /// sent the record `channel` carries: any device for a record of the
    /// workspace, only its people's for a resource store's document a set of
    /// people reads.
    pub(crate) fn reaches_person(&self, channel: &[u8; 32], person: &[u8; 32]) -> bool {
        // Every retained record's person is known first: a host that just
        // started has read no lineage yet.
        if let Err(error) = self.read_lineage_records() {
            tracing::warn!(%error, "the retained records' people did not read");
            return false;
        }
        self.classes().reaches(channel, person)
    }

    /// Who alone may be sent the document the state frame `delta` changes: the
    /// people of its reader set, for a resource store's document only they
    /// read. `None` for a document every member reads.
    pub(crate) fn frame_readers(&self, delta: &[u8]) -> Option<BTreeSet<[u8; 32]>> {
        let channel = self.frame_channel(delta)?;
        self.classes().readers_of(&channel)
    }

    /// Where the document the state frame `delta` changes is retained, which
    /// document that is learned from the frame: retained there, or sent as it
    /// is.
    fn frame_channel(&self, delta: &[u8]) -> Option<[u8; 32]> {
        if let Some(
            crate::coedit::CoeditPayload::Checkpoint { channel, body }
            | crate::coedit::CoeditPayload::RetainedDelta { channel, body },
        ) = crate::coedit::CoeditPayload::decode(delta)
        {
            if let Some((route, _)) = self.payloads.open_document(&body) {
                self.classes().learn(channel, &route);
            }
            return Some(channel);
        }
        let (route, _) = self.payloads.open_document(delta)?;
        let channel = neighbour_wire::app_document_group(&route.app_id, &route.document_id);
        self.classes().learn(channel, &route);
        Some(channel)
    }

    /// Learn the record of `channel` from `bytes`, a frame about to be
    /// retained on it, unless the channel is known already. A chain's later
    /// part is then a head before any capture can list its lineage, so a
    /// capture never takes an earlier part for the head because it met that
    /// part first, nor misses a lineage the directory scan skipped: the
    /// scan's change test compares directory times, which two lineages added
    /// within one clock tick leave equal.
    pub(super) fn learn_retained_document(&self, channel: &[u8; 32], bytes: &[u8]) {
        if self.classes().channels.contains_key(channel) {
            return;
        }
        let Some((route, _)) = super::retained::body_of(bytes, self.workspace_id)
            .and_then(|body| self.payloads.open_document(&body))
        else {
            return;
        };
        self.classes().learn(*channel, &route);
    }

    /// The channels of the chains' heads: what a device that subscribes now
    /// is replayed of each chain, and keeps following once a later part
    /// starts. None in a workspace that sends records when opened, where a
    /// device follows only the chains whose head it opened.
    pub(crate) fn chain_heads(&self) -> Vec<[u8; 32]> {
        let classes = self.classes();
        if classes.when_opened {
            return Vec::new();
        }
        classes
            .heads
            .values()
            .map(|(_, channel)| *channel)
            .collect()
    }

    /// Read every retained lineage not read yet for its record, as a capture
    /// does once before it lists them: nothing changes the lineages while a
    /// capture lists them, since it lists in one owner operation, so its
    /// answers ([`Classes::leaves_out`], [`Self::carries_document`]) need
    /// no other read. Read again for every lineage it listed, a crowd's
    /// captures each scanned the whole retained directory hundreds of times
    /// while one lineage was still being retained, and newcomers queued
    /// behind the owner for seconds.
    pub(super) fn classify_lineages(&self) -> Result<(), StoreError> {
        self.read_lineage_records()
    }

    /// Whether the retained lineage `name` carries a record's frames, of the
    /// lineages read so far ([`Self::classify_lineages`]). A lineage of
    /// anything else — a device's certificate, the workspace's own document,
    /// a channel's governance — carries none.
    pub(super) fn carries_document(&self, name: &OsStr) -> bool {
        lineage_channel(name).is_some_and(|channel| self.classes().channels.contains_key(&channel))
    }

    /// Take `channel` to carry `app`'s `record`, as a frame retained on it
    /// would say.
    #[cfg(test)]
    pub(crate) fn learn_document(&self, channel: [u8; 32], app: &str, record: &str) {
        self.classes().learn_plain(channel, app, record);
    }

    /// Learn the record of every retained lineage not read yet, unless no
    /// lineage was added or removed since the last time.
    fn read_lineage_records(&self) -> Result<(), StoreError> {
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        let changed = fs::metadata(&v2).and_then(|meta| meta.modified()).ok();
        {
            let classes = self.classes();
            // A lineage whose first frame is still being retained is read again, alone: the
            // directory was read whole.
            if changed.is_some() && classes.scanned == changed && classes.pending.is_empty() {
                return Ok(());
            }
        }
        self.read_lineages_at(&v2, changed)
    }

    /// [`Self::read_lineage_records`] whatever the modification time says: a
    /// lineage added inside the tick the scan last saw is otherwise never read.
    fn read_lineages_again(&self) -> Result<(), StoreError> {
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        let changed = fs::metadata(&v2).and_then(|meta| meta.modified()).ok();
        self.read_lineages_at(&v2, changed)
    }

    /// Learn the record of every retained lineage of `v2` not read yet, and
    /// remember `changed` as the directory's state then.
    fn read_lineages_at(
        &self,
        v2: &Path,
        changed: Option<std::time::SystemTime>,
    ) -> Result<(), StoreError> {
        let entries = match fs::read_dir(v2) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        // A lineage still being retained is read again next time, whether or
        // not another is added meanwhile: until then the store does not know
        // which part of its chain is the head.
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
            // Its first frame is still being retained: it holds nothing to classify yet, and it is
            // read again on the next look. Marking it stale here would mark it on every look, and a
            // capture listing the order again and again never finishes.
            if matches!(read, LineageRecord::Empty) {
                self.classes().pending.insert(name);
                continue;
            }
            // Read for the first time: the replay order lists it.
            self.forget_listed(Some(std::slice::from_ref(&name)));
            let mut classes = self.classes();
            if let (LineageRecord::Record(route), Some(channel)) = (read, channel) {
                classes.learn(channel, &route);
            }
            classes.pending.remove(&name);
            classes.lineages.insert(name);
        }
        self.classes().scanned = changed;
        Ok(())
    }

    /// The app and record of the route a frame of `lineage` names.
    fn lineage_record(&self, lineage: &Path) -> Result<LineageRecord, StoreError> {
        let Some(active) = super::retained::current(lineage)? else {
            return Ok(LineageRecord::Empty);
        };
        let Some(frame) = super::compaction::document_names(&active)?
            .into_iter()
            .next()
        else {
            return Ok(LineageRecord::Empty);
        };
        let Some(bytes) = tilo_server_store::frames::frame_bytes(&active.join(frame))? else {
            return Ok(LineageRecord::Other);
        };
        Ok(super::retained::body_of(&bytes, self.workspace_id)
            .and_then(|body| self.payloads.open_document(&body))
            .map_or(LineageRecord::Other, |(route, _)| {
                LineageRecord::Record(route)
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
            for name in super::compaction::document_names(&active)? {
                let Some(bytes) = tilo_server_store::frames::frame_bytes(&active.join(&name))?
                else {
                    continue;
                };
                if let Some((envelope @ Envelope::Delta { workspace_id, .. }, [])) =
                    tilo_server_store::frames::framed_envelope(&bytes)
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
#[path = "document_sync_tests.rs"]
mod store_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_replays_its_head_and_keeps_earlier_parts_for_who_opens_them() {
        let mut classes = Classes::default();
        let (eager, on_demand, first, second) = ([1; 32], [2; 32], [3; 32], [4; 32]);
        classes.learn_plain(eager, "app", "general");
        classes.learn_plain(
            on_demand,
            "app",
            &neighbour_wire::on_demand_document("uuid:1"),
        );
        classes.learn_plain(first, "app", &neighbour_wire::document_part("chat", 0));
        assert_eq!(classes.reach(&eager), Some(Reach::Everyone));
        assert_eq!(classes.reach(&on_demand), Some(Reach::Opened(on_demand)));
        assert_eq!(classes.reach(&first), Some(Reach::Head(first)));
        classes.learn_plain(second, "app", &neighbour_wire::document_part("chat", 1));
        assert_eq!(classes.reach(&first), Some(Reach::Opened(first)));
        assert_eq!(classes.reach(&second), Some(Reach::Head(second)));
        classes.learn_plain(first, "app", &neighbour_wire::document_part("chat", 0));
        assert_eq!(classes.reach(&second), Some(Reach::Head(second)));
        assert_eq!(classes.reach(&[9; 32]), None);
    }

    #[test]
    fn a_public_workspace_sends_every_record_but_its_desktop_when_opened() {
        let mut classes = Classes {
            when_opened: true,
            ..Classes::default()
        };
        let (note, desktop, first, second) = ([1; 32], desktop_channel(), [3; 32], [4; 32]);
        let chain = neighbour_wire::app_document_group("chat", &neighbour_wire::chain_head("m"));
        classes.learn_plain(note, "notes", "note-1");
        classes.learn_plain(
            desktop,
            tilo_desktop_model::ownership::DESKTOP_APP,
            tilo_desktop_model::ownership::DESKTOP_DOCUMENT,
        );
        assert_eq!(classes.head_of(&chain), None, "a chain with no part yet");
        classes.learn_plain(first, "chat", &neighbour_wire::document_part("m", 0));
        assert_eq!(classes.reach(&note), Some(Reach::Opened(note)));
        assert_eq!(classes.reach(&desktop), Some(Reach::Everyone));
        // A chain's head reaches who opened the chain's head, which names it.
        assert_eq!(
            classes.reach(&first),
            Some(Reach::ChainHead { head: first, chain })
        );
        assert_eq!(classes.head_of(&chain), Some(first));
        classes.learn_plain(second, "chat", &neighbour_wire::document_part("m", 1));
        assert_eq!(classes.head_of(&chain), Some(second));
        assert_eq!(classes.reach(&first), Some(Reach::Opened(first)));
        // Another app's chain of the same name is a chain of its own.
        let other = neighbour_wire::app_document_group("notes", &neighbour_wire::chain_head("m"));
        assert_eq!(classes.head_of(&other), None);
        classes.when_opened = false;
        assert_eq!(classes.reach(&note), Some(Reach::Everyone));
        assert_eq!(classes.reach(&second), Some(Reach::Head(second)));
    }
}
