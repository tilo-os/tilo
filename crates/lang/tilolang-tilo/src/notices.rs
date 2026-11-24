//! Showing a person their notices, on one device.
//!
//! A notice is a document of the host's `Notice` type, written by `notify` in the change that told
//! the person ([`tilolang_runtime::notify`]). Every device of the workspace reads it like any
//! document; this shows it to that notice's person, and to no one else's, as one
//! [`tilo_service::notice::Notice`].
//!
//! What one device told is kept ([`Notices::kept`]) so a restart tells nothing twice, and what the
//! document held when it first looked is old news ([`Notices::step`]): a document that reached it in a
//! replay tells nothing of what is already there. What the person did with a notice on any device
//! is the document's `seen` field ([`Notices::seen`]), so the devices they were not at take it back.

use std::collections::{BTreeMap, BTreeSet};

use tilo_service::notice::{Notice, Place};
use tilolang::{
    NOTICE_ACTIONS, NOTICE_BODY, NOTICE_DEVICE, NOTICE_OPENS, NOTICE_SEEN, NOTICE_TITLE, NOTICE_TO,
    NOTICE_TYPE,
};
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::{ResourceRef, Value};

use crate::host::Host;

/// The action a notice's `seen` flag is set by: an edit of one of its fields, as any bound
/// control's edit of a field is.
pub const SEEN_ACTION: &str = crate::action::EDIT_FLAG_ACTION;

/// What a notice's notification tag starts with, so a host knows which of its closings are the
/// document's.
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
    /// The instant of the newest notice of theirs the document held when the device first looked:
    /// an older one is not news, whatever else it is.
    from: Option<i64>,
    /// This device's id: a notice naming another device is not shown here. Empty when the host
    /// names no device, and then every notice is.
    device: String,
}

impl Notices {
    /// The notices of `person` at this device, as the device starts: what the document holds the
    /// first time it looks is not news.
    #[must_use]
    pub fn new(person: impl Into<String>) -> Self {
        Self {
            person: person.into(),
            told: BTreeSet::new(),
            showing: BTreeMap::new(),
            from: None,
            device: String::new(),
        }
    }

    /// The device these notices are shown on: a notice naming another device is not shown here.
    pub fn set_device(&mut self, device: impl Into<String>) {
        self.device = device.into();
    }

    /// Take up what this device told before it restarted, as it kept it ([`Self::kept`]): none
    /// of those notices is told again.
    pub fn remember(&mut self, kept: impl IntoIterator<Item = String>) {
        self.told.extend(kept);
    }

    /// What this device told, to keep: the notices it shows no more, whatever the document still
    /// holds of them.
    #[must_use]
    pub fn kept(&self) -> Vec<String> {
        self.told.iter().cloned().collect()
    }

    /// Look at the notices the workspace holds for this person: what to show, and the tags of
    /// those to take back because the person has seen them on this or another device.
    pub fn step(&mut self, host: &Host) -> Told {
        let mut held: Vec<Held> = Vec::new();
        for id in host.store().resources_of(NOTICE_TYPE).unwrap_or_default() {
            let Some(notice) = held_notice(host, &id) else {
                continue;
            };
            if !notice.to.eq_ignore_ascii_case(&self.person) {
                continue;
            }
            if !notice.device.is_empty()
                && !self.device.is_empty()
                && !notice.device.eq_ignore_ascii_case(&self.device)
            {
                continue;
            }
            held.push(notice);
        }
        let newest = held.iter().map(|notice| notice.at).max();
        let from = *self.from.get_or_insert_with(|| newest.unwrap_or(i64::MIN));
        let access: Vec<&Held> = held
            .iter()
            .filter(|notice| !notice.seen && notice.at > from)
            .collect();
        let access_ids: BTreeSet<&str> = access.iter().map(|n| n.id.as_str()).collect();
        let withdraw: Vec<String> = self
            .showing
            .iter()
            .filter(|(id, _)| !access_ids.contains(id.as_str()))
            .map(|(_, tag)| tag.clone())
            .collect();
        self.showing
            .retain(|id, _| access_ids.contains(id.as_str()));
        let mut show: Vec<Notice> = Vec::new();
        for notice in access {
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
    /// seen, so every device of theirs takes it back.
    pub fn seen(&mut self, host: &Host, id: &str) -> Result<(), String> {
        let at = ResourceRef::root(id.to_owned(), NOTICE_TYPE.to_owned());
        let by = crate::runtime::Actor::Member(crate::runtime::ids::MemberId::person(
            self.person.as_str(),
        ));
        host.store()
            .set_register(
                &at,
                NOTICE_SEEN,
                tilolang_runtime::store::Scalar::Bool(true),
                SEEN_ACTION,
                &by,
            )
            .map_err(|error| error.to_string())
    }

    /// The notice this device is showing as the notification `tag`: the id the document keeps it
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

/// A notice as the document holds it.
struct Held {
    id: String,
    to: String,
    title: String,
    body: String,
    opens: ResourceRef,
    seen: bool,
    /// When it was written.
    at: i64,
    /// The device it is for; empty for every device of the person's.
    device: String,
    /// The actions it offers, by the name of each in its app.
    actions: Vec<String>,
}

impl Held {
    /// The notice this notice shows as, and where choosing it lands: the app that keeps what it
    /// opens, the workspace's document of the app, and the resource itself, published as the
    /// session names it.
    fn shown(&self, host: &Host, person: &str) -> Option<Notice> {
        let schema = crate::schema::lookup(&self.opens.ty)?;
        let package = host.package(&schema.app);
        let record = String::new();
        let app = package.as_ref().map_or_else(
            || schema.app.clone(),
            |package| crate::publish::app_name(&*host.runtime().words(), &package.meta),
        );
        // Every published resource opens itself, at itself, as its search hit does.
        let resource = crate::publish::resource_id(&schema.app, &self.opens.id_string());
        let place = Place {
            app: schema.app.clone(),
            record,
            anchor: resource.clone(),
            resource,
        };
        let mut notice = Notice::opens(app, self.title.clone(), self.body.clone(), place)
            .tagged(format!("{NOTICE_TAGS}{}/{person}", self.id));
        // The actions it offers, named as its app names them: one the app does not declare is
        // offered nowhere.
        let offered: Vec<(String, String)> = package.as_ref().map_or_else(Vec::new, |package| {
            self.actions
                .iter()
                .filter_map(|name| {
                    let action = package
                        .meta
                        .actions
                        .iter()
                        .find(|action| action.name == *name)?;
                    let label = if action.label.is_empty() {
                        action.name.clone()
                    } else {
                        action.label.clone()
                    };
                    Some((name.clone(), label))
                })
                .collect()
        });
        if !offered.is_empty() {
            notice = notice.choosing(offered);
        }
        Some(notice)
    }
}

/// The notice `id`, as the document holds it; `None` for a resource the store does not answer for
/// as a notice (one it does not hold, or one whose fields it cannot read).
fn held_notice(host: &Host, id: &str) -> Option<Held> {
    let at = ResourceRef::root(id.to_owned(), NOTICE_TYPE.to_owned());
    let runtime = host.runtime();
    let to = match runtime.read(&at, NOTICE_TO).ok()? {
        Value::Actor(person) => person.member_id()?.person_id()?.to_owned(),
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
    let device = text(NOTICE_DEVICE).unwrap_or_default();
    let actions = match runtime.read(&at, NOTICE_ACTIONS) {
        Ok(Value::List(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Text(name) => Some(name.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
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
        device,
        actions,
    })
}
