//! Telling the person when someone mentions them, wherever in the workspace.
//!
//! A mention is a run of a record's rich text marked with a member's id, whichever app wrote
//! it ([`ui::mentions::in_resource_for`]). Every record a change reaches is looked at as the change
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
use crate::platform::records::{Changed, Records, Since};

/// The package this device's told state is kept under: the session's.
const PACKAGE: &str = "so.tilo.session.mentions";

/// Least time between two looks, in milliseconds: changes arriving in a burst (a replay, someone
/// typing) are looked at together.
const LOOK_EVERY_MS: u64 = 750;

/// How long a mention's notice stays on screen: what the person was told, not a demand.
const NOTICE_SECONDS: u64 = 8;

/// What a member is called.
pub type Names = Rc<dyn Fn(&ui::MemberId) -> String>;

/// Whether a window on screen shows an item of an app, by the app and the item's address.
pub type OnScreen = Rc<dyn Fn(&str, &str) -> bool>;

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
    /// The record a mention in it opens in, by route: the record itself, or the one whose items
    /// a part keeps (a chat's part, published as the chat's items).
    pub place: &'a str,
    /// The kind of `place`, whose `open` shows an item of it; empty when unknown, and the record
    /// then opens whole.
    pub kind: &'a str,
    /// Its document.
    pub doc: &'a AutoCommit,
    /// The document of `place`, when it is another record than this one.
    pub tree: Option<&'a AutoCommit>,
    /// Take what it holds as seen: the person is looking at it, or it is what the workspace held
    /// before this device first followed it.
    pub quiet: bool,
    /// How a reference in its text reads for the person told: what a notice's excerpt says of it
    /// ([`ui::mentions::reading_nothing`] where nobody here can tell what they hold).
    pub read: ui::mentions::Reader<'a>,
    /// Whether a window on screen shows an item of an app, by the app and the item's address
    /// ([`Given::shown`]).
    pub shown: &'a dyn Fn(&str, &str) -> bool,
}

impl Look<'_> {
    /// Whether a window on screen shows the item at `anchor`, or the conversation `channel` it was
    /// said in: the person sees there what a notice of it would tell.
    fn on_screen(&self, anchor: &str, channel: Option<&str>) -> bool {
        let conversation = channel.filter(|_| !self.kind.is_empty()).map(|channel| {
            tilo_resource::items::item_resource_id_in(
                self.kind,
                ui::mentions::CHANNELS,
                &tilo_resource::Id::from_raw(channel),
            )
        });
        [anchor, conversation.as_deref().unwrap_or_default()]
            .into_iter()
            .any(|anchor| !anchor.is_empty() && (self.shown)(self.app, anchor))
    }

    /// What a notice of `mention` says: who wrote to the person, or who mentioned them and where.
    fn title(
        &self,
        mention: &ui::mentions::Mention,
        name: &dyn Fn(&ui::MemberId) -> String,
    ) -> String {
        let who = mention
            .by
            .as_ref()
            .map_or_else(|| crate::i18n::t("mention-notice-someone"), name);
        let channel = mention.channel.as_deref().and_then(|channel| {
            self.tree
                .and_then(|tree| ui::mentions::channel_name(tree, channel))
        });
        let direct = mention
            .channel
            .as_deref()
            .is_some_and(|channel| ui::MemberId::direct_conversation_members(channel).is_some());
        match (channel, &mention.title) {
            _ if direct => {
                crate::i18n::t_args("mention-notice-direct", &[("name", who.as_str().into())])
            }
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
            (None, None) => crate::i18n::t_args("mention-notice", &[("name", who.as_str().into())]),
        }
    }
}

/// What this device told, and what must not be told again.
#[derive(Default)]
pub struct Held {
    /// Per record (`app` and route), the tags told and still live.
    told: BTreeMap<(String, String), BTreeSet<String>>,
    /// Per record, where it was last looked at: what changed since is all a look reads of it.
    seen: BTreeMap<(String, String), Seen>,
}

/// Where a record was last looked at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Seen {
    /// The heads it stood at.
    heads: Vec<automerge::ChangeHash>,
    /// Whether its mentions were read there, or the record was only taken as seen up to them: what
    /// it held before this device followed it, which is not news and was not read.
    read: bool,
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
        me: &BTreeSet<ui::MemberId>,
        name: &dyn Fn(&ui::MemberId) -> String,
    ) -> Told {
        let route = look.record.to_owned();
        // Each live mention's notice, and whether the person sees what it is in already.
        let mut live_tags: BTreeMap<String, (Notice, bool)> = BTreeMap::new();
        for mention in ui::mentions::in_resource_for(look.doc, look.read) {
            // What the person wrote themselves, on any of their devices, is no news to them.
            if !me.contains(&mention.person)
                || mention.by.as_ref().is_some_and(|by| me.contains(by))
            {
                continue;
            }
            // The item opens through the record's resource, published under its kind, at the
            // address its app publishes the item and a search hit on it opens. Without a kind the
            // item has no address: the record opens.
            let (resource, anchor) = match mention.item.as_ref().filter(|_| !look.kind.is_empty()) {
                Some((field, id)) => (
                    look.kind.to_owned(),
                    tilo_resource::items::item_resource_id_in(
                        look.kind,
                        field,
                        &tilo_resource::Id::from_raw(id.as_str()),
                    ),
                ),
                None => Default::default(),
            };
            let place = Place {
                app: look.app.to_owned(),
                record: look.place.to_owned(),
                resource,
                anchor,
            };
            // Each item is told once: one with no address of its own still is not another's.
            let mut tag = place.tag(&mention.person);
            if let Some((field, id)) = mention.item.as_ref().filter(|_| place.anchor.is_empty()) {
                tag = format!("{tag}/{field}/{id}");
            }
            if live_tags.contains_key(&tag) {
                continue;
            }
            let seen = look.on_screen(&place.anchor, mention.channel.as_deref());
            let notice = Notice::opens(
                tilo_desktop_search::names::app_name(look.app),
                look.title(&mention, name),
                mention.excerpt,
                place,
            )
            .tagged(tag.clone())
            .choosing(vec![(
                OPEN_ACTION.to_owned(),
                crate::i18n::t("mention-notice-open"),
            )])
            // News, not a demand: it leaves the banner by itself and the centre keeps it.
            .staying(Some(ui::time::Duration::from_secs(NOTICE_SECONDS)));
            live_tags.insert(tag, (notice, seen));
        }
        let told = self.told.entry((look.app.to_owned(), route)).or_default();
        let withdraw: Vec<String> = told
            .iter()
            .filter(|tag| !live_tags.contains_key(*tag))
            .cloned()
            .collect();
        for tag in &withdraw {
            told.remove(tag);
        }
        // What the person sees as it arrives counts as told: it is not told once they look away.
        let show = live_tags
            .into_iter()
            .filter(|(tag, _)| told.insert(tag.clone()))
            .filter(|(_, (_, seen))| !look.quiet && !seen)
            .map(|(_, (notice, _))| notice)
            .collect();
        Told { show, withdraw }
    }

    /// What this device told, as it is kept: one entry per record, its app and route then each
    /// tag.
    fn encoded(&self) -> serde_json::Map<String, serde_json::Value> {
        self.told
            .iter()
            .map(|(key, tags)| {
                let seen = self.seen.get(key);
                (
                    format!("{}\t{}", key.0, key.1),
                    serde_json::json!({
                        "told": tags,
                        "heads": seen
                            .map(|seen| seen.heads.iter().map(ToString::to_string).collect::<Vec<_>>())
                            .unwrap_or_default(),
                        "read": seen.is_some_and(|seen| seen.read),
                    }),
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
            let key = (app.to_owned(), route.to_owned());
            let told: BTreeSet<String> = tags["told"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tag| Some(tag.as_str()?.to_owned()))
                .collect();
            // Heads kept in part are no place to follow a record from: without all of them it is
            // read whole at its next change.
            let heads: Result<Vec<automerge::ChangeHash>, String> = tags["heads"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|head| {
                    head.as_str()
                        .ok_or_else(|| "a head is not a string".to_owned())?
                        .parse()
                        .map_err(|error: automerge::ParseChangeHashError| error.to_string())
                })
                .collect();
            let heads = heads.unwrap_or_else(|error| {
                tracing::warn!(%error, %app, %route, "where a record was looked at is not kept");
                Vec::new()
            });
            if !heads.is_empty() {
                let read = tags["read"].as_bool().unwrap_or(false);
                self.seen.insert(key.clone(), Seen { heads, read });
            }
            self.told.entry(key).or_default().extend(told);
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
    /// What this host calls a member.
    pub name: Names,
    /// Whether a window on screen shows an item of an app: a mention there is seen as it arrives,
    /// and no notice tells it again.
    pub shown: OnScreen,
    /// What draws a frame when a notice told the person something.
    pub wake: Option<tilo_service::Wake>,
}

struct Inner {
    me: BTreeSet<ui::MemberId>,
    store: Option<Store>,
    records: Option<Rc<dyn Records>>,
    notices: Option<Rc<dyn Notices>>,
    name: Names,
    shown: OnScreen,
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
            shown,
            wake,
        } = given;
        let mut held = Held::default();
        if let Some(kept) = store.as_ref().and_then(|store| read(store.as_ref())) {
            held.restore(&kept);
        }
        Self(Rc::new(Inner {
            me: person
                .into_iter()
                .filter(|id| !id.is_empty())
                .map(ui::MemberId::person)
                .collect(),
            store,
            records,
            notices,
            name,
            shown,
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
            if let Some(moved) = self.unread(records.as_ref(), &changed) {
                kept |= moved;
                continue;
            }
            let Some(mut doc) = records.document(&changed.app, &changed.workspace, &changed.record)
            else {
                continue;
            };
            let place = ui::mentions::record_of_part(&changed.record);
            let tree =
                place.and_then(|place| records.document(&changed.app, &changed.workspace, place));
            let place = place.unwrap_or(&changed.record);
            // The kind of the record a mention opens in: the one its document names, else, for
            // the app's own record, which names none, the one its app's surface declares first.
            let kind = if place.is_empty() {
                ui::app_surface::primary_kind(&changed.app)
            } else {
                resource_type(tree.as_ref().unwrap_or(&doc))
            }
            .unwrap_or_default();
            let mut held = self.0.held.borrow_mut();
            let key = (changed.app.clone(), changed.record.clone());
            self.read_where_seen(&mut held, &changed, (place, &kind), &mut doc, tree.as_ref());
            // What the workspace held before this device followed the record is not news.
            let quiet = changed.replayed && !held.knows(&changed.app, &changed.record);
            let told = held.look(
                &Look {
                    app: &changed.app,
                    record: &changed.record,
                    place,
                    kind: &kind,
                    doc: &doc,
                    tree: tree.as_ref(),
                    quiet,
                    // Nobody here can tell what the person holds of what a text refers to: an
                    // excerpt names no resource, never a title they may not read.
                    read: &ui::mentions::reading_nothing,
                    shown: &*self.0.shown,
                },
                &self.0.me,
                &*self.0.name,
            );
            let heads = doc.get_heads();
            if heads.is_empty() {
                held.seen.remove(&key);
            } else {
                held.seen.insert(key, Seen { heads, read: true });
            }
            drop(held);
            kept = true;
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

    /// Where `changed`'s record was taken as seen up to some heads and never read, read what it
    /// held there, quietly: none of it is news when it is read now.
    fn read_where_seen(
        &self,
        held: &mut Held,
        changed: &Changed,
        (place, kind): (&str, &str),
        doc: &mut AutoCommit,
        tree: Option<&AutoCommit>,
    ) {
        let key = (changed.app.clone(), changed.record.clone());
        let Some(heads) = held
            .seen
            .get(&key)
            .filter(|seen| !seen.read)
            .map(|seen| seen.heads.clone())
        else {
            return;
        };
        let Ok(before) = doc.fork_at(&heads) else {
            return;
        };
        held.look(
            &Look {
                app: &changed.app,
                record: &changed.record,
                place,
                kind,
                doc: &before,
                tree,
                quiet: true,
                read: &ui::mentions::reading_nothing,
                shown: &*self.0.shown,
            },
            &self.0.me,
            &*self.0.name,
        );
    }

    /// Whether `changed` needs no reading of its record, and if so whether what is kept moved:
    /// `None` when the record must be read whole. A record looked at before is read as what
    /// changed since, and read whole only when that names the person, or takes something away
    /// where a mention of them stands; one the workspace held before this device followed it is
    /// taken as seen where it stands. So a large record's changes cost what they hold.
    fn unread(&self, records: &dyn Records, changed: &Changed) -> Option<bool> {
        let key = (changed.app.clone(), changed.record.clone());
        let mut held = self.0.held.borrow_mut();
        if let Some(seen) = held.seen.get(&key).cloned() {
            let (heads, bytes) = match records.since(
                &changed.app,
                &changed.workspace,
                &changed.record,
                &seen.heads,
            ) {
                Since::Unchanged => return Some(false),
                Since::Whole => return None,
                Since::Changes { heads, bytes } => (heads, bytes),
            };
            let touched = ui::mentions::touched_by(&bytes, &self.0.me)?;
            let has_told = held.told.get(&key).is_some_and(|told| !told.is_empty());
            if touched.names || (touched.removes && has_told) || heads.is_empty() {
                return None;
            }
            held.seen.insert(
                key,
                Seen {
                    heads,
                    read: seen.read,
                },
            );
            return Some(true);
        }
        if !changed.replayed || held.knows(&changed.app, &changed.record) {
            return None;
        }
        let heads = records
            .heads(&changed.app, &changed.workspace, &changed.record)
            .filter(|heads| !heads.is_empty())?;
        held.told.entry(key.clone()).or_default();
        held.seen.insert(key, Seen { heads, read: false });
        Some(true)
    }

    /// Take up the places the person chose from a notice, so the host opens them through its own
    /// API: the role knows where they want to be, and the host how to get there.
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
    /// the host opens through its own API.
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
        let key = StorageKey::app(PACKAGE);
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

/// What the session calls a member in a notice: an agent by the name its workspace's list of
/// agents gives it, whether it is here or not; anyone else by the name the workspace's members go
/// by here.
#[cfg(target_os = "linux")]
pub(crate) fn member_names(
    shell: tilo_desktop_surface::DesktopHandle,
    place_here: Rc<dyn Fn(&mut tilo_desktop_model::Layout)>,
    search: Rc<crate::search::SessionSearch>,
) -> Names {
    Rc::new(move |who: &ui::MemberId| {
        who.agent_id()
            .and_then(|agent| search.agent_name(agent))
            .or_else(|| {
                let mut tree = shell.read(Clone::clone);
                place_here(&mut tree);
                tree.workspaces
                    .iter()
                    .flat_map(|workspace| workspace.members.iter())
                    .find(|member| member.id == *who)
                    .map(|member| member.name.clone())
            })
            .filter(|name| ui::presence::shown_name(name).is_some())
            .unwrap_or_else(|| ui::presence::unnamed_label(who))
    })
}

/// What this device told before it started, as its store kept it.
fn read(store: &dyn DurableStore) -> Option<serde_json::Map<String, serde_json::Value>> {
    let key = StorageKey::app(PACKAGE);
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

/// The kind of record `doc` names itself as, when it names one.
fn resource_type(doc: &AutoCommit) -> Option<String> {
    ui::resource::identity(doc).map(|identity| identity.kind.as_str().to_owned())
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
