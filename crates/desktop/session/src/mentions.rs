//! Telling the person when someone mentions them, wherever in the workspace.
//!
//! A mention is a run of a record's rich text marked with a member's id, whichever app wrote
//! it ([`tilo_ui_base::mentions::in_resource_for`]). Every record a change reaches is looked at as the change
//! arrives, and each new mention of the person here is one [`Notice`]. A mention is never told
//! twice, is taken back when it is edited away, and one that was already there when this device
//! started following the record is not news. What was told is kept in one of the device's records,
//! so a restart tells nothing again.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::task::Poll;

use tilo_session_client::device_records::{Arrived, DeviceRecord};
use ui::automerge::AutoCommit;

use crate::platform::notice::{Notice, Notices, OPEN_ACTION, Place};
use crate::platform::records::{Changed, Records, Since};

/// The device record what this device told is kept in.
pub(crate) const KEPT: &str = "so.tilo.session.mentions";

/// Least time between two looks, in milliseconds: changes arriving in a burst (a replay, someone
/// typing) are looked at together.
const LOOK_EVERY_MS: u64 = 750;

/// How long a mention's notice stays on screen: what the person was told, not a demand.
const NOTICE_SECONDS: u64 = 8;

/// What a member is called.
pub type Names = Rc<dyn Fn(&tilo_ui_base::MemberId) -> String>;

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
    /// The record, as its route names it: empty for the app's own record of the workspace. A
    /// mention in it opens there.
    pub record: &'a str,
    /// The kind of the record, whose `open` shows an item of it; empty when unknown, and the
    /// record then opens whole.
    pub kind: &'a str,
    /// Its document.
    pub doc: &'a AutoCommit,
    /// Take what it holds as seen: the person is looking at it, or it is what the workspace held
    /// before this device first followed it.
    pub quiet: bool,
    /// How a reference in its text reads for the person told: what a notice's excerpt says of it
    /// ([`tilo_ui_base::mentions::reading_nothing`] where nobody here can tell what they hold).
    pub read: tilo_ui_base::mentions::Reader<'a>,
    /// Whether a window on screen shows an item of an app, by the app and the item's address
    /// ([`Given::shown`]).
    pub shown: &'a dyn Fn(&str, &str) -> bool,
}

impl Look<'_> {
    /// Whether a window on screen shows the item at `anchor`: the person sees there what a notice
    /// of it would tell.
    fn on_screen(&self, anchor: &str) -> bool {
        !anchor.is_empty() && (self.shown)(self.app, anchor)
    }
}

/// What a notice of `mention` says: who mentioned the person, and where.
fn notice_title(mention: &tilo_ui_base::mentions::Mention, name: &dyn Fn(&tilo_ui_base::MemberId) -> String) -> String {
    let who = mention
        .by
        .as_ref()
        .map_or_else(|| crate::i18n::t("mention-notice-someone"), name);
    match &mention.title {
        Some(title) => crate::i18n::t_args(
            "mention-notice-title",
            &[
                ("name", who.as_str().into()),
                ("place", title.as_str().into()),
            ],
        ),
        None => crate::i18n::t_args("mention-notice", &[("name", who.as_str().into())]),
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
        me: &BTreeSet<tilo_ui_base::MemberId>,
        name: &dyn Fn(&tilo_ui_base::MemberId) -> String,
    ) -> Told {
        let route = look.record.to_owned();
        // Each live mention's notice, and whether the person sees what it is in already.
        let mut live_tags: BTreeMap<String, (Notice, bool)> = BTreeMap::new();
        for mention in tilo_ui_base::mentions::in_resource_for(look.doc, look.read) {
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
            // A record the workspace's store keeps opens by its id, which its document's address
            // names.
            let record = tilo_resource_store::content_resource(look.record)
                .map_or(look.record, |(_, id)| id);
            let place = Place {
                app: look.app.to_owned(),
                record: record.to_owned(),
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
            let seen = look.on_screen(&place.anchor);
            let notice = Notice::opens(
                tilo_desktop_search::names::app_name(look.app),
                notice_title(&mention, name),
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
    /// The device record what was told is kept in.
    pub kept: Option<DeviceRecord>,
    /// The records of this device, as the host reads them.
    pub records: Option<Reads>,
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
    me: BTreeSet<tilo_ui_base::MemberId>,
    kept: Option<DeviceRecord>,
    /// What another of the person's tools wrote to the record since the last pass.
    arrived: Arrived,
    records: Option<Reads>,
    notices: Option<Rc<dyn Notices>>,
    name: Names,
    shown: OnScreen,
    wake: Option<tilo_service::Wake>,
    held: RefCell<Held>,
    /// The records a change reached since the last look.
    pending: RefCell<BTreeSet<Changed>>,
    /// What was looked at and whose read has not landed yet.
    arriving: RefCell<BTreeSet<Changed>>,
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
            kept,
            records,
            notices,
            name,
            shown,
            wake,
        } = given;
        let me = people(&person);
        let records = records.map(|reads| reads.started(&me, wake.clone()));
        let mut held = Held::default();
        if let Some(told) = kept.as_ref().and_then(read) {
            held.restore(&told);
        }
        // What was told is followed while the session runs: what is marked told elsewhere is not
        // told here again.
        let (kept, arrived) = match (kept, &wake) {
            (Some(record), Some(wake)) => match record.follow_waking(std::sync::Arc::clone(wake)) {
                Ok((record, arrived)) => (Some(record), arrived),
                Err(error) => {
                    tracing::warn!(%error, "the mentions told are not followed");
                    (Some(record), Arrived::default())
                }
            },
            (kept, _) => (kept, Arrived::default()),
        };
        Self(Rc::new(Inner {
            me,
            kept,
            arrived,
            records,
            notices,
            name,
            shown,
            wake,
            held: RefCell::new(held),
            pending: RefCell::default(),
            arriving: RefCell::default(),
            chosen: RefCell::default(),
            looked_at: Cell::default(),
        }))
    }

    /// Look for mentions in the records a change reached since the last pass, and open what the
    /// person chose from a notice.
    pub fn step(&self) {
        self.step_at(ui::time::now());
    }

    /// [`Self::step`] at `now`, as a pass reads the clock. Never waits for a read: what it reads
    /// is looked at again on the pass after the read lands.
    pub(crate) fn step_at(&self, now: ui::time::Instant) {
        if self.0.notices.is_none() && self.0.records.is_none() {
            return;
        }
        self.chosen_places();
        if let Some(Some(told)) = self.0.arrived.take()
            && let Ok(told) = serde_json::from_value(told)
                .inspect_err(|error| tracing::warn!(%error, "the mentions told do not read back"))
        {
            self.0.held.borrow_mut().restore(&told);
        }
        let Some(records) = &self.0.records else {
            return;
        };
        for changed in records.take_changed() {
            // Changed again while its read was out: read again, as what lands of the earlier
            // read is not what it holds now.
            records.renoted(&changed);
            self.0
                .arriving
                .borrow_mut()
                .retain(|waiting| route_of(waiting) != route_of(&changed));
            self.0.pending.borrow_mut().insert(changed);
        }
        let mut looking = std::mem::take(&mut *self.0.arriving.borrow_mut());
        if !self.0.pending.borrow().is_empty() {
            let pause = ui::time::Duration::from_millis(LOOK_EVERY_MS);
            if self
                .0
                .looked_at
                .get()
                .is_some_and(|then| now.duration_since(then) < pause)
            {
                // Looked at on a pass once the pause is over.
                self.wake();
            } else {
                self.0.looked_at.set(Some(now));
                looking.extend(std::mem::take(&mut *self.0.pending.borrow_mut()));
            }
        }
        let mut kept = false;
        for changed in looking {
            match self.look_at(records, &changed) {
                Poll::Ready(moved) => kept |= moved,
                Poll::Pending => {
                    self.0.arriving.borrow_mut().insert(changed);
                }
            }
        }
        if kept {
            self.keep();
        }
    }

    /// Look for the person's mentions in what `changed` names, as far as its read landed: whether
    /// what is kept moved, or pending while the read is out.
    fn look_at(&self, reads: &Reads, changed: &Changed) -> Poll<bool> {
        let key = (changed.app.clone(), changed.record.clone());
        let ask = {
            let held = self.0.held.borrow();
            let seen = held.seen.get(&key);
            Ask {
                seen: seen.map(|seen| seen.heads.clone()),
                told: held.told.get(&key).is_some_and(|told| !told.is_empty()),
                unknown: changed.replayed && !held.knows(&changed.app, &changed.record),
            }
        };
        let mut doc = match reads.fetch(changed, ask, &self.0.me) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Fetched::Gone | Fetched::Unchanged) => return Poll::Ready(false),
            Poll::Ready(Fetched::Moved(heads)) => {
                let mut held = self.0.held.borrow_mut();
                let read = held.seen.get(&key).is_some_and(|seen| seen.read);
                held.seen.insert(key, Seen { heads, read });
                return Poll::Ready(true);
            }
            Poll::Ready(Fetched::Before(heads)) => {
                let mut held = self.0.held.borrow_mut();
                held.told.entry(key.clone()).or_default();
                held.seen.insert(key, Seen { heads, read: false });
                return Poll::Ready(true);
            }
            Poll::Ready(Fetched::Whole(whole)) => *whole,
        };
        // The kind of the record a mention opens in: the one the store founded it as, which its
        // document's address names, else the one its document names, else, for the app's own
        // record, which names none, the one its app's surface declares first.
        let kind = match tilo_resource_store::content_resource(&changed.record) {
            Some((kind, _)) => Some(kind.to_owned()),
            None if changed.record.is_empty() => ui::app_surface::primary_kind(&changed.app),
            None => resource_type(&doc),
        }
        .unwrap_or_default();
        let mut held = self.0.held.borrow_mut();
        self.read_where_seen(&mut held, changed, &kind, &mut doc);
        // What the workspace held before this device followed the record is not news.
        let quiet = changed.replayed && !held.knows(&changed.app, &changed.record);
        let told = held.look(
            &Look {
                app: &changed.app,
                record: &changed.record,
                kind: &kind,
                doc: &doc,
                quiet,
                // Nobody here can tell what the person holds of what a text refers to: an
                // excerpt names no resource, never a title they may not read.
                read: &tilo_ui_base::mentions::reading_nothing,
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
        if let Some(notices) = &self.0.notices {
            for tag in &told.withdraw {
                notices.withdraw(tag);
            }
            for notice in &told.show {
                notices.show(notice);
            }
        }
        Poll::Ready(true)
    }

    /// Where `changed`'s record was taken as seen up to some heads and never read, read what it
    /// held there, quietly: none of it is news when it is read now.
    fn read_where_seen(
        &self,
        held: &mut Held,
        changed: &Changed,
        kind: &str,
        doc: &mut AutoCommit,
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
                kind,
                doc: &before,
                quiet: true,
                read: &tilo_ui_base::mentions::reading_nothing,
                shown: &*self.0.shown,
            },
            &self.0.me,
            &*self.0.name,
        );
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
        if let Some(kept) = &self.0.kept {
            kept.set(serde_json::Value::Object(self.0.held.borrow().encoded()));
        }
    }
}

/// What the session calls a member in a notice: an agent by the name its workspace's list of
/// agents gives it, whether it is here or not; anyone else by the name the workspace's members go
/// by here.
#[cfg(target_os = "linux")]
pub(crate) fn member_names(
    shell: tilo_session_core::desktop::state::DesktopHandle,
    place_here: Rc<dyn Fn(&mut tilo_desktop_model::Layout)>,
    search: Rc<crate::search::SessionSearch>,
) -> Names {
    Rc::new(move |who: &tilo_ui_base::MemberId| {
        Some(who)
            .filter(|who| who.is_agent())
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
            .filter(|name| tilo_ui_base::presence::shown_name(name).is_some())
            .unwrap_or_else(|| tilo_ui_base::presence::unnamed_label(who))
    })
}

/// A document by its workspace, app and route: what one read of it is kept by.
type Route = (String, String, String);

fn route_of(changed: &Changed) -> Route {
    (
        changed.workspace.clone(),
        changed.app.clone(),
        changed.record.clone(),
    )
}

/// What a look at a document needs read of it, by what the role holds of it.
#[derive(Clone, Debug)]
struct Ask {
    /// The heads it was last looked at, when it was.
    seen: Option<Vec<automerge::ChangeHash>>,
    /// Whether a mention in it was told.
    told: bool,
    /// It is what the workspace held before this device followed it, and was never looked at.
    unknown: bool,
}

/// What was read of a document for a look at it.
enum Fetched {
    /// Nothing to read: it is gone, or not held here.
    Gone,
    /// It stands where it was last looked at.
    Unchanged,
    /// It moved to these heads, and nothing in what changed names the person.
    Moved(Vec<automerge::ChangeHash>),
    /// What the workspace held before this device followed it, at these heads: taken as seen
    /// there, unread.
    Before(Vec<automerge::ChangeHash>),
    /// It whole.
    Whole(Box<AutoCommit>),
}

/// Read what a look at `changed` needs of `from`, as `ask` says. A document looked at before is
/// read as what changed since, and read whole only when that names the person, or takes
/// something away where a mention of them stands; one the workspace held before this device
/// followed it is taken as seen where it stands. So a large document's changes cost what they
/// hold.
fn fetch(from: &dyn Records, changed: &Changed, ask: &Ask, me: &BTreeSet<tilo_ui_base::MemberId>) -> Fetched {
    let (app, workspace, route) = (
        changed.app.as_str(),
        changed.workspace.as_str(),
        changed.record.as_str(),
    );
    if let Some(seen) = &ask.seen {
        match from.since(app, workspace, route, seen) {
            Since::Unchanged => return Fetched::Unchanged,
            Since::Whole => {}
            Since::Changes { heads, bytes } => {
                let apart = tilo_ui_base::mentions::touched_by(&bytes, me).is_some_and(|touched| {
                    !(touched.names || (touched.removes && ask.told) || heads.is_empty())
                });
                if apart {
                    return Fetched::Moved(heads);
                }
            }
        }
    } else if ask.unknown
        && let Some(heads) = from
            .heads(app, workspace, route)
            .filter(|heads| !heads.is_empty())
    {
        return Fetched::Before(heads);
    }
    let Some(doc) = from.document(app, workspace, route) else {
        return Fetched::Gone;
    };
    Fetched::Whole(Box::new(doc))
}

/// How the role reads the workspaces' documents: on the thread that steps it where a read
/// answers at once, as a page's store does, or on a worker of its own where a read may wait on a
/// workspace still opening, as this device's service does. A frame never waits for one.
pub struct Reads(Way);

enum Way {
    Here(Rc<dyn Records>),
    /// Read on a worker once the role starts it.
    #[cfg(target_os = "linux")]
    Apart(crate::platform::records::Source),
    #[cfg(target_os = "linux")]
    Behind(Behind),
}

/// Reads made on a worker, each answered when it lands.
#[cfg(target_os = "linux")]
struct Behind {
    from: crate::platform::records::Source,
    asks: std::sync::mpsc::Sender<(Changed, u64, Ask)>,
    landed: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<Route, (u64, Fetched)>>>,
    /// The read out for each document, by its ticket.
    out: RefCell<std::collections::HashMap<Route, u64>>,
    tickets: Cell<u64>,
}

impl Reads {
    /// Reads of `from` on the thread that steps the role: where a read answers at once.
    #[must_use]
    pub fn here(from: Rc<dyn Records>) -> Self {
        Self(Way::Here(from))
    }

    /// Reads of `from` on a worker of their own.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn apart(from: crate::platform::records::Source) -> Self {
        Self(Way::Apart(from))
    }

    /// Reads of what this device's workspace service holds, through `reads`, with `changes`
    /// saying what moved: on a worker, as a read there waits on a workspace still opening.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn of_service(
        reads: std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        changes: crate::platform::records::Changes,
    ) -> Self {
        Self::apart(std::sync::Arc::new(crate::platform::records::Held::over(
            reads, changes,
        )))
    }

    /// Start the worker these reads are made on, reading for `me` and waking the session's loop
    /// through `wake` as each lands.
    fn started(self, me: &BTreeSet<tilo_ui_base::MemberId>, wake: Option<tilo_service::Wake>) -> Self {
        #[cfg(target_os = "linux")]
        if let Way::Apart(from) = self.0 {
            use std::sync::{Arc, Mutex, PoisonError, mpsc};
            let (asks, asked) = mpsc::channel::<(Changed, u64, Ask)>();
            let landed: Arc<Mutex<std::collections::HashMap<Route, (u64, Fetched)>>> =
                Arc::default();
            let me = me.clone();
            let (reading, answers) = (Arc::clone(&from), Arc::clone(&landed));
            let worker = std::thread::Builder::new()
                .name("mentions".to_owned())
                .spawn(move || {
                    // Ends when the role does.
                    while let Ok((changed, ticket, ask)) = asked.recv() {
                        let fetched = fetch(reading.as_ref(), &changed, &ask, &me);
                        answers
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(route_of(&changed), (ticket, fetched));
                        if let Some(wake) = &wake {
                            wake();
                        }
                    }
                });
            if let Err(error) = worker {
                tracing::warn!(%error, "the person's mentions are not read");
            }
            return Self(Way::Behind(Behind {
                from,
                asks,
                landed,
                out: RefCell::default(),
                tickets: Cell::default(),
            }));
        }
        let _ = (me, wake);
        self
    }

    fn take_changed(&self) -> Vec<Changed> {
        match &self.0 {
            Way::Here(from) => from.take_changed(),
            #[cfg(target_os = "linux")]
            Way::Apart(from) => from.take_changed(),
            #[cfg(target_os = "linux")]
            Way::Behind(behind) => behind.from.take_changed(),
        }
    }

    /// `changed`'s document changed again: a read of it out now reads what it held before.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            clippy::unused_self,
            reason = "a page reads its mentions at once and keeps no route"
        )
    )]
    fn renoted(&self, changed: &Changed) {
        #[cfg(target_os = "linux")]
        if let Way::Behind(behind) = &self.0 {
            let route = route_of(changed);
            behind.out.borrow_mut().remove(&route);
            behind
                .landed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&route);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = changed;
    }

    /// What `ask` reads of `changed`'s document: at once here, or pending until the worker's read
    /// of it lands.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            clippy::needless_pass_by_value,
            reason = "the desktop's pending read takes the ask; a page's answers at once"
        )
    )]
    fn fetch(&self, changed: &Changed, ask: Ask, me: &BTreeSet<tilo_ui_base::MemberId>) -> Poll<Fetched> {
        match &self.0 {
            Way::Here(from) => Poll::Ready(fetch(from.as_ref(), changed, &ask, me)),
            #[cfg(target_os = "linux")]
            Way::Apart(from) => Poll::Ready(fetch(from.as_ref(), changed, &ask, me)),
            #[cfg(target_os = "linux")]
            Way::Behind(behind) => {
                let route = route_of(changed);
                let mut out = behind.out.borrow_mut();
                if let Some(ticket) = out.get(&route).copied() {
                    let landed = behind
                        .landed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&route);
                    return match landed {
                        Some((at, fetched)) if at == ticket => {
                            out.remove(&route);
                            Poll::Ready(fetched)
                        }
                        // What landed of a read before it changed again: the one out now answers.
                        _ => Poll::Pending,
                    };
                }
                let ticket = behind.tickets.get().wrapping_add(1);
                behind.tickets.set(ticket);
                if behind.asks.send((changed.clone(), ticket, ask)).is_err() {
                    // The worker is gone with the session.
                    return Poll::Ready(Fetched::Gone);
                }
                out.insert(route, ticket);
                Poll::Pending
            }
        }
    }
}

/// The members `ids` name, without the empty one.
fn people(ids: &[String]) -> BTreeSet<tilo_ui_base::MemberId> {
    ids.iter()
        .filter(|id| !id.is_empty())
        .map(tilo_ui_base::MemberId::person)
        .collect()
}

/// What this device told before it started, as its record kept it.
fn read(kept: &DeviceRecord) -> Option<serde_json::Map<String, serde_json::Value>> {
    let value = match kept.read() {
        Ok(value) => value?,
        Err(error) => {
            tracing::warn!(%error, "the mentions told are not read");
            return None;
        }
    };
    serde_json::from_value(value)
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
