//! The record of what operated a tab's page, as a resource of Browser's.
//!
//! Several actors may work in one tab, and the engine sets down every action
//! that operated its page: who asked, what was done, the element it acted on
//! by role and name, what it wrote and how it ended. The engine's own copy
//! lives with the page and ends with it. This is where it is kept:
//! [`PageRecord`], a record Browser keeps in the workspace it is open in,
//! beside its browsing history. Each action is one recorded change of it,
//! added after the tab's others and authored by the actor the action door
//! admitted, so History steps through a tab action by action, a recording is
//! a stretch of that log, and another device that holds the workspace reads
//! the same record.
//!
//! A window keeps none of it. What it holds ([`TabRecorder`]) is where its
//! engine's record stands against the resource, which it loses with the
//! engine and finds again in the resource.
//!
//! Only a window adds to it, and only what its engine listed. Setting an
//! action down is Browser's own writing (`#[action(by_app)]`), not an action
//! of the record: no caller is offered it, and the record's door refuses it
//! by name, as a value and in a group. So an actor cannot set down what it
//! never did, and a reviewer reads who did what.
//!
//! Never a secret: what was written into a secret field is not in what the
//! engine lists, whatever wrote it, and the entry says so (`secret`). The
//! record keeps of such an entry that the field was set and none of its
//! text, whatever the entry carries; and text the window itself sent as a
//! secret is taken out of whatever the engine lists before it is written
//! ([`TabRecorder::admitted`]).
//!
//! What a person types straight into the page is no action and no answer
//! carries it: the engine sets it down and says so of its own accord
//! ([`TabRecorder::heard`]). It is written in its place among the actions
//! around it, as the person's.
//!
//! What a window an agent operates held is no action either. The engine sets
//! down that something the page was about to send waits for the person (a
//! form submission, a request with data, a download), and then how that
//! ended: let leave, kept as the page is, dropped with nobody answering, or
//! turned away at once as one more than the window asks the person about.
//! Such an entry has `how: "hold"`. One that says a hold began names what
//! waits as its `action`, where it would go as `to_url` and its method there
//! as its message, under the actor whose action reached it. One that says how
//! it ended names who answered: the engine cannot know that, so the window
//! says it with the answer ([`TabRecorder::acts_as`]). Neither carries what
//! was about to leave.
//!
//! The record is bounded as the engine bounds its own: a tab keeps its newest
//! [`MAX_TAB_ACTIONS`] actions and the record its [`MAX_TABS`] most recently
//! used tabs. What goes beyond leaves the record's state, counted in the
//! tab's `dropped`, and stays in its history, where History still shows it.

use std::time::Instant;

use serde_json::Value;
use ui::framework::PersonRecord;
use ui::tilo_actions::{ActionInvoker, ActorKind};
use ui::tilo_resource::{self, ActionCx, Atom, Map, Model, Refusal, resource};
use ui::webview::{PageActor, PageFailure, PageRecordNote, PageReply, PageRequest};

/// How many actions of one tab the record holds: the newest, as the engine
/// holds them.
pub const MAX_TAB_ACTIONS: usize = 1000;
/// How many tabs the record holds: the ones that acted last.
pub const MAX_TABS: usize = 32;
/// How many actions one read answers with, as the engine answers.
pub const MAX_READ: usize = 200;
/// The longest message, written text, chosen value and answer kept, in
/// bytes: what the engine keeps of them.
const MAX_TEXT_BYTES: usize = 200;
/// The longest name kept (an actor, an action, a role, an element's name).
const MAX_NAME_BYTES: usize = 512;
/// The longest address kept.
const MAX_URL_BYTES: usize = 2048;
/// The most options one action chose that are kept.
const MAX_VALUES: usize = 64;
/// The most answers of one form that are kept.
const MAX_ANSWERS: usize = 200;
/// The longest tab id.
const MAX_TAB_ID_BYTES: usize = 64;
/// How many actors a window remembers the admitted invoker of.
const MAX_ACTORS: usize = 64;
/// How many secrets a window remembers it sent, to keep them out.
const MAX_WITHHELD: usize = 256;

/// One answer a form action placed: the name the form asks under and what
/// was placed.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// The name the form asks under.
    pub name: Atom,
    /// What was placed.
    pub value: Atom,
}

/// One action that operated a tab's page, as the tab's engine set it down
/// when the action ended.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct PageAction {
    /// Its place in the tab's record.
    pub seq: u64,
    /// When it ended, in milliseconds since the Unix epoch.
    pub time_ms: u64,
    /// The name its actor acted under; empty for the person at the window.
    pub actor: Atom,
    /// `input` for what was typed straight into the page and `hold` for what
    /// the window held the page from sending; empty for an action.
    pub how: Atom,
    /// What was asked for (`click`, `fill`, `select`). Of a hold, what was
    /// held (`submit`, `request`, `download`) or how it ended (`allow`,
    /// `keep`, `drop`, `too_many`).
    pub action: Atom,
    /// Whether it did what was asked.
    pub ok: bool,
    /// Why it did not.
    pub code: Atom,
    /// What the page answered.
    #[model(label)]
    pub message: Atom,
    /// The page it ran on.
    pub url: Atom,
    /// The page it left that one for. Of a hold, where what was held would
    /// go, by its origin.
    pub to_url: Atom,
    /// Whether it led the page away.
    pub navigation: Atom,
    /// The frame it ran in.
    pub frame: Atom,
    /// The role of the element it acted on.
    pub role: Atom,
    /// The name of that element.
    pub name: Atom,
    /// Which element of that role and name, counted from 1; 0 for the only
    /// one.
    pub nth: u64,
    /// It wrote into a secret field: what it wrote is in the page and
    /// nowhere else, and the record holds none of it.
    pub secret: bool,
    /// The text it wrote. Never a secret's.
    pub text: Option<Atom>,
    /// The options it chose.
    pub values: Option<Vec<Atom>>,
    /// The answers it placed in a form.
    pub answers: Vec<Answer>,
}

/// What the record keeps about one tab beside its actions.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Tab {
    /// The place of the newest action set down for the tab.
    pub last: u64,
    /// How many of its oldest actions have left the record.
    pub dropped: u64,
    /// When its newest action ended, in milliseconds since the Unix epoch.
    pub at: u64,
    /// The device whose engine runs the tab's page.
    pub device: Atom,
}

/// The record of what operated Browser's tabs, in the workspace Browser is
/// open in: each action by its tab and its place in that tab's record.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct PageRecord {
    /// The actions, by tab and place. Only setting one down changes them.
    #[model(readonly)]
    pub actions: Map<PageAction>,
    /// The tabs that have actions here, by their id.
    #[model(readonly)]
    pub tabs: Map<Tab>,
}

/// A part of one tab's record: what a read of it answers with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordPage {
    /// The actions after the place asked for, oldest first, as the engine
    /// lists them.
    pub entries: Vec<Value>,
    /// The place to read on from.
    pub next: u64,
    /// Whether more actions follow these.
    pub more: bool,
    /// How many of the tab's oldest actions have left the record.
    pub dropped: u64,
}

#[resource(
    kind = "browser.page-record",
    label = "Page record",
    params = page_actions
)]
impl PageRecord {
    /// Set down a page action.
    ///
    /// One action that operated a tab's page, as the tab's engine listed it
    /// when the action ended. It is added after the tab's others and changes
    /// none of them; only what the record keeps of an action is taken from
    /// the entry.
    ///
    /// Only the window whose engine listed the action sets it down (its
    /// `TabRecorder`). It is the browser keeping its record: not an action
    /// anyone invokes, nor an edit of anyone's to take back. No caller is
    /// offered it and the record's door refuses it, so nobody sets down an
    /// action the engine did not list.
    ///
    /// The window writes each action as whoever did it: the entry's actor
    /// is the name of the invoker its change is authored by, and what a
    /// person typed or did at the window is authored by a person. An entry
    /// under another's name is refused.
    #[action(housekeeping, by_app)]
    pub fn set_down(
        &mut self,
        cx: &mut ActionCx,
        #[param("The id of the tab the action ran in")] tab: String,
        #[param("The device whose engine runs the tab's page")] device: String,
        #[param("The action as the engine listed it, with its place in the tab's record as `seq`")]
        entry: serde_json::Value,
    ) -> Result<(), Refusal> {
        if !valid_tab(&tab) {
            return Err(Refusal::new("a tab is named by its id"));
        }
        let action = PageAction::of(&entry)
            .ok_or_else(|| Refusal::new("a page action has a place and says what was done"))?;
        if !action.is_by(cx.author(), cx.actor_kind()) {
            return Err(Refusal::new(
                "a page action is set down by the actor who did it",
            ));
        }
        let known = self.tabs.get(&tab).cloned();
        if known.as_ref().is_some_and(|known| action.seq <= known.last) {
            return Err(Refusal::new("the tab's record already holds that place"));
        }
        // The tab that acted longest ago makes room for a new one.
        while known.is_none() && self.tabs.len() >= MAX_TABS {
            let Some(oldest) = self
                .tabs
                .iter()
                .min_by_key(|(id, kept)| (kept.at, (*id).clone()))
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.forget_tab(&oldest);
        }
        let mut kept = known.unwrap_or_default();
        kept.last = action.seq;
        kept.at = kept.at.max(action.time_ms);
        kept.device = Atom(clipped(&device, MAX_NAME_BYTES));
        self.actions.insert(action_key(&tab, action.seq), action);
        // A tab keeps its newest actions; the oldest leave the record's
        // state and stay in its history.
        let held = self.keys_of(&tab);
        let over = held.len().saturating_sub(MAX_TAB_ACTIONS);
        for key in held.into_iter().take(over) {
            self.actions.remove(&key);
            kept.dropped = kept.dropped.saturating_add(1);
        }
        self.tabs.insert(tab, kept);
        Ok(())
    }
}

impl PageRecord {
    /// The actions of `tab` after the place `since`, oldest first: at most
    /// [`MAX_READ`], with the place to read on from.
    pub fn page(&self, tab: &str, since: u64) -> RecordPage {
        let prefix = tab_prefix(tab);
        let mut page = RecordPage {
            next: since,
            dropped: self.tabs.get(tab).map_or(0, |kept| kept.dropped),
            ..RecordPage::default()
        };
        for (_, action) in self
            .actions
            .iter()
            .filter(|(key, action)| key.starts_with(&prefix) && action.seq > since)
        {
            if page.entries.len() >= MAX_READ {
                page.more = true;
                break;
            }
            page.next = action.seq;
            page.entries.push(action.entry());
        }
        page
    }

    /// The place of the newest action set down for `tab`; 0 for none.
    pub fn last(&self, tab: &str) -> u64 {
        self.tabs.get(tab).map_or(0, |kept| kept.last)
    }

    /// The keys of `tab`'s actions, oldest first.
    fn keys_of(&self, tab: &str) -> Vec<String> {
        let prefix = tab_prefix(tab);
        self.actions
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect()
    }

    /// Take `tab` and its actions out of the record's state.
    fn forget_tab(&mut self, tab: &str) {
        for key in self.keys_of(tab) {
            self.actions.remove(&key);
        }
        self.tabs.remove(tab);
    }
}

impl PageAction {
    /// The action `entry` lists, with only what the record keeps of one and
    /// each text within its bound. `None` for an entry without a place or
    /// that says neither what was asked nor how it came about.
    pub fn of(entry: &Value) -> Option<Self> {
        let entry = entry.as_object()?;
        let seq = entry.get("seq").and_then(whole).filter(|seq| *seq > 0)?;
        let text = |key: &str, most: usize| {
            Atom(
                entry
                    .get(key)
                    .and_then(Value::as_str)
                    .map(|text| clipped(text, most))
                    .unwrap_or_default(),
            )
        };
        let (action, how) = (text("action", MAX_NAME_BYTES), text("how", MAX_NAME_BYTES));
        if action.0.is_empty() && how.0.is_empty() {
            return None;
        }
        let ok = entry.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let target = entry.get("target").and_then(Value::as_object);
        let of_target = |key: &str| {
            Atom(
                target
                    .and_then(|target| target.get(key))
                    .and_then(Value::as_str)
                    .map(|text| clipped(text, MAX_NAME_BYTES))
                    .unwrap_or_default(),
            )
        };
        let secret = entry.get("secret").and_then(Value::as_bool) == Some(true);
        // What an action wrote is kept only when the page took it, and never
        // when it went into a secret field, whatever the entry carries.
        let kept = ok && !secret;
        let wrote = kept
            .then(|| entry.get("text").and_then(Value::as_str))
            .flatten()
            .map(|text| Atom(clipped(text, MAX_TEXT_BYTES)));
        let values = kept
            .then(|| entry.get("values").and_then(Value::as_array))
            .flatten()
            .map(|values| {
                values
                    .iter()
                    .take(MAX_VALUES)
                    .map(|value| Atom(plain(value, MAX_TEXT_BYTES)))
                    .collect()
            });
        let answers = kept
            .then(|| entry.get("answers").and_then(Value::as_array))
            .flatten()
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .take(MAX_ANSWERS)
            .map(|answer| Answer {
                name: Atom(
                    answer
                        .get("name")
                        .map(|name| plain(name, MAX_TEXT_BYTES))
                        .unwrap_or_default(),
                ),
                value: Atom(
                    answer
                        .get("value")
                        .map(|value| plain(value, MAX_TEXT_BYTES))
                        .unwrap_or_default(),
                ),
            })
            .collect();
        Some(Self {
            seq,
            time_ms: entry.get("time_ms").and_then(whole).unwrap_or(0),
            actor: text("actor", MAX_NAME_BYTES),
            how,
            action,
            ok,
            code: if ok {
                Atom::default()
            } else {
                text("code", MAX_NAME_BYTES)
            },
            message: text("message", MAX_TEXT_BYTES),
            url: text("url", MAX_URL_BYTES),
            to_url: text("to_url", MAX_URL_BYTES),
            navigation: text("navigation", MAX_NAME_BYTES),
            frame: text("frame", MAX_NAME_BYTES),
            role: of_target("role"),
            name: of_target("name"),
            nth: target
                .and_then(|target| target.get("nth"))
                .and_then(whole)
                .unwrap_or(0),
            secret,
            text: wrote,
            values,
            answers,
        })
    }

    /// Whether `author`, an actor of `kind`, is who did this action. What
    /// was typed straight into the page, and what the tab's unnamed actor
    /// did, is a person's. Any other action is its actor's: the name the
    /// engine keeps `author` under.
    fn is_by(&self, author: Option<&str>, kind: Option<ActorKind>) -> bool {
        if self.how.0 == "input" || self.actor.0.is_empty() {
            return kind.is_none_or(|kind| kind == ActorKind::Person);
        }
        author
            .and_then(PageActor::named)
            .is_some_and(|named| named.as_str() == self.actor.0)
    }

    /// The action as the engine lists one: what [`Self::of`] kept, under the
    /// names it was listed by.
    pub fn entry(&self) -> Value {
        let mut entry = serde_json::Map::new();
        let mut say = |key: &str, text: &Atom| {
            if !text.0.is_empty() {
                entry.insert(key.to_owned(), text.0.clone().into());
            }
        };
        say("how", &self.how);
        say("action", &self.action);
        say("code", &self.code);
        say("message", &self.message);
        say("url", &self.url);
        say("to_url", &self.to_url);
        say("navigation", &self.navigation);
        say("frame", &self.frame);
        entry.insert("seq".into(), self.seq.into());
        if self.time_ms > 0 {
            entry.insert("time_ms".into(), self.time_ms.into());
        }
        entry.insert("actor".into(), self.actor.0.clone().into());
        entry.insert("ok".into(), self.ok.into());
        if !self.role.0.is_empty() || !self.name.0.is_empty() {
            let mut target = serde_json::Map::new();
            target.insert("role".into(), self.role.0.clone().into());
            target.insert("name".into(), self.name.0.clone().into());
            if self.nth > 0 {
                target.insert("nth".into(), self.nth.into());
            }
            entry.insert("target".into(), target.into());
        }
        // Said so a reader knows the field was set and why no text follows.
        if self.secret {
            entry.insert("secret".into(), true.into());
        }
        if let Some(text) = &self.text {
            entry.insert("text".into(), text.0.clone().into());
        }
        if let Some(values) = &self.values {
            let values: Vec<Value> = values.iter().map(|value| value.0.clone().into()).collect();
            entry.insert("values".into(), values.into());
        }
        if !self.answers.is_empty() {
            let answers: Vec<Value> = self
                .answers
                .iter()
                .map(|answer| serde_json::json!({"name": answer.name.0, "value": answer.value.0}))
                .collect();
            entry.insert("answers".into(), answers.into());
        }
        entry.into()
    }
}

/// Whether `tab` can name a tab: a short id of letters, digits, `-` and `_`.
fn valid_tab(tab: &str) -> bool {
    !tab.is_empty()
        && tab.len() <= MAX_TAB_ID_BYTES
        && tab
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// What every key of `tab`'s actions starts with.
fn tab_prefix(tab: &str) -> String {
    format!("{tab}/")
}

/// The key of the action at `seq` of `tab`: a tab's keys sort by place.
fn action_key(tab: &str, seq: u64) -> String {
    format!("{tab}/{seq:020}")
}

/// Past 2^53 a double no longer names every whole number.
const LARGEST_EXACT: f64 = 9_007_199_254_740_992.0;

/// A number an engine may have written as a double (`3.0`), when it is
/// whole.
fn whole(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return Some(number);
    }
    let number = value.as_f64()?;
    (number.fract() == 0.0 && (0.0..=LARGEST_EXACT).contains(&number)).then_some(number as u64)
}

/// At most `most` bytes of `text`, never ending inside a character.
fn clipped(text: &str, most: usize) -> String {
    if text.len() <= most {
        return text.to_owned();
    }
    let mut end = most;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default().to_owned()
}

/// `value` as the text the record keeps: a text as it is, anything else as
/// it is written.
fn plain(value: &Value, most: usize) -> String {
    match value {
        Value::String(text) => clipped(text, most),
        Value::Null => String::new(),
        other => clipped(&other.to_string(), most),
    }
}

/// Where a window's engine stands against the tab's record, and who the
/// tab's actors were admitted as.
///
/// Presentation-side bookkeeping only: the record is the resource. The
/// engine counts its own record from 1 each time it starts, so the window
/// keeps how far it has read the engine's (`cursor`) and what the tab's
/// record held when that engine began (`base`). An action the engine lists
/// at its place *n* stands at `base + n` in the resource, and that is the
/// place every answer names.
#[derive(Default)]
pub(crate) struct TabRecorder {
    record: Option<PersonRecord<PageRecord>>,
    /// The tab's id in the record.
    tab: String,
    /// The device this window runs on.
    device: String,
    /// The engine `base` and `cursor` are counted against, by its address.
    engine: Option<usize>,
    base: u64,
    cursor: u64,
    /// The read of the engine's record this window sent for itself, and
    /// when it stops waiting for it.
    pull: Option<(u32, Instant)>,
    /// Something ended while that read was with the engine.
    again: bool,
    /// The invoker each actor of this tab was admitted as, by the name the
    /// engine keeps it under.
    actors: Vec<(String, ActionInvoker)>,
    /// What the secrets this window sent are told apart by: their keyed
    /// digests, never their text.
    key: [u8; 32],
    withheld: Vec<[u8; 32]>,
}

impl TabRecorder {
    /// Keep the tab's record in `record`, as a tab of the device `device`.
    pub(crate) fn attach(&mut self, record: PersonRecord<PageRecord>, device: &str) {
        let mut key = [0; 32];
        for half in key.chunks_mut(16) {
            half.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        }
        *self = Self {
            record: Some(record),
            tab: uuid::Uuid::new_v4().simple().to_string(),
            device: device.to_owned(),
            key,
            ..Self::default()
        };
    }

    /// Whether the tab's record is kept in a resource. A private window
    /// keeps none.
    pub(crate) const fn attached(&self) -> bool {
        self.record.is_some()
    }

    /// The tab's id in the record.
    pub(crate) fn tab(&self) -> Option<&str> {
        self.attached().then_some(self.tab.as_str())
    }

    /// Go on with the record of the tab `tab`: the one this window showed
    /// before it was closed. Only a tab of this device is taken up, and only
    /// before this window set anything down under its own id.
    pub(crate) fn adopt(&mut self, tab: &str) {
        let Some(record) = &self.record else {
            return;
        };
        if !valid_tab(tab) || self.cursor > 0 {
            return;
        }
        let ours = record.with(|state| {
            state
                .tabs
                .get(tab)
                .is_none_or(|kept| kept.device.0 == self.device)
        });
        if ours {
            tab.clone_into(&mut self.tab);
            self.engine = None;
        }
    }

    /// The engine the tab's page runs in now. One this window has not read
    /// yet counts its record from the start: the tab's record goes on from
    /// what it holds.
    pub(crate) fn engine_is(&mut self, engine: usize) {
        if self.engine == Some(engine) {
            return;
        }
        self.engine = Some(engine);
        self.base = self
            .record
            .as_ref()
            .map_or(0, |record| record.with(|state| state.last(&self.tab)));
        self.cursor = 0;
        self.pull = None;
        self.again = false;
    }

    /// `request` was admitted for `invoker`, who acts in the tab as `actor`:
    /// what the engine sets down under that name is authored by that
    /// invoker. The text of a secret fill is remembered by its digest, so
    /// it is never written whatever the engine lists.
    pub(crate) fn admitted(
        &mut self,
        actor: Option<&PageActor>,
        invoker: Option<&ActionInvoker>,
        request: &PageRequest,
    ) {
        if !self.attached() {
            return;
        }
        self.acts_as(actor, invoker);
        if let PageRequest::Fill {
            secret: true, text, ..
        } = request
        {
            let kept = clipped(text, MAX_TEXT_BYTES);
            for secret in [text.as_str(), kept.as_str()] {
                let digest = self.digest(secret);
                if !self.withheld.contains(&digest) {
                    if self.withheld.len() >= MAX_WITHHELD {
                        self.withheld.remove(0);
                    }
                    self.withheld.push(digest);
                }
            }
        }
    }

    /// `invoker` acts in the tab as `actor`: what the engine sets down under
    /// that name is authored by that invoker, whether it is a page action of
    /// theirs or their answer to something the window held.
    pub(crate) fn acts_as(&mut self, actor: Option<&PageActor>, invoker: Option<&ActionInvoker>) {
        if !self.attached() {
            return;
        }
        if let (Some(actor), Some(invoker)) = (actor, invoker) {
            self.actors.retain(|(name, _)| name != actor.as_str());
            if self.actors.len() >= MAX_ACTORS {
                self.actors.remove(0);
            }
            self.actors
                .push((actor.as_str().to_owned(), invoker.clone()));
        }
    }

    fn digest(&self, text: &str) -> [u8; 32] {
        *blake3::keyed_hash(&self.key, text.as_bytes()).as_bytes()
    }

    /// Whether `value` is the text of a secret this window sent.
    fn is_withheld(&self, value: &Value) -> bool {
        value
            .as_str()
            .is_some_and(|text| self.withheld.contains(&self.digest(text)))
    }

    /// Take out of `entry` every text this window sent as a secret.
    fn withhold(&self, entry: &mut Value) {
        if self.withheld.is_empty() {
            return;
        }
        let Some(entry) = entry.as_object_mut() else {
            return;
        };
        if entry.get("text").is_some_and(|text| self.is_withheld(text)) {
            entry.remove("text");
        }
        if let Some(Value::Array(values)) = entry.get_mut("values") {
            values.retain(|value| !self.is_withheld(value));
        }
        if let Some(Value::Array(answers)) = entry.get_mut("answers") {
            answers.retain(|answer| {
                answer
                    .get("value")
                    .is_none_or(|value| !self.is_withheld(value))
            });
        }
    }

    /// Who the engine's `actor` was admitted as. A name this window no
    /// longer remembers is still an actor of that name.
    fn invoker(&self, actor: &str) -> ActionInvoker {
        self.actors
            .iter()
            .find(|(name, _)| name == actor)
            .map_or_else(
                || ActionInvoker::actor(actor).with_kind(ActorKind::Agent),
                |(_, invoker)| invoker.clone(),
            )
    }

    /// Write what the engine's record lists in `data` and this window has
    /// not written yet: each action one change of the resource, authored by
    /// its actor. What was typed straight into the page, and what the tab's
    /// unnamed actor did, is the person's. Whether the engine has more to
    /// list after these.
    pub(crate) fn set_down(&mut self, data: Option<&Value>) -> bool {
        let (Some(record), Some(data)) = (self.record.clone(), data) else {
            return false;
        };
        let entries = data
            .get("entries")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for listed in entries {
            let Some(seq) = listed.get("seq").and_then(whole) else {
                continue;
            };
            if seq <= self.cursor {
                continue;
            }
            let mut entry = listed.clone();
            if let Some(fields) = entry.as_object_mut() {
                fields.insert("seq".into(), self.base.saturating_add(seq).into());
            }
            self.withhold(&mut entry);
            // An entry the record cannot keep is passed over, not waited on.
            if PageAction::of(&entry).is_none() {
                self.cursor = seq;
                continue;
            }
            let actor = listed
                .get("actor")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let typed = listed.get("how").and_then(Value::as_str) == Some("input");
            let invoke = PageRecord::SET_DOWN.with(page_actions::SetDown {
                tab: self.tab.clone(),
                device: self.device.clone(),
                entry,
            });
            let written = if typed || actor.is_empty() {
                record.invoke(&invoke)
            } else {
                record.invoke_as(&self.invoker(actor), &invoke)
            };
            if let Err(error) = written {
                // It is read from the engine again with the next action.
                tracing::warn!(%error, "a page action was not set down in the tab's record");
                return false;
            }
            self.cursor = seq;
        }
        data.get("more").and_then(Value::as_bool).unwrap_or(false)
    }

    /// The engine says it set `entry` down at its place `seq` of its own
    /// accord: what a person typed straight into the page, or what the
    /// window held the page from sending and how that ended, which no
    /// action's answer carries. The entry that follows what this window has
    /// written is written at once, as its actor's, and as the person's when
    /// it names none. One further on has actions
    /// before it this window has not read: it is written in its place once
    /// they are, from the engine's own listing. Whether the window is to
    /// read the engine's record for that.
    pub(crate) fn heard(&mut self, seq: u64, entry: &Value) -> bool {
        if !self.attached() || seq <= self.cursor {
            return false;
        }
        if seq != self.cursor.saturating_add(1) {
            return true;
        }
        let mut entry = entry.clone();
        if let Some(fields) = entry.as_object_mut() {
            fields.insert("seq".into(), seq.into());
        }
        self.set_down(Some(&serde_json::json!({ "entries": [entry] })));
        // One that could not be written is read from the engine again.
        self.cursor < seq
    }

    /// The part of the tab's record after the place `since`, as a read of
    /// it answers: `entries`, `next`, `more` and `dropped`. `engine_more`
    /// says the engine lists actions this window has not written yet.
    pub(crate) fn read(&self, since: u64, engine_more: bool) -> Option<Value> {
        let record = self.record.as_ref()?;
        let page = record.with(|state| state.page(&self.tab, since));
        Some(serde_json::json!({
            "entries": page.entries,
            "next": page.next,
            "more": page.more || engine_more,
            "dropped": page.dropped,
        }))
    }

    /// How far this window has read its engine's record.
    pub(crate) const fn cursor(&self) -> u64 {
        self.cursor
    }

    /// The engine's place `seq`, as the tab's record counts it.
    fn place(&self, seq: &Value) -> Option<Value> {
        whole(seq).map(|seq| self.base.saturating_add(seq).into())
    }

    /// The action the engine says is running, with its place as the tab's
    /// record counts it.
    pub(crate) fn running(&self, running: &Value) -> Value {
        let mut running = running.clone();
        self.replace(&mut running);
        running
    }

    /// Name the place in `entry` as the tab's record counts it.
    fn replace(&self, entry: &mut Value) {
        if self.base == 0 {
            return;
        }
        if let Some(entry) = entry.as_object_mut()
            && let Some(place) = entry.get("seq").and_then(|seq| self.place(seq))
        {
            entry.insert("seq".into(), place);
        }
    }

    /// Name the places `note` carries as the tab's record counts them.
    pub(crate) fn places(&self, note: &mut PageRecordNote) {
        if self.base == 0 {
            return;
        }
        note.seq = note.seq.map(|seq| self.base.saturating_add(seq));
        for other in &mut note.others {
            self.replace(other);
        }
    }

    /// An engine's answer with every place in it named as the tab's record
    /// counts it, and whether it says something was set down that this
    /// window should read.
    pub(crate) fn placed(
        &self,
        mut result: Result<PageReply, PageFailure>,
    ) -> (Result<PageReply, PageFailure>, bool) {
        let note = match &mut result {
            Ok(reply) => {
                // A review of the page's fields names the action that set
                // each one by its place.
                if let Some(Value::Array(fields)) =
                    reply.data.as_mut().and_then(|data| data.get_mut("fields"))
                {
                    for field in fields {
                        if let Some(set) = field.get_mut("set") {
                            self.replace(set);
                        }
                    }
                }
                &mut reply.record
            }
            Err(failure) => &mut failure.record,
        };
        let news = note.seq.is_some() || !note.others.is_empty();
        self.places(note);
        (result, news)
    }

    /// Whether a read of the engine's record this window sent for itself is
    /// still awaited at `now`.
    pub(crate) fn pulling(&mut self, now: Instant) -> bool {
        let awaited = self.pull.is_some_and(|(_, until)| now < until);
        if awaited {
            self.again = true;
        }
        awaited
    }

    /// This window asked its engine for its record as `request_id`, and
    /// waits for the answer until `until`.
    pub(crate) fn pull_sent(&mut self, request_id: u32, until: Instant) {
        self.pull = Some((request_id, until));
        self.again = false;
    }

    /// Whether `request_id` is the read this window sent for itself; it is
    /// awaited no longer.
    pub(crate) fn pulled(&mut self, request_id: u32) -> bool {
        let ours = self.pull.is_some_and(|(sent, _)| sent == request_id);
        if ours {
            self.pull = None;
        }
        ours
    }

    /// Whether something ended while the last read was with the engine.
    pub(crate) fn take_again(&mut self) -> bool {
        std::mem::take(&mut self.again)
    }
}

#[cfg(test)]
mod tests;
