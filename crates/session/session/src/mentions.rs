//! Telling the person when someone mentions them, wherever in the workspace.
//!
//! A mention is a run of a record's rich text marked with a participant's id, whichever app wrote
//! it ([`ui::mentions::in_record_for`]). Every record a change reaches is looked at as the change
//! arrives, and each new mention of the person here is one [`Notice`]. A mention is never told
//! twice, is taken back when it is edited away, and one that was already there when this device
//! started following the record is not news. What was told is kept in the session's store, so a
//! restart tells nothing again.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use durable_store::{DurableStore, StorageKey, WriteOp};
use ui::automerge::AutoCommit;

use crate::platform::Store;
use crate::platform::notice::{Notice, Notices, OPEN_ACTION, Place};
use crate::platform::records::{Changed, Records};

/// The package this device's told state is kept under: the session's.
const PACKAGE: &str = "so.tilo.session.mentions";

/// Least time between two looks, in milliseconds: changes arriving in a burst (a replay, someone
/// typing) are looked at together.
const LOOK_EVERY_MS: u64 = 750;

/// How long a mention's notice stays on screen: what the person was told, not a demand.
const NOTICE_SECONDS: u64 = 8;

/// What one look at a record changed: notices to show, and the tags of those whose mention is
/// gone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    pub show: Vec<Notice>,
    pub withdraw: Vec<String>,
}

/// A record as the person's mentions are looked for in it.
pub struct Look<'a> {
    /// The app keeping it.
    pub app: &'a str,
    /// The record, as its route names it: empty for the app's own record of the workspace.
    pub record: &'a str,
    /// The kind of the app's records, for a record (or chain) whose document names none, as the
    /// app's own record of the workspace does.
    pub kind: &'a str,
    /// Its document.
    pub doc: &'a AutoCommit,
    /// The record its places belong to, when it is a part of another (a chat's part, published as
    /// the chat's items): its document.
    pub tree: Option<&'a AutoCommit>,
    /// Take what it holds as seen: the person is looking at it, or it is what the workspace held
    /// before this device first followed it.
    pub quiet: bool,
    /// How a reference in its text reads for the person told: what a notice's excerpt says of it
    /// ([`ui::mentions::reading_nothing`] where nobody here can tell what they hold).
    pub read: ui::mentions::Reader<'a>,
}

/// What this device told, and what must not be told again.
#[derive(Default)]
pub struct Held {
    /// Per record (`app` and route), the tags told and still standing.
    told: BTreeMap<(String, String), BTreeSet<String>>,
}

impl Held {
    /// Whether this device ever looked at `app`'s record `route`.
    #[must_use]
    pub fn knows(&self, app: &str, route: &str) -> bool {
        self.told.contains_key(&(app.to_owned(), route.to_owned()))
    }

    /// Look at a record again: new mentions of `me` are told, ones no longer there withdrawn.
    pub fn look(
        &mut self,
        look: &Look<'_>,
        me: &BTreeSet<String>,
        name: &dyn Fn(&str) -> String,
    ) -> Told {
        let (record, kind) = place_record(look);
        let route = look.record.to_owned();
        let mut standing: BTreeMap<String, Notice> = BTreeMap::new();
        for mention in ui::mentions::in_record_for(look.doc, look.read) {
            if !me.contains(&mention.person)
                || mention.by.as_ref().is_some_and(|by| me.contains(by))
            {
                continue;
            }
            // Without a kind the item has no address: the record opens.
            let resource = mention
                .item
                .as_ref()
                .filter(|_| !kind.is_empty())
                .map_or_else(String::new, |(field, id)| format!("{kind}/{field}/{id}"));
            let place = Place {
                app: look.app.to_owned(),
                record: record.clone(),
                resource,
            };
            let tag = place.tag(&mention.person);
            if standing.contains_key(&tag) {
                continue;
            }
            let who = mention
                .by
                .as_deref()
                .map_or_else(|| crate::i18n::t("mention-notice-someone"), name);
            let channel = mention.channel.as_deref().and_then(|channel| {
                look.tree
                    .and_then(|tree| ui::mentions::channel_name(tree, channel))
            });
            let title = match (channel, &mention.title) {
                (Some(channel), _) => crate::i18n::t_args(
                    "mention-notice-channel",
                    &[
                        ("name", who.as_str().into()),
                        ("channel", channel.as_str().into()),
                    ],
                ),
                (None, Some(title)) => crate::i18n::t_args(
                    "mention-notice-title",
                    &[
                        ("name", who.as_str().into()),
                        ("place", title.as_str().into()),
                    ],
                ),
                (None, None) => {
                    crate::i18n::t_args("mention-notice", &[("name", who.as_str().into())])
                }
            };
            standing.insert(
                tag.clone(),
                Notice::opens(
                    tilo_desktop_search::names::app_name(look.app),
                    title,
                    mention.excerpt,
                    place,
                )
                .tagged(tag)
                .choosing(vec![(
                    OPEN_ACTION.to_owned(),
                    crate::i18n::t("mention-notice-open"),
                )])
                // News, not a demand: it leaves the banner by itself and the centre keeps it.
                .staying(Some(ui::time::Duration::from_secs(NOTICE_SECONDS))),
            );
        }
        let told = self.told.entry((look.app.to_owned(), route)).or_default();
        let withdraw: Vec<String> = told
            .iter()
            .filter(|tag| !standing.contains_key(*tag))
            .cloned()
            .collect();
        for tag in &withdraw {
            told.remove(tag);
        }
        let show = standing
            .into_iter()
            .filter(|(tag, _)| told.insert(tag.clone()))
            .map(|(_, notice)| notice)
            .filter(|_| !look.quiet)
            .collect();
        Told { show, withdraw }
    }

    /// What this device told, as it is kept: one entry per record, its app and route then each
    /// tag.
    fn encoded(&self) -> serde_json::Map<String, serde_json::Value> {
        self.told
            .iter()
            .map(|((app, route), tags)| {
                (
                    format!("{app}\t{route}"),
                    serde_json::Value::Array(
                        tags.iter()
                            .map(|tag| serde_json::Value::String(tag.clone()))
                            .collect(),
                    ),
                )
            })
            .collect()
    }

    /// Take up what was kept, as [`Self::encoded`] wrote it.
    fn restore(&mut self, kept: &serde_json::Map<String, serde_json::Value>) {
        for (record, tags) in kept {
            let Some((app, route)) = record.split_once('\t') else {
                continue;
            };
            let tags: BTreeSet<String> = tags
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tag| Some(tag.as_str()?.to_owned()))
                .collect();
            self.told
                .entry((app.to_owned(), route.to_owned()))
                .or_default()
                .extend(tags);
        }
    }
}

/// What the person's mentions are looked at among: the records a change reached, and what tells
/// the person of them.
#[derive(Clone)]
pub struct Mentions(Rc<Inner>);

pub struct Given {
    /// Every id the person here goes by.
    pub person: Vec<String>,
    /// Where what was told is kept.
    pub store: Option<Store>,
    /// The records of this device, as the host reads them.
    pub records: Option<Rc<dyn Records>>,
    /// The person's notifications, as the host shows them.
    pub notices: Option<Rc<dyn Notices>>,
    /// What this host calls a participant.
    pub name: Rc<dyn Fn(&str) -> String>,
    /// What draws a frame when a notice told the person something.
    pub wake: Option<tilo_service::Wake>,
}

struct Inner {
    me: BTreeSet<String>,
    store: Option<Store>,
    records: Option<Rc<dyn Records>>,
    notices: Option<Rc<dyn Notices>>,
    name: Rc<dyn Fn(&str) -> String>,
    wake: Option<tilo_service::Wake>,
    held: RefCell<Held>,
    lease: RefCell<Option<durable_store::Lease>>,
    /// The records a change reached since the last look.
    pending: RefCell<BTreeSet<Changed>>,
    /// The places the person chose and the host has not opened yet.
    chosen: RefCell<Vec<Place>>,
    /// When the records were last looked at.
    looked_at: Cell<Option<ui::time::Instant>>,
}

impl Mentions {
    /// The person's mentions on this device, over what `given` names.
    #[must_use]
    pub fn over(given: Given) -> Self {
        let Given {
            person,
            store,
            records,
            notices,
            name,
            wake,
        } = given;
        let mut held = Held::default();
        if let Some(kept) = store.as_ref().and_then(|store| read(store.as_ref())) {
            held.restore(&kept);
        }
        Self(Rc::new(Inner {
            me: person.into_iter().filter(|id| !id.is_empty()).collect(),
            store,
            records,
            notices,
            name,
            wake,
            held: RefCell::new(held),
            lease: RefCell::default(),
            pending: RefCell::default(),
            chosen: RefCell::default(),
            looked_at: Cell::default(),
        }))
    }

    /// Look for mentions in the records a change reached since the last pass, and open what the
    /// person chose from a notice.
    pub fn step(&self) {
        self.step_at(ui::time::now());
    }

    /// [`Self::step`] at `now`, as a host's pass reads the clock.
    pub(crate) fn step_at(&self, now: ui::time::Instant) {
        if self.0.notices.is_none() && self.0.records.is_none() {
            return;
        }
        self.chosen_places();
        let Some(records) = &self.0.records else {
            return;
        };
        for record in records.take_changed() {
            self.0.pending.borrow_mut().insert(record);
        }
        if self.0.pending.borrow().is_empty() {
            return;
        }
        let pause = ui::time::Duration::from_millis(LOOK_EVERY_MS);
        if self
            .0
            .looked_at
            .get()
            .is_some_and(|then| now.duration_since(then) < pause)
        {
            // Looked at on a pass once the pause is over.
            self.wake();
            return;
        }
        self.0.looked_at.set(Some(now));
        let pending = std::mem::take(&mut *self.0.pending.borrow_mut());
        let mut kept = false;
        for changed in pending {
            let Some(doc) = records.document(&changed.app, &changed.workspace, &changed.record)
            else {
                continue;
            };
            let tree = ui::mentions::chain_of(&changed.record)
                .and_then(|chain| records.document(&changed.app, &changed.workspace, chain));
            // The kind the record names itself as, else the one its app keeps there.
            let named = match tree.as_ref() {
                Some(tree) => record_kind(tree),
                None => record_kind(&doc),
            };
            let kind = named
                .or_else(|| self.known_kind(&changed.app, &changed.workspace))
                .unwrap_or_default();
            let mut held = self.0.held.borrow_mut();
            // What the workspace held before this device followed the record is not news.
            let quiet = changed.replayed && !held.knows(&changed.app, &changed.record);
            let told = held.look(
                &Look {
                    app: &changed.app,
                    record: &changed.record,
                    kind: &kind,
                    doc: &doc,
                    tree: tree.as_ref(),
                    quiet,
                    // Nobody here can tell what the person holds of what a text refers to: an
                    // excerpt names no resource, never a title they may not read.
                    read: &ui::mentions::reading_nothing,
                },
                &self.0.me,
                &*self.0.name,
            );
            drop(held);
            kept |= !told.show.is_empty() || !told.withdraw.is_empty() || quiet;
            let Some(notices) = &self.0.notices else {
                continue;
            };
            for tag in &told.withdraw {
                notices.withdraw(tag);
            }
            for notice in &told.show {
                notices.show(notice);
            }
        }
        if kept {
            self.keep();
        }
    }

    /// The record kind this host knows `app`'s records by, where the record names none itself.
    fn known_kind(&self, app: &str, workspace: &str) -> Option<String> {
        let records = self.0.records.as_ref()?;
        let kind = records.kind(app, workspace);
        (!kind.is_empty()).then_some(kind)
    }

    /// Take up the places the person chose from a notice, so the host opens them through its own
    /// door: the role knows where they want to be, and the host how to get there.
    fn chosen_places(&self) {
        let Some(notices) = &self.0.notices else {
            return;
        };
        let chosen = notices.take_chosen();
        if !chosen.is_empty() {
            self.0.chosen.borrow_mut().extend(chosen);
        }
    }

    /// The places the person chose since this was last taken, in the order they chose them: what
    /// the host opens through its own door.
    pub fn take_chosen(&self) -> Vec<Place> {
        std::mem::take(&mut *self.0.chosen.borrow_mut())
    }

    /// Wake the host's frame, where one waits for it.
    fn wake(&self) {
        if let Some(wake) = &self.0.wake {
            wake();
        }
    }

    /// Keep what this device told, for this session and the next.
    fn keep(&self) {
        let Some(store) = &self.0.store else {
            return;
        };
        let key = StorageKey::package(PACKAGE);
        let Ok(bytes) =
            serde_json::to_vec(&serde_json::Value::Object(self.0.held.borrow().encoded()))
        else {
            return;
        };
        let mut lease = self.0.lease.borrow_mut();
        if lease.is_none() {
            match store.lease(&key) {
                Ok(held) => *lease = Some(held),
                Err(error) => {
                    tracing::warn!(%error, "the mentions told are not written");
                    return;
                }
            }
        }
        if let Err(error) = store.write(&key, WriteOp::Replace(bytes), None) {
            tracing::warn!(%error, "the mentions told are not written");
        }
    }
}

/// What this device told before it started, as its store kept it.
fn read(store: &dyn DurableStore) -> Option<serde_json::Map<String, serde_json::Value>> {
    let key = StorageKey::package(PACKAGE);
    let stored = match store.read(&key) {
        Ok(stored) => stored?,
        Err(error) => {
            tracing::warn!(%error, "the mentions told are not read");
            return None;
        }
    };
    serde_json::from_slice(&stored.bytes)
        .inspect_err(|error| tracing::warn!(%error, "the mentions told do not read back"))
        .ok()
}

/// The record a mention in the record `look` names opens in, by route, and that record's kind:
/// the chain a part belongs to, else the record itself.
fn place_record(look: &Look<'_>) -> (String, String) {
    let kind_of = |doc: Option<&AutoCommit>| {
        doc.and_then(record_kind)
            .unwrap_or_else(|| look.kind.to_owned())
    };
    match ui::mentions::chain_of(look.record) {
        Some(chain) => (chain.to_owned(), kind_of(look.tree)),
        None => (look.record.to_owned(), kind_of(Some(look.doc))),
    }
}

/// The kind of record `doc` names itself as, when it names one.
fn record_kind(doc: &AutoCommit) -> Option<String> {
    ui::record::identity(doc).map(|identity| identity.kind.as_str().to_owned())
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
