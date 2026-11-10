//! Showing a person their notices, on one device.
//!
//! A notice is a record of the host's `Notice` type, written by `notify` in the change that told
//! the person ([`tilolang_runtime::notify`]): it says who it is for, what it says, what choosing
//! it opens, and whether they have seen it. Every device of the workspace reads it like any
//! record; this shows it to that notice's person, and to no one else's, through the notifications
//! the device's session serves — the same ones every app's notification goes through.
//!
//! What one device told is kept ([`Notices::kept`]) so a restart tells nothing twice, and what
//! the record held when the device first looked is taken as old news ([`Notices::step`]): a
//! record that reached it in a replay, or one it joined late, tells nothing of what is already
//! there. What the person did with a notice on any device — dismissed it, or opened what it
//! names — is the record's `seen` field ([`Notices::seen`]), so the devices they were not at
//! take the notification back. Each notice is one of the session's own
//! ([`tilo_session_core::notice::Notice`]), shown by whichever host runs this device.

use std::collections::{BTreeMap, BTreeSet};

use tilo_session_core::notice::{Notice, Place};
use tilolang::{
    HOST_APP, NOTICE_BODY, NOTICE_OPENS, NOTICE_SEEN, NOTICE_TITLE, NOTICE_TO, NOTICE_TYPE,
};
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::{ResourceRef, Value};

use crate::host::Host;

/// The action a notice's `seen` flag is set by: an edit of one of its fields, as any bound
/// control's edit of a field is.
pub const SEEN_ACTION: &str = crate::action::EDIT_FLAG_ACTION;

/// What a notice's notification tag starts with, so a host knows which of its closings are the
/// record's.
pub const NOTICE_TAGS: &str = "notice:";

/// The notices of one person on one device.
#[derive(Clone, Debug)]
pub struct Notices {
    /// The person at this device: whose notices these are, and whose changes they are seen by.
    person: String,
    /// The notices this device told, and kept: none of them is told again.
    told: BTreeSet<String>,
    /// The notices it is showing, by the tag each shows under.
    showing: BTreeMap<String, String>,
    /// The instant of the newest notice of theirs the record held when the device first looked:
    /// an older one is not news, whatever else it is.
    from: Option<i64>,
}

impl Notices {
    /// The notices of `person` at this device, as the device starts: what the record holds the
    /// first time it looks is not news.
    #[must_use]
    pub fn new(person: impl Into<String>) -> Self {
        Self {
            person: person.into(),
            told: BTreeSet::new(),
            showing: BTreeMap::new(),
            from: None,
        }
    }

    /// Take up what this device told before it restarted, as it kept it ([`Self::kept`]): none
    /// of those notices is told again.
    pub fn remember(&mut self, kept: impl IntoIterator<Item = String>) {
        self.told.extend(kept);
    }

    /// What this device told, to keep: the notices it shows no more, whatever the record still
    /// holds of them.
    #[must_use]
    pub fn kept(&self) -> Vec<String> {
        self.told.iter().cloned().collect()
    }

    /// Look at the notices the workspace holds for this person: what to show, and the tags of
    /// those to take back because the person has seen them on this or another device.
    pub fn step(&mut self, host: &Host) -> Told {
        let mut held: Vec<Held> = Vec::new();
        for id in host.store().resources_of(HOST_APP, NOTICE_TYPE) {
            let Some(notice) = held_notice(host, &id) else {
                continue;
            };
            if !notice.to.eq_ignore_ascii_case(&self.person) {
                continue;
            }
            held.push(notice);
        }
        let newest = held.iter().map(|notice| notice.at).max();
        let from = *self.from.get_or_insert_with(|| newest.unwrap_or(i64::MIN));
        let standing: Vec<&Held> = held
            .iter()
            .filter(|notice| !notice.seen && notice.at > from)
            .collect();
        let standing_ids: BTreeSet<&str> = standing.iter().map(|n| n.id.as_str()).collect();
        let withdraw: Vec<String> = self
            .showing
            .iter()
            .filter(|(id, _)| !standing_ids.contains(id.as_str()))
            .map(|(_, tag)| tag.clone())
            .collect();
        self.showing
            .retain(|id, _| standing_ids.contains(id.as_str()));
        let mut show: Vec<Notice> = Vec::new();
        for notice in standing {
            if self.told.contains(&notice.id) || self.showing.contains_key(&notice.id) {
                continue;
            }
            if let Some(shown) = notice.shown(host, &self.person) {
                self.told.insert(notice.id.clone());
                self.showing.insert(notice.id.clone(), shown.tag.clone());
                show.push(shown);
            }
        }
        Told { show, withdraw }
    }

    /// The person at this device saw the notice `id`, by opening or dismissing it: it is marked
    /// seen in the record, so every device of theirs takes it back. What is written waits for
    /// the host to commit it (`Host::take_committed`).
    pub fn seen(&mut self, host: &Host, id: &str) -> Result<(), String> {
        host.store()
            .see(id, &self.person)
            .map_err(|error| error.to_string())
    }

    /// The notice this device is showing as the notification `tag`: the id the record keeps it
    /// under.
    #[must_use]
    pub fn shown_as(&self, tag: &str) -> Option<&str> {
        self.showing
            .iter()
            .find(|(_, shown)| shown.as_str() == tag)
            .map(|(id, _)| id.as_str())
    }
}

/// What one step told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    /// The notices to show, none of them shown before.
    pub show: Vec<Notice>,
    /// The tags to take back.
    pub withdraw: Vec<String>,
}

/// A notice as the record holds it.
struct Held {
    id: String,
    to: String,
    title: String,
    body: String,
    opens: ResourceRef,
    seen: bool,
    /// When it was written.
    at: i64,
}

impl Held {
    /// The notice this notice shows as, and where choosing it lands: the app that keeps what it
    /// opens, the record of the workspace that person's app keeps (their own, for an app that
    /// keeps one), and the resource itself, published as the session names it.
    fn shown(&self, host: &Host, person: &str) -> Option<Notice> {
        let schema = crate::schema::lookup(&self.opens.ty)?;
        let package = host.package(&schema.app);
        let record = package
            .as_ref()
            .and_then(|package| crate::own_record(&package.meta, person))
            .unwrap_or_default();
        let app = package
            .as_ref()
            .map_or_else(|| schema.app.clone(), |package| package.meta.title.clone());
        let place = Place {
            app: schema.app.clone(),
            record,
            resource: crate::publish::resource_id(&schema.app, &self.opens.id_string()),
        };
        Some(
            Notice::opens(app, self.title.clone(), self.body.clone(), place)
                .tagged(format!("{NOTICE_TAGS}{}/{person}", self.id)),
        )
    }
}

/// The notice `id`, as the record holds it; `None` for a resource the store does not answer for
/// as a notice (one it does not hold, or one whose fields it cannot read).
fn held_notice(host: &Host, id: &str) -> Option<Held> {
    let at = ResourceRef::root(id.to_owned(), NOTICE_TYPE.to_owned());
    let runtime = host.runtime();
    let to = match runtime.read(&at, NOTICE_TO).ok()? {
        Value::Principal(person) => person.0,
        _ => return None,
    };
    let text = |field: &str| match runtime.read(&at, field) {
        Ok(Value::Text(text)) => Some(text),
        _ => None,
    };
    let opens = match runtime.read(&at, NOTICE_OPENS).ok()? {
        Value::Ref(reference) => ResourceRef::clone(&reference.target),
        _ => return None,
    };
    let seen = matches!(runtime.read(&at, NOTICE_SEEN), Ok(Value::Bool(true)));
    let at_instant = host
        .store()
        .version_end(&at, false)
        .ok()
        .flatten()
        .and_then(|version| tilolang::calendar::instant(&version.at))?;
    Some(Held {
        id: id.to_owned(),
        to,
        title: text(NOTICE_TITLE)?,
        body: text(NOTICE_BODY).unwrap_or_default(),
        opens,
        seen,
        at: at_instant,
    })
}
