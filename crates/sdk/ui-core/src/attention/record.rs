//! The waiting stack: what wants a person, and the one thing to do about
//! each.
//!
//! An [`Item`] cannot be built without an [`Action`]: the type is what keeps
//! "rows that cannot act are not rows" true, rather than a rule somebody has
//! to remember. An item has an owner with a face (icon *and* name), what it
//! is about, why (when there is a why), since when, how urgent it is, and
//! the verb it waits on.
//!
//! Three states, deliberately distinct ([`State`]): **unseen** (it counts on
//! the bar), **seen** (the row stays, the count does not), and **deferred**
//! (it is out of the way but counted — a quiet system is still honest about
//! what it is holding back). Nothing expires: an item leaves when it is
//! acted on, or when the app that raised it takes it back.

use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::icons::Icon;
use crate::lock::MutexRecover as _;
use crate::time::unix_now_ms;
use crate::wake::WakeHandle;

/// The most items one app may have waiting; raising a further one drops the
/// oldest of that app's, so one app cannot flood the stack.
const MAX_PER_OWNER: usize = 12;

/// The most items the stack holds at all.
const MAX_ITEMS: usize = 64;

/// Who is asking: an app's identity, as the row carries it. A row always
/// names the app as well as showing its icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    /// The app's id (`so.tilo.store`).
    pub app: String,
    /// The app's name as the bar shows it ("Store").
    pub name: String,
    /// The app's icon.
    pub icon: Icon,
}

impl Owner {
    /// `app`'s owner, with the icon of an app the caller does not know.
    #[must_use]
    pub fn new(app: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            name: name.into(),
            icon: Icon::Info,
        }
    }

    /// The same owner, with the icon its app uses.
    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
    }
}

/// The one thing a person can do about an item: a verb the app knows, and
/// the words the button shows ("Print again").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Action {
    /// The verb, in the app's own vocabulary; what acting reports back.
    pub verb: String,
    /// The button's words.
    pub label: String,
}

impl Action {
    /// The verb `verb`, shown as `label`.
    #[must_use]
    pub fn new(verb: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            verb: verb.into(),
            label: label.into(),
        }
    }
}

/// The verb a failed job's row carries: open the job's subject.
pub const SHOW_JOB: &str = "jobs.show";

/// How much it costs to leave an item waiting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Urgency {
    /// Something is wrong, or will be: act now.
    Blocking,
    /// Nothing breaks, but it waits on a person.
    #[default]
    Waiting,
    /// Worth knowing, and it can wait a while.
    Noted,
}

impl Urgency {
    /// The label a sorted stack shows it under.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Blocking => "attention-blocking",
            Self::Waiting => "attention-waiting",
            Self::Noted => "attention-noted",
        }
    }
}

/// What an item is about, when the app that raised it can take it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// A job in [`crate::jobs`].
    Job(u64),
    /// A ledger record.
    Record(u64),
}

/// Where an item stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum State {
    /// Nobody has looked at it: it counts.
    #[default]
    Unseen,
    /// It was shown; the row stays, the count does not.
    Seen,
    /// The person put it out of the way; it is still there, and counted.
    Deferred,
}

/// An item's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(u64);

impl ItemId {
    /// The id as it is written in a log line or a test.
    #[must_use]
    pub const fn number(self) -> u64 {
        self.0
    }
}

/// One thing waiting for a person.
#[derive(Clone, Debug)]
pub struct Item {
    id: ItemId,
    owner: Owner,
    title: String,
    about: Option<String>,
    because: Option<String>,
    action: Action,
    urgency: Urgency,
    since_ms: u64,
    target: Option<Target>,
    state: State,
}

impl Item {
    /// An item `owner` raises, titled `title`, waiting on `action`.
    ///
    /// This is the only way to make one: an item without an action would be
    /// a log line pretending to be a decision.
    #[must_use]
    pub fn new(owner: Owner, title: impl Into<String>, action: Action) -> Self {
        Self {
            id: ItemId(0),
            owner,
            title: title.into(),
            about: None,
            because: None,
            action,
            urgency: Urgency::default(),
            since_ms: unix_now_ms(),
            target: None,
            state: State::Unseen,
        }
    }

    /// What it is about ("LibreOffice", "Home folder").
    #[must_use]
    pub fn about(mut self, about: impl Into<String>) -> Self {
        self.about = Some(about.into());
        self
    }

    /// Why it is waiting, in one clause ("The printer stopped answering").
    #[must_use]
    pub fn because(mut self, because: impl Into<String>) -> Self {
        self.because = Some(because.into());
        self
    }

    /// How much it costs to leave it.
    #[must_use]
    pub fn urgent(mut self, urgency: Urgency) -> Self {
        self.urgency = urgency;
        self
    }

    /// What it is about, for the app to take it back.
    #[must_use]
    pub fn subject(mut self, target: Target) -> Self {
        self.target = Some(target);
        self
    }

    /// Its identity.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
    }

    /// Who is asking.
    #[must_use]
    pub const fn owner(&self) -> &Owner {
        &self.owner
    }

    /// What happened, in one line.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// What it is about.
    #[must_use]
    pub fn about_line(&self) -> Option<&str> {
        self.about.as_deref()
    }

    /// Why it is waiting.
    #[must_use]
    pub fn because_line(&self) -> Option<&str> {
        self.because.as_deref()
    }

    /// The one thing to do about it.
    #[must_use]
    pub const fn action(&self) -> &Action {
        &self.action
    }

    /// How much it costs to leave it.
    #[must_use]
    pub const fn urgency(&self) -> Urgency {
        self.urgency
    }

    /// When it was raised, in milliseconds since the epoch.
    #[must_use]
    pub const fn since_ms(&self) -> u64 {
        self.since_ms
    }

    /// What it is about, for the app.
    #[must_use]
    pub const fn target(&self) -> Option<Target> {
        self.target
    }

    /// Where it stands.
    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }

    /// The row's secondary line: what it is about and why, as one clause.
    #[must_use]
    pub fn detail_line(&self) -> String {
        match (&self.about, &self.because) {
            (None, None) => String::new(),
            (Some(about), None) => about.clone(),
            (None, Some(because)) => because.clone(),
            (Some(about), Some(because)) => crate::i18n::widget_format(
                "attention-because",
                &[
                    ("about", crate::i18n::ArgValue::from(about.clone())),
                    ("because", crate::i18n::ArgValue::from(because.clone())),
                ],
            ),
        }
    }
}

/// What [`publish`] did with a notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Published {
    /// It waits for a person: the item's identity.
    Waiting(ItemId),
    /// It was recorded: the ledger entry's identity.
    Recorded(u64),
}

/// What the system has to say: a piece of news, and whether it needs a
/// person.
///
/// [`Notice::needs`] carries the action it waits on and goes to the waiting
/// stack; [`Notice::did`] carries none and is recorded in the ledger. There
/// is no third place.
#[derive(Clone, Debug)]
pub struct Notice {
    owner: Owner,
    title: String,
    about: Option<String>,
    because: Option<String>,
    action: Option<Action>,
    urgency: Urgency,
    target: Option<Target>,
}

impl Notice {
    /// News that waits for a person, who can `action` it.
    #[must_use]
    pub fn needs(owner: Owner, title: impl Into<String>, action: Action) -> Self {
        Self {
            owner,
            title: title.into(),
            about: None,
            because: None,
            action: Some(action),
            urgency: Urgency::default(),
            target: None,
        }
    }

    /// News that does not: it is recorded, and read when someone looks.
    #[must_use]
    pub fn did(owner: Owner, title: impl Into<String>) -> Self {
        Self {
            owner,
            title: title.into(),
            about: None,
            because: None,
            action: None,
            urgency: Urgency::default(),
            target: None,
        }
    }

    /// What it is about.
    #[must_use]
    pub fn about(mut self, about: impl Into<String>) -> Self {
        self.about = Some(about.into());
        self
    }

    /// Why, in one clause.
    #[must_use]
    pub fn because(mut self, because: impl Into<String>) -> Self {
        self.because = Some(because.into());
        self
    }

    /// How much it costs to leave it.
    #[must_use]
    pub fn urgent(mut self, urgency: Urgency) -> Self {
        self.urgency = urgency;
        self
    }

    /// What it is about, for the app to take it back.
    #[must_use]
    pub fn subject(mut self, target: Target) -> Self {
        self.target = Some(target);
        self
    }
}

/// Say it once: an outcome goes to the waiting stack if it needs a person,
/// or to the ledger if it does not.
///
/// Raising the same news twice (the same app, about the same thing, titled
/// the same way) does not stack two rows: the one waiting takes the newer
/// why, and is unseen again if it had been seen.
pub fn publish(notice: Notice) -> Published {
    if let Some(action) = notice.action.clone() {
        let mut item = Item::new(notice.owner, notice.title, action).urgent(notice.urgency);
        if let Some(about) = notice.about {
            item = item.about(about);
        }
        if let Some(because) = notice.because {
            item = item.because(because);
        }
        if let Some(target) = notice.target {
            item = item.subject(target);
        }
        Published::Waiting(raise(item))
    } else {
        let mut entry = crate::attention::ledger::Entry::new(notice.owner, notice.title);
        if let Some(about) = notice.about {
            entry = entry.about(about);
        }
        if let Some(because) = notice.because {
            entry = entry.because(because);
        }
        if let Some(target) = notice.target {
            entry = entry.subject(target);
        }
        Published::Recorded(crate::attention::ledger::record(entry))
    }
}

/// Put `item` in the stack. The same news waiting again replaces what waits,
/// so a repeating failure is one row that says the latest why.
pub fn raise(item: Item) -> ItemId {
    let (id, wakes) = {
        let mut registry = registry();
        let next = registry.next_item;
        registry.next_item = registry.next_item.wrapping_add(1);
        let mut item = item;
        item.id = ItemId(next);
        // The same news is the same app saying the same thing about the same
        // thing: it replaces what waits (and takes the newer why with it), so
        // a repeating failure is one row rather than a stack.
        let same = |waiting: &Item| {
            waiting.owner.app == item.owner.app
                && waiting.title == item.title
                && waiting.about == item.about
                && waiting.state != State::Deferred
        };
        let id = if let Some(index) = registry.items.iter().position(same) {
            let earlier = registry.items[index].since_ms;
            let id = registry.items[index].id;
            item.since_ms = earlier;
            registry.items[index] = item;
            id
        } else {
            registry.items.push(item);
            registry.trim();
            ItemId(next)
        };
        registry.changed();
        (id, registry.wakes())
    };
    for wake in wakes {
        wake.wake();
    }
    id
}

/// Every item waiting, ordered by urgency and then by age: the first row is
/// the one that costs most to leave.
#[must_use]
pub fn items() -> Vec<Item> {
    let registry = registry();
    let mut items: Vec<Item> = registry
        .items
        .iter()
        .filter(|item| item.state != State::Deferred)
        .cloned()
        .collect();
    items.sort_by(|a, b| {
        a.urgency
            .cmp(&b.urgency)
            .then_with(|| a.since_ms.cmp(&b.since_ms))
            .then_with(|| a.id.cmp(&b.id))
    });
    items
}

/// How many items nobody has looked at: the count a bar shows.
#[must_use]
pub fn unseen() -> usize {
    registry()
        .items
        .iter()
        .filter(|item| item.state == State::Unseen)
        .count()
}

/// How many items are waiting and not deferred.
#[must_use]
pub fn waiting() -> usize {
    registry()
        .items
        .iter()
        .filter(|item| item.state != State::Deferred)
        .count()
}

/// How many the person put out of the way: still counted, still honest.
#[must_use]
pub fn deferred() -> usize {
    registry()
        .items
        .iter()
        .filter(|item| item.state == State::Deferred)
        .count()
}

/// Note that the stack was shown: everything in it stops counting, and the
/// rows stay. Seeing is not dismissing.
pub fn seen() {
    let wakes = {
        let mut registry = registry();
        let mut changed = false;
        for item in &mut registry.items {
            if item.state == State::Unseen {
                item.state = State::Seen;
                changed = true;
            }
        }
        if changed {
            registry.changed();
        }
        registry.wakes()
    };
    for wake in wakes {
        wake.wake();
    }
}

/// Put item `id` out of the way; it stays counted as deferred.
pub fn defer(id: ItemId) {
    change(id, |item| item.state = State::Deferred);
}

/// Take item `id` out of the stack: it was answered, or no longer holds.
pub fn dismiss(id: ItemId) {
    let wakes = {
        let mut registry = registry();
        let before = registry.items.len();
        registry.items.retain(|item| item.id != id);
        if registry.items.len() == before {
            return;
        }
        registry.changed();
        registry.wakes()
    };
    for wake in wakes {
        wake.wake();
    }
}

/// Take back every item about `target`: the thing that raised it was
/// answered somewhere else (a job's row was read, its failure with it).
pub fn dismissed(target: Target) {
    let wakes = {
        let mut registry = registry();
        let before = registry.items.len();
        registry.items.retain(|item| item.target != Some(target));
        if registry.items.len() == before {
            return;
        }
        registry.changed();
        registry.wakes()
    };
    for wake in wakes {
        wake.wake();
    }
}

/// Answer item `id`: the action it waited on, which the app now performs.
/// The item leaves the stack.
#[must_use]
pub fn act(id: ItemId) -> Option<Action> {
    let (action, wakes) = {
        let mut registry = registry();
        let index = registry.items.iter().position(|item| item.id == id)?;
        let action = registry.items[index].action.clone();
        let _ = registry.items.remove(index);
        registry.changed();
        (action, registry.wakes())
    };
    for wake in wakes {
        wake.wake();
    }
    Some(action)
}

/// What acting on an item said, for the surface's caller to carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acted {
    /// Which item it was.
    pub item: ItemId,
    /// The app that raised it.
    pub owner: Owner,
    /// What it is about, for the app.
    pub target: Option<Target>,
    /// The verb to perform.
    pub action: Action,
}

/// Take the item `id` out of the stack and say what it asked for.
#[must_use]
pub fn take(id: ItemId) -> Option<Acted> {
    let (owner, target) = {
        let registry = registry();
        let item = registry.items.iter().find(|item| item.id == id)?;
        (item.owner.clone(), item.target)
    };
    let action = act(id)?;
    Some(Acted {
        item: id,
        owner,
        target,
        action,
    })
}

/// Take every item away: a test's clean slate, and what a session reset
/// does.
pub fn clear() {
    let mut registry = registry();
    registry.items.clear();
    registry.changed();
}

fn change(id: ItemId, change: impl FnOnce(&mut Item)) {
    let wakes = {
        let mut registry = registry();
        let Some(item) = registry.items.iter_mut().find(|item| item.id == id) else {
            return;
        };
        change(item);
        registry.changed();
        registry.wakes()
    };
    for wake in wakes {
        wake.wake();
    }
}

#[derive(Default)]
struct Registry {
    next_item: u64,
    items: Vec<Item>,
    revision: u64,
    watchers: Vec<(u64, WakeHandle)>,
    next_watch: u64,
}

impl Registry {
    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn wakes(&self) -> Vec<WakeHandle> {
        self.watchers.iter().map(|(_, wake)| wake.clone()).collect()
    }

    /// Keep the stack bounded: the oldest item of an app that has too many,
    /// and the oldest item at all past the cap.
    fn trim(&mut self) {
        let over: Vec<String> = {
            let mut per_owner: Vec<(String, usize)> = Vec::new();
            for item in &self.items {
                match per_owner.iter_mut().find(|(app, _)| *app == item.owner.app) {
                    Some((_, count)) => *count = count.saturating_add(1),
                    None => per_owner.push((item.owner.app.clone(), 1)),
                }
            }
            per_owner
                .into_iter()
                .filter(|(_, count)| *count > MAX_PER_OWNER)
                .map(|(app, _)| app)
                .collect()
        };
        for app in over {
            if let Some(index) = self.items.iter().position(|item| item.owner.app == app) {
                let _ = self.items.remove(index);
            }
        }
        while self.items.len() > MAX_ITEMS {
            let _ = self.items.remove(0);
        }
    }
}

fn registry() -> MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Mutex::new(Registry::default()))
        .lock_recover()
}

/// Bumped by every change to the stack.
#[must_use]
pub fn revision() -> u64 {
    registry().revision
}

pub(crate) fn watch(wake: WakeHandle) -> crate::attention::Watch {
    let mut registry = registry();
    let id = registry.next_watch;
    registry.next_watch = registry.next_watch.wrapping_add(1);
    registry.watchers.push((id, wake));
    crate::attention::Watch::new(id)
}

pub(crate) fn unwatch(id: u64) {
    let mut registry = registry();
    registry.watchers.retain(|(watcher, _)| *watcher != id);
}
