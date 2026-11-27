//! What apps hold, found whether or not they run: the device's search index, which its daemon
//! keeps, asked through the device's search ([`Answers`]): a note, a message, a login, by
//! its title, its secondary line and its short fields.
//!
//! A hit runs, by address, the entry's own published action, or the app's `open` at the entry's
//! resource ([`OPEN_ACTION`]); the host invokes it on the running app or launches the app first.
//! Secret-bearing entries (Passwords' logins) are found only while their app last said its
//! secrets are unlocked; while they are locked, a query one of them would match gets one row
//! instead, "Unlock Passwords to search for …", which opens the app to unlock it.
//!
//! The answer comes from the daemon, so a query asks and returns at once; the answer is kept for
//! that query, wakes the shell, and the next pass lists it ([`Source::poll`]).

use std::sync::{Arc, Mutex, PoisonError};

use tilo_actions::remote::Caller;
use tilo_workspace_records::search::{Answers, Found, Within};
use tilo_workspace_records::search::{IndexKind, IndexPerson};
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind, provider_score};

use crate::model::{Hit, InvokeTarget, LaunchPlace, Source, Target};

pub use crate::model::{OPEN_ACTION, OPEN_ANCHOR};

/// The id prefix of an app's unlock row.
const UNLOCK: &str = "unlock";

/// Separates the parts of a row id.
const ID_SEPARATOR: char = '\u{1f}';

/// How many recipients a box lists: the most recent.
const MAX_RECIPIENTS: usize = 500;

/// What the source last asked, and the answer to it.
#[derive(Default)]
struct Asked {
    needle: String,
    /// When it was asked.
    at: Option<ui::time::Instant>,
    found: Option<Vec<Found>>,
    fresh: bool,
}

/// The people, agents and conversations last listed, and whether a listing is on its way.
#[derive(Default)]
struct Listed {
    recipients: Vec<ui::floating_surface::Recipient>,
    generation: u64,
    /// When the listing on its way, or the last one, was asked.
    asked: Option<ui::time::Instant>,
    asking: bool,
}

/// How long a listing of recipients is read before it is asked again.
const LISTED_FOR: ui::time::Duration = ui::time::Duration::from_secs(2);

/// The app an unlock row's id names.
#[must_use]
pub fn unlock_app(id: &str) -> Option<&str> {
    id.strip_prefix(UNLOCK)?.strip_prefix(ID_SEPARATOR)
}

/// What apps hold, from the device's search index.
pub struct IndexedContent {
    search: Arc<dyn Answers>,
    /// Who asks: the person at this session.
    caller: Caller,
    /// What the search is narrowed to: one app, for a window's search.
    within: Within,
    /// The workspace in front, where the search is asked from one: a record of another
    /// workspace is offered to bring here.
    here: Option<String>,
    /// An app's name as the person knows it.
    app_name: Arc<dyn Fn(&str) -> String + Send + Sync>,
    /// The person searching, by their id: a row of what they keep says "you" rather than their
    /// name, and a box never sends them their own message.
    person: String,
    /// What asks the shell for the pass that lists an answer.
    wake: Arc<dyn Fn() + Send + Sync>,
    asked: Arc<Mutex<Asked>>,
    listed: Arc<Mutex<Listed>>,
}

impl IndexedContent {
    /// A source over `search`, asked as `caller` (the person `person`), naming apps by
    /// `app_name`; `wake` asks for the pass that lists an answer.
    pub fn new(
        search: Arc<dyn Answers>,
        caller: Caller,
        app_name: impl Fn(&str) -> String + Send + Sync + 'static,
        person: impl Into<String>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            search,
            caller,
            within: Within::default(),
            here: None,
            app_name: Arc::new(app_name),
            person: person.into(),
            wake,
            asked: Arc::default(),
            listed: Arc::default(),
        }
    }

    /// This source over what `app` alone published, narrowed to `within` — the anchor of one
    /// part of the app (a book, a notebook, a folder) when the search is scoped to it: what a
    /// window's search finds in the app it shows, whether or not the app is running.
    #[must_use]
    pub fn for_app(&self, app: &str, within: Option<String>) -> Self {
        Self {
            search: Arc::clone(&self.search),
            caller: self.caller.clone(),
            within: Within {
                app: Some(app.to_owned()),
                place: within,
                ..Within::default()
            },
            here: self.here.clone(),
            app_name: Arc::clone(&self.app_name),
            person: self.person.clone(),
            wake: Arc::clone(&self.wake),
            asked: Arc::default(),
            listed: Arc::default(),
        }
    }

    /// This source asked from the workspace `here`, sharing what it asked and was answered: a
    /// row of a record another workspace keeps also brings it here.
    #[must_use]
    pub fn here(&self, here: &str) -> Self {
        Self {
            search: Arc::clone(&self.search),
            caller: self.caller.clone(),
            within: self.within.clone(),
            here: Some(here.to_owned()),
            app_name: Arc::clone(&self.app_name),
            person: self.person.clone(),
            wake: Arc::clone(&self.wake),
            asked: Arc::clone(&self.asked),
            listed: Arc::clone(&self.listed),
        }
    }

    fn asked(&self) -> std::sync::MutexGuard<'_, Asked> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn listed(&self) -> std::sync::MutexGuard<'_, Listed> {
        self.listed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The rows of `found`, the answer to `needle`.
    fn rows(&self, found: &[Found], needle: &str) -> Vec<Hit> {
        found
            .iter()
            .filter_map(|found| {
                if found.locked {
                    return Some(self.unlock_row(found, needle));
                }
                self.row(found)
            })
            .collect()
    }

    /// The row for one result: what it is, who it shows, and the action that opens it.
    fn row(&self, found: &Found) -> Option<Hit> {
        let app = found.app.as_deref()?;
        let workspace = found
            .workspace
            .clone()
            .or_else(|| self.caller.workspace().map(str::to_owned))?;
        let subtitle = found.subtitle.clone().or_else(|| match found.about {
            Some(_) => Self::under_name(found),
            None => Some((self.app_name)(app)),
        });
        let (avatar, faces) = shown(found);
        // What the entry is, when its app says something the generic record row would read
        // poorly: its own section, its own glyph, its people at the front and beside it.
        let drawn = (!faces.is_empty() || avatar.is_some()).then(|| {
            ui::search::SearchRow::Command(Box::new(ui::search::CommandRow {
                avatar,
                faces,
                ..ui::search::CommandRow::default()
            }))
        });
        let item = found
            .arguments
            .get(OPEN_ANCHOR)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&found.resource);
        let action = found.open.as_deref().unwrap_or(OPEN_ACTION);
        let score = provider_score(found.rank.unwrap_or(0)).clamp(1, MAX_PROVIDER_SCORE);
        Some(Hit {
            row: drawn.or_else(|| {
                found
                    .item
                    .then(|| ui::search::SearchRow::item_of(app.to_owned(), item))
                    .flatten()
            }),
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: kind(found.about),
            provider: self.id().into(),
            id: found.key.clone().unwrap_or_else(|| {
                format!(
                    "{app}{ID_SEPARATOR}{workspace}{ID_SEPARATOR}{}",
                    found.resource
                )
            }),
            title: found.label.clone(),
            subtitle,
            facts: Vec::new(),
            score,
            icon: found
                .icon
                .as_deref()
                .and_then(tilo_ui_base::icons::Icon::from_name),
            target: Target::Invoke(Box::new(self.bringing(
                found,
                InvokeTarget {
                    arguments: found.arguments.clone(),
                    ..InvokeTarget::action(app, &workspace, &found.resource, action)
                },
            ))),
        })
    }

    /// `opens`, the row of `found`, which also brings the record it is in here when another
    /// workspace keeps it: choosing the row still opens it where it lives.
    fn bringing(&self, found: &Found, opens: InvokeTarget) -> InvokeTarget {
        let (Some(here), Some(from), Some((app, record))) = (
            self.here.as_deref(),
            found.workspace.as_deref(),
            found
                .provider
                .as_deref()
                .and_then(|provider| provider.strip_prefix("record:")?.split_once(':')),
        ) else {
            return opens;
        };
        if here == from || record.is_empty() {
            return opens;
        }
        opens.with_alternate(
            crate::providers::references::BRING_MODIFIER,
            crate::i18n::t("search-bring-here"),
            crate::providers::references::bring(here, from, app, record),
        )
    }

    /// What the entry says under its name when its app left it unsaid: who is in a conversation,
    /// and who a message is from, where, and how long ago.
    fn under_name(found: &Found) -> Option<String> {
        match found.about {
            Some(IndexKind::Conversation) => {
                let names: Vec<&str> = named(&found.people).map(|(name, _)| name).collect();
                (!names.is_empty()).then(|| names.join(", "))
            }
            Some(IndexKind::Message) => {
                let sender = found
                    .people
                    .first()
                    .map(|person| person.name.trim())
                    .filter(|name| !name.is_empty());
                let place = found.place.as_deref();
                let age = found.when.map(|when| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |elapsed| {
                            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
                        });
                    tilo_ui_base::format::relative(now.saturating_sub(when))
                });
                match (sender, place, age) {
                    (Some(sender), Some(place), Some(age)) => Some(crate::i18n::t_args(
                        "search-row-message",
                        &[
                            ("sender", sender.into()),
                            ("conversation", place.into()),
                            ("age", age.into()),
                        ],
                    )),
                    (None, Some(place), Some(age)) => Some(crate::i18n::t_args(
                        "search-row-message-unnamed",
                        &[("conversation", place.into()), ("age", age.into())],
                    )),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// One row offering to unlock the app `found` names, for the `needle` its locked entries
    /// match.
    fn unlock_row(&self, found: &Found, needle: &str) -> Hit {
        let app = found.app.as_deref().unwrap_or(&found.resource);
        let name = (self.app_name)(app);
        Hit {
            // The row says what was typed back, in quotes: nothing in it is a match.
            row: Some(ui::search::SearchRow::echoing_query()),
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ResourceKind::Record,
            provider: self.id().into(),
            id: format!("{UNLOCK}{ID_SEPARATOR}{app}"),
            title: crate::i18n::t_args(
                "search-unlock-app",
                &[
                    ("app", name.as_str().into()),
                    ("query", needle.trim().into()),
                ],
            ),
            subtitle: Some(crate::i18n::t_args(
                "search-unlock-app-detail",
                &[("app", name.as_str().into())],
            )),
            facts: Vec::new(),
            score: provider_score(found.rank.unwrap_or(0)).max(1),
            icon: Some(tilo_ui_base::icons::Icon::Lock),
            target: Target::Launch {
                app: app.to_owned(),
                place: LaunchPlace::Focus,
            },
        }
    }

    /// The agents, people and conversations a message can go to, the most recent first: what a
    /// box that sends a message finds whom it goes to in. The list is the one last read, and is
    /// read again on each call: [`Self::generation`] moves once a newer one arrives.
    #[must_use]
    pub fn recipients(&self) -> Vec<ui::floating_surface::Recipient> {
        self.ask_recipients();
        self.listed().recipients.clone()
    }

    /// A number that moves whenever what [`Self::recipients`] lists did.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.listed().generation
    }

    /// Ask the device's search for the agents, people and conversations, unless it is asked
    /// already.
    fn ask_recipients(&self) {
        {
            let mut listed = self.listed();
            let now = ui::time::now();
            let recent = listed
                .asked
                .is_some_and(|at| now.saturating_duration_since(at) < LISTED_FOR);
            if listed.asking || recent {
                return;
            }
            listed.asking = true;
            listed.asked = Some(now);
        }
        let listed = Arc::clone(&self.listed);
        let wake = Arc::clone(&self.wake);
        let person = self.person.clone();
        let within = Within {
            about: vec![IndexKind::Agent, IndexKind::Person, IndexKind::Conversation],
            ..self.within.clone()
        };
        self.search
            .find_within(&self.caller, "", &within)
            .then(move |found| {
                let recipients =
                    recipients_of(&person, &found.and_then(Result::ok).unwrap_or_default());
                let mut held = listed.lock().unwrap_or_else(PoisonError::into_inner);
                held.asking = false;
                if held.recipients != recipients {
                    held.recipients = recipients;
                    held.generation = held.generation.wrapping_add(1);
                    drop(held);
                    wake();
                }
            });
    }
}

/// The section a result belongs to: what it is, when its app says, else the records an app keeps.
fn kind(about: Option<IndexKind>) -> ResourceKind {
    match about {
        Some(IndexKind::Agent) => ResourceKind::Custom(crate::i18n::t("search-kind-agents")),
        Some(IndexKind::Conversation) => {
            ResourceKind::Custom(crate::i18n::t("search-kind-conversations"))
        }
        Some(IndexKind::Message) => ResourceKind::Custom(crate::i18n::t("search-kind-messages")),
        Some(IndexKind::Person) => ResourceKind::Custom(crate::i18n::t("search-kind-people")),
        None => ResourceKind::Record,
    }
}

/// Each of `people` who has a name, with it.
fn named(people: &[IndexPerson]) -> impl Iterator<Item = (&str, &IndexPerson)> {
    people
        .iter()
        .map(|person| (person.name.trim(), person))
        .filter(|(name, _)| !name.is_empty())
}

/// A person as a row's face stack draws them.
fn face(name: &str, person: &IndexPerson) -> ui::search::RowFace {
    ui::search::RowFace::of_member(name.to_owned(), person.identity.clone(), person.agent)
}

/// The avatar and the faces of the row a result draws: an agent or a message shows its person in
/// front, a conversation shows the people in it beside its name.
fn shown(found: &Found) -> (Option<ui::search::RowFace>, Vec<ui::search::RowFace>) {
    let mut people = named(&found.people);
    match found.about {
        Some(IndexKind::Agent | IndexKind::Message | IndexKind::Person) => (
            people.next().map(|(name, person)| face(name, person)),
            Vec::new(),
        ),
        Some(IndexKind::Conversation) => (
            None,
            people.map(|(name, person)| face(name, person)).collect(),
        ),
        None => (None, Vec::new()),
    }
}

/// The recipients `found` names, the most recent first, each once: a direct conversation and the
/// agent in it are one, and the line under it is whichever of them has one.
fn recipients_of(person: &str, found: &[Found]) -> Vec<ui::floating_surface::Recipient> {
    let mut out: Vec<ui::floating_surface::Recipient> = Vec::new();
    for recipient in found.iter().filter_map(|found| recipient(person, found)) {
        match out
            .iter_mut()
            .find(|kept| kept.kind == recipient.kind && kept.id == recipient.id)
        {
            Some(kept) => {
                if kept.detail.as_deref().is_none_or(str::is_empty) {
                    kept.detail = recipient.detail;
                }
            }
            None => out.push(recipient),
        }
    }
    out.truncate(MAX_RECIPIENTS);
    out
}

/// The recipient `found` names, when it names one a message can go to: an agent the workspace
/// keeps, a person, or a conversation. The `person` sitting at this session is no recipient of
/// their own message.
fn recipient(person: &str, found: &Found) -> Option<ui::floating_surface::Recipient> {
    use ui::floating_surface::{Recipient, RecipientKind};
    let who = found.people.first();
    let recipient = match found.about? {
        IndexKind::Agent => {
            let agent = who?.identity.as_str();
            agent.starts_with("agent:").then_some(())?;
            Recipient::found(RecipientKind::Agent, agent, &found.label)
        }
        IndexKind::Person => {
            let who = who?;
            if who.identity == person {
                return None;
            }
            Recipient::found(party_kind(who), &who.identity, &found.label)
                .with_detail(found.subtitle.clone().unwrap_or_default())
        }
        IndexKind::Conversation => {
            // A direct conversation is the one other person or agent in it: a message to them
            // goes there, so a box lists them once.
            let others: Vec<&IndexPerson> = found
                .people
                .iter()
                .filter(|other| other.identity != person)
                .collect();
            if let ([_, _], [other]) = (found.people.as_slice(), others.as_slice()) {
                let name = Some(other.name.as_str()).filter(|name| !name.is_empty());
                return Some(Recipient::found(
                    party_kind(other),
                    &other.identity,
                    name.unwrap_or(&found.label),
                ));
            }
            // The conversation is the resource the row opens: what a message to it is sent in.
            let id = found
                .arguments
                .get(OPEN_ANCHOR)
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&found.resource);
            let id = Some(id).filter(|id| !id.is_empty())?;
            let people: Vec<&str> = named(&found.people).map(|(name, _)| name).collect();
            Recipient::found(RecipientKind::Conversation, id, &found.label).with_detail(
                if people.is_empty() {
                    found.subtitle.clone().unwrap_or_default()
                } else {
                    people.join(", ")
                },
            )
        }
        IndexKind::Message => return None,
    };
    Some(recipient)
}

/// Whether a message to `person` goes to an agent or a person.
fn party_kind(person: &IndexPerson) -> ui::floating_surface::RecipientKind {
    if person.agent || person.identity.starts_with("agent:") {
        ui::floating_surface::RecipientKind::Agent
    } else {
        ui::floating_surface::RecipientKind::Person
    }
}

impl Source for IndexedContent {
    fn id(&self) -> &'static str {
        "app-content"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        {
            let mut asked = self.asked();
            if asked.needle == needle {
                return asked
                    .found
                    .as_deref()
                    .map_or_else(Vec::new, |found| self.rows(found, needle));
            }
            *asked = Asked {
                needle: needle.to_owned(),
                at: Some(ui::time::now()),
                found: None,
                fresh: false,
            };
        }
        let answer = Arc::clone(&self.asked);
        let wake = Arc::clone(&self.wake);
        let asking = needle.to_owned();
        self.search
            .find_within(&self.caller, needle, &self.within)
            .then(move |found| {
                let mut asked = answer.lock().unwrap_or_else(PoisonError::into_inner);
                // An answer to a query typed over since is no row of the one shown.
                if asked.needle != asking {
                    return;
                }
                asked.found = Some(found.and_then(Result::ok).unwrap_or_default());
                asked.fresh = true;
                drop(asked);
                wake();
            });
        // A search that answered at once is listed at once.
        let asked = self.asked();
        asked
            .found
            .as_deref()
            .map_or_else(Vec::new, |found| self.rows(found, needle))
    }

    fn poll(&self) -> bool {
        std::mem::take(&mut self.asked().fresh)
    }

    fn waiting(&self) -> bool {
        let asked = self.asked();
        asked.found.is_none()
            && asked.at.is_some_and(|at| {
                ui::time::now().saturating_duration_since(at) < crate::providers::hosted::PATIENCE
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_workspace_search::{AppContentIndex, IndexSearch};
    use ui::search::index_publication::{IndexAction, IndexEntry, IndexState, Publisher};

    /// This source over `index`, as the device's search answers it for `person`, who may read it
    /// all.
    fn over(
        index: &Arc<AppContentIndex>,
        app_name: impl Fn(&str) -> String + Send + Sync + 'static,
        person: &str,
    ) -> IndexedContent {
        let search = Arc::new(IndexSearch::new(
            Arc::clone(index),
            tilo_workspace_search::deciding(|_, _| true),
        ));
        IndexedContent::new(
            search,
            Caller::local_person(person).in_workspace("workspace"),
            app_name,
            person,
            Arc::new(|| {}),
        )
    }

    fn note(key: &str, title: &str, secret: bool) -> IndexEntry {
        IndexEntry {
            key: key.into(),
            resource: key.into(),
            action: None,
            title: title.into(),
            subtitle: None,
            group: None,
            icon: None,
            boost: 0,
            fields: Vec::new(),
            secret,
            references: Vec::new(),
            ..IndexEntry::default()
        }
    }

    fn publish(
        index: &AppContentIndex,
        app: &str,
        unlocked: Option<bool>,
        entries: Vec<IndexEntry>,
    ) {
        let mut publisher = Publisher::default();
        let state = IndexState {
            generation: 1,
            secrets_unlocked: unlocked,
        };
        for delta in publisher.publish(state, entries) {
            index
                .publish(
                    "workspace",
                    app,
                    "tab:0001",
                    &delta.encode().expect("a delta encodes"),
                )
                .expect("the index takes the delta");
        }
    }

    tilo_test_env::scenario! {
        fn an_indexed_entry_reads_as_what_it_is_and_opens_what_it_names;

        phase a_window_reads_only_its_own_apps_entries;
        // A window's search is over the one app it shows: what that app published, never another's.
        {
            let index = Arc::new(AppContentIndex::in_memory());
            publish(
                &index,
                "so.tilo.chat",
                None,
                vec![note("c_voice", "voice", false)],
            );
            publish(
                &index,
                "so.tilo.mail",
                None,
                vec![note("inbox", "Inbox", false)],
            );
            let content = over(&index, str::to_owned, "");
            let window = content.for_app("so.tilo.chat", None);
            assert_eq!(window.query("voice").len(), 1);
            assert!(
                window.query("inbox").is_empty(),
                "another app's entry is not the window's"
            );
            assert_eq!(
                content.query("inbox").len(),
                1,
                "the whole index still finds it"
            );
        }

        phase a_window_scoped_to_one_resource_finds_only_its_items;
        // A search opened on a book, a notebook or a folder narrows what its app published to
        // the entries that live in it (`IndexEntry::owner`), by the address the scope names.
        {
            let index = Arc::new(AppContentIndex::in_memory());
            let chapter = |key: &str, owner: &str, title: &str| IndexEntry {
                owner: Some(owner.to_owned()),
                ..note(key, title, false)
            };
            publish(
                &index,
                "so.tilo.ebook",
                None,
                vec![
                    chapter("tl:so.tilo.ebook:book1/ch1", "tl:so.tilo.ebook:book1", "A river"),
                    chapter("tl:so.tilo.ebook:book1/ch2", "tl:so.tilo.ebook:book1", "A river's mouth"),
                    chapter("tl:so.tilo.ebook:book2/ch1", "tl:so.tilo.ebook:book2", "A river elsewhere"),
                ],
            );
            let content = over(&index, str::to_owned, "");
            let book = content.for_app("so.tilo.ebook", Some("tl:so.tilo.ebook:book1".into()));
            let titles = |content: &IndexedContent| -> Vec<String> {
                content
                    .query("river")
                    .into_iter()
                    .map(|hit| hit.title)
                    .collect()
            };
            assert_eq!(
                titles(&book),
                ["A river", "A river's mouth"],
                "only the chapters inside the book the search was scoped to"
            );
            assert_eq!(
                titles(&content).len(),
                3,
                "the app's whole search still finds every book's passages"
            );
        }

        phase a_hit_on_an_item_asks_its_record_to_open_it;
        // An item's entry names the `open` of the record holding it, which every window showing
        // anything of the record publishes, and the hit tells that `open` the item: one no window
        // shows still opens.
        {
            let index = Arc::new(AppContentIndex::in_memory());
            let mut task = note("tl:tasks:doc/t1", "Book the ferry", false);
            task.action = Some(IndexAction {
                resource: "tl:tasks:doc".into(),
                action: OPEN_ACTION.into(),
            });
            publish(&index, "tasks", None, vec![task]);
            let content = over(&index, str::to_owned, "");
            let hits = content.query("ferry");
            let Target::Invoke(target) = &hits[0].target else {
                panic!("an entry runs its record's open");
            };
            assert_eq!(
                target.address,
                tilo_actions::ActionAddress::new("tl:tasks:doc", OPEN_ACTION)
            );
            assert_eq!(target.arguments[OPEN_ANCHOR], "tl:tasks:doc/t1");
        }

        phase a_hit_opens_its_app_at_the_entry_by_address;
        {
            let index = Arc::new(AppContentIndex::in_memory());
            publish(
                &index,
                "so.tilo.notes",
                None,
                vec![note("n1", "Groceries", false)],
            );
            let content = over(&index, |app| format!("{app}!"), "");
            let hits = content.query("groceries");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].subtitle.as_deref(), Some("so.tilo.notes!"));
            let Target::Invoke(target) = &hits[0].target else {
                panic!("an entry runs its app's open");
            };
            assert_eq!(
                target.address,
                tilo_actions::ActionAddress::new("n1", OPEN_ACTION)
            );
            assert_eq!(target.arguments[OPEN_ANCHOR], "n1");
        }

        phase an_entry_that_names_an_item_carries_its_reference;
        // An entry an app publishes as an item of a resource (a note in a
        // notebook) is a reference: the row says which item it is, so the
        // palette can draw it as the app lists it, and it opens there. An entry
        // that names no item is drawn as the generic row.
        {
            let index = Arc::new(AppContentIndex::in_memory());
            let mut page = note("page-7", "Trip plan", false);
            page.resource = "notes.notebook/pages/page-7".into();
            publish(&index, "so.tilo.notes", None, vec![page.clone()]);
            let content = over(&index, |_| String::new(), "");
            let hits = content.query("trip");
            assert_eq!(hits.len(), 1);
            assert_eq!(
                hits[0].row,
                Some(ui::search::SearchRow::item(
                    "so.tilo.notes",
                    "notes.notebook.pages",
                    "page-7"
                ))
            );
            // The row addresses the resource the hit opens.
            assert_eq!(
                hits[0]
                    .row
                    .as_ref()
                    .and_then(ui::search::SearchRow::item_resource)
                    .as_deref(),
                Some(page.resource.as_str())
            );
        }

        phase what_an_entry_is_names_the_section_it_sits_in;
        {
            let index = Arc::new(AppContentIndex::in_memory());
            publish(
                &index,
                "so.tilo.chat",
                None,
                vec![
                    shaped(
                        note("chat.agent/agents/a_ada", "Ada", false),
                        IndexKind::Agent,
                        vec![person(
                            "Ada",
                            "agent:u_ada/a_ada",
                            "chat.agent/agents/a_ada",
                            true,
                        )],
                    ),
                    shaped(
                        note("chat.workspace/channels/c_crit", "Ada's design crit", false),
                        IndexKind::Conversation,
                        vec![person("Ada", "agent:u_ada/a_ada", "", true)],
                    ),
                    shaped(
                        note("chat.workspace/messages/m_1", "Ada, can you look?", false),
                        IndexKind::Message,
                        vec![person("Ada", "agent:u_ada/a_ada", "", true)],
                    ),
                ],
            );
            let content = over(&index, str::to_owned, "");
            let kinds: Vec<ResourceKind> = content
                .query("ada")
                .into_iter()
                .map(|hit| hit.kind)
                .collect();
            let labels: Vec<String> = kinds
                .iter()
                .map(|kind| match kind {
                    ResourceKind::Custom(label) => label.clone(),
                    other => other.label().to_owned(),
                })
                .collect();
            assert_eq!(labels, ["Agents", "Conversations", "Messages"]);
            // The groups keep that order whatever their strongest hit.
            let mut sorted = kinds.clone();
            sorted.sort_by_key(ResourceKind::sort_key);
            assert_eq!(
                sorted.first().map(ResourceKind::sort_key),
                kinds.first().map(ResourceKind::sort_key)
            );
        }

        phase an_agent_reads_as_itself;
        {
            let index = Arc::new(AppContentIndex::in_memory());
            let mut agent = shaped(
                note("chat.agent/agents/a_ada", "Ada", false),
                IndexKind::Agent,
                vec![person(
                    "Ada",
                    "agent:u_ada/a_ada",
                    "chat.agent/agents/a_ada",
                    true,
                )],
            );
            agent.icon = Some("sparkles".into());
            publish(&index, "so.tilo.chat", None, vec![agent]);
            let content = over(&index, str::to_owned, "u_me");
            let hits = content.query("ada");
            assert_eq!(hits.len(), 1);
            assert_eq!(
                hits[0].subtitle, None,
                "an agent is the workspace's: its row names no member it is kept by"
            );
            let Some(ui::search::SearchRow::Command(row)) = &hits[0].row else {
                panic!("an agent is drawn with its own face: {:?}", hits[0].row);
            };
            assert_eq!(
                row.avatar,
                Some(ui::search::RowFace::of_member("Ada", "agent:u_ada/a_ada", true))
            );
        }

        phase a_conversation_lists_the_people_in_it;
        {
            let index = Arc::new(AppContentIndex::in_memory());
            publish(
                &index,
                "so.tilo.chat",
                None,
                vec![shaped(
                    note("chat.workspace/channels/c_crit", "Ada's design crit", false),
                    IndexKind::Conversation,
                    vec![
                        person("Aria", "u_aria", "", false),
                        person("Ada", "agent:u_ada/a_ada", "", true),
                    ],
                )],
            );
            let content = over(&index, str::to_owned, "u_me");
            let hits = content.query("crit");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].subtitle.as_deref(), Some("Aria, Ada"));
            let Some(ui::search::SearchRow::Command(row)) = &hits[0].row else {
                panic!("a conversation shows its people: {:?}", hits[0].row);
            };
            let names: Vec<&str> = row.faces.iter().map(|face| face.name.as_str()).collect();
            assert_eq!(names, ["Aria", "Ada"]);
            assert!(row.faces[1].is_agent);
            assert_eq!(row.avatar, None);
        }

        phase a_box_finds_agents_people_and_conversations_never_messages_or_itself;
        // A box's recipients are what the workspace's apps publish of their
        // agents, people and conversations: never a message, never the person
        // asking, each once — and an empty query opens on what is there, the
        // most recent first.
        {
            use ui::floating_surface::RecipientKind;

            let index = Arc::new(AppContentIndex::in_memory());
            let mut agent = shaped(
                note("chat.agent/agents/a_ada", "Ada", false),
                IndexKind::Agent,
                vec![person(
                    "Ada",
                    "agent:u_ada/a_ada",
                    "chat.agent/agents/a_ada",
                    true,
                )],
            );
            agent.when = Some(20);
            let mut conversation = shaped(
                note("chat.workspace/channels/c_ada", "Ada", false),
                IndexKind::Conversation,
                vec![
                    person("Me", "u_me", "", false),
                    person("Ada", "agent:u_ada/a_ada", "", true),
                ],
            );
            conversation.when = Some(30);
            publish(
                &index,
                "so.tilo.chat",
                None,
                vec![
                    agent,
                    conversation,
                    shaped(
                        note("chat.workspace/people/u_me", "Me", false),
                        IndexKind::Person,
                        vec![person(
                            "Me",
                            "u_me",
                            "chat.workspace/people/u_me",
                            false,
                        )],
                    ),
                    shaped(
                        note("chat.workspace/people/u_ada", "Ada", false),
                        IndexKind::Person,
                        vec![person(
                            "Ada",
                            "u_ada",
                            "chat.workspace/people/u_ada",
                            false,
                        )],
                    ),
                    shaped(
                        note("chat.workspace/messages/m_ada", "Ada, can you look?", false),
                        IndexKind::Message,
                        vec![person("Ada", "u_ada", "", false)],
                    ),
                ],
            );
            let content = over(&index, str::to_owned, "u_me");

            // Typing a name: what it names, never the message that mentions it
            // and never the person typing.
            let found = ui::floating_surface::recipients_matching(&content.recipients(), "ada");
            let listed: Vec<(RecipientKind, &str)> = found
                .iter()
                .map(|recipient| (recipient.kind, recipient.id.as_str()))
                .collect();
            assert_eq!(
                listed,
                [
                    (RecipientKind::Agent, "agent:u_ada/a_ada"),
                    (RecipientKind::Person, "u_ada"),
                ],
                "the direct conversation with the agent is the agent, listed once"
            );

            // Before anything is typed, what the workspace holds, most recent
            // first.
            let held = content.recipients();
            let all: Vec<(RecipientKind, &str)> = held
                .iter()
                .map(|recipient| (recipient.kind, recipient.id.as_str()))
                .collect();
            assert_eq!(
                all,
                [
                    (RecipientKind::Agent, "agent:u_ada/a_ada"),
                    (RecipientKind::Person, "u_ada"),
                ]
            );
        }

        phase a_box_finds_an_agent_by_name_among_many;
        // Of a workspace's 55 agents, the one called what was typed leads, then
        // those whose name starts with it, then those with a word that does.
        {
            let index = Arc::new(AppContentIndex::in_memory());
            let mut entries = Vec::new();
            let mut agent = |id: &str, name: &str, when: i64| {
                let mut entry = shaped(
                    note(&format!("chat.agent/agents/{id}"), name, false),
                    IndexKind::Agent,
                    vec![person(name, &format!("agent:u_me/{id}"), "", true)],
                );
                entry.when = Some(when);
                entries.push(entry);
            };
            for number in 0..52 {
                agent(
                    &format!("a_{number}"),
                    &format!("Helper {number}"),
                    100 + i64::from(number),
                );
            }
            agent("a_weekly", "Weekly digest", 1);
            agent("a_digester", "Digester", 2);
            agent("a_digest", "Digest", 3);
            publish(&index, "so.tilo.chat", None, entries);
            let content = over(&index, str::to_owned, "u_me");

            let all = content.recipients();
            let ids = |query: &str| -> Vec<String> {
                ui::floating_surface::recipients_matching(&all, query)
                    .into_iter()
                    .map(|recipient| recipient.id)
                    .collect()
            };
            assert_eq!(
                ids("Digest")[..3],
                ["agent:u_me/a_digest", "agent:u_me/a_digester", "agent:u_me/a_weekly"]
            );
            assert_eq!(
                ids("helper 7").first().map(String::as_str),
                Some("agent:u_me/a_7")
            );
            assert_eq!(ids("helper 4")[0], "agent:u_me/a_4");
        }

        phase a_message_says_who_said_it_where_and_when;
        {
            let index = Arc::new(AppContentIndex::in_memory());
            let text = "We shipped the new palette yesterday and the animations still stutter \
                    when the window resizes, and the wheel was reported by three more \
                    people this morning; Ada, can you look at the wheel again before the \
                    demo tomorrow morning? The venue needs the final slide deck today, \
                    and Priya has the numbers in a sheet nobody can open yet.";
            let mut message = shaped(
                note("chat.workspace/messages/m_wheel", text, false),
                IndexKind::Message,
                vec![person(
                    "",
                    "u_aria",
                    "chat.workspace/people/u_aria",
                    false,
                )],
            );
            message.place = Some(person(
                "",
                "c_crit",
                "chat.workspace/channels/c_crit",
                false,
            ));
            message.when = Some(
                i64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("the clock is after the epoch")
                        .as_secs(),
                )
                .expect("seconds fit")
                    - 3600,
            );
            publish(
                &index,
                "so.tilo.chat",
                None,
                vec![
                    shaped(
                        note("chat.workspace/people/u_aria", "Aria", false),
                        IndexKind::Person,
                        vec![person("Aria", "u_aria", "", false)],
                    ),
                    shaped(
                        note("chat.workspace/channels/c_crit", "Ada's design crit", false),
                        IndexKind::Conversation,
                        vec![person("Aria", "u_aria", "", false)],
                    ),
                    message,
                ],
            );
            let content = over(&index, str::to_owned, "u_me");
            let hits = content.query("ada");
            let hit = hits
                .iter()
                .find(|hit| hit.title.contains("can you look"))
                .expect("the message is found by what it says");
            assert_eq!(
                hit.subtitle.as_deref(),
                Some(format!("Aria in Ada's design crit, {}", tilo_ui_base::format::relative(3600)).as_str())
            );
            assert!(
                hit.title.starts_with('\u{2026}'),
                "a long message shows the line around the match: {:?}",
                hit.title
            );
            let Some(ui::search::SearchRow::Command(row)) = &hit.row else {
                panic!("a message is drawn with its sender's face: {:?}", hit.row);
            };
            assert_eq!(
                row.avatar,
                Some(ui::search::RowFace::of_member("Aria", "u_aria", false))
            );
            let Target::Invoke(target) = &hit.target else {
                panic!("a message opens where it lives");
            };
            assert_eq!(target.address.resource, "chat.workspace/messages/m_wheel");
            assert_eq!(
                target.arguments[OPEN_ANCHOR],
                "chat.workspace/messages/m_wheel"
            );
        }

        phase a_locked_vault_offers_only_to_unlock_it;
        {
            let index = Arc::new(AppContentIndex::in_memory());
            publish(
                &index,
                "so.tilo.passwords",
                Some(false),
                vec![note("l1", "GitHub", true)],
            );
            let content = over(&index, str::to_owned, "");
            let hits = content.query("github");
            assert_eq!(hits.len(), 1);
            assert_eq!(
                hits[0].title,
                "Unlock so.tilo.passwords to search for “github”"
            );
            assert_eq!(
                hits[0].target,
                Target::Launch {
                    app: "so.tilo.passwords".into(),
                    place: LaunchPlace::Focus,
                }
            );
        }
    }

    /// A person as an entry names them.
    fn person(name: &str, identity: &str, anchor: &str, agent: bool) -> IndexPerson {
        IndexPerson {
            name: name.into(),
            anchor: anchor.into(),
            identity: identity.into(),
            agent,
        }
    }

    /// `entry` as what an app says it is: its kind, the people it is about,
    /// who keeps it, where it is, and when.
    fn shaped(entry: IndexEntry, kind: IndexKind, people: Vec<IndexPerson>) -> IndexEntry {
        IndexEntry {
            kind: Some(kind),
            people,
            ..entry
        }
    }
}
