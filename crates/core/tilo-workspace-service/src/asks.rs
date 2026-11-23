//! Asks of agents and their replies, read and written through the agent package
//! (`so.tilo.agent`) on a tilolang record ([`crate::tl_records`]).
//!
//! An ask is a resource of the agent package, kept in the record of whichever app it was made
//! in: a mention in a Chat message makes one in Chat's record, one in a note in Notes'. It is
//! made from what it is about (`created(a)?.from`), and who asked is who made it
//! (`created(a)?.who`). What it reads around an ask is where that resource sits ([`around`]):
//! the collection it is in, each item by its first text and who wrote it, in order. Of the
//! resource itself it reads the files a message sends with it ([`carried_files`]), which a turn
//! is told and reads like any file of the workspace; nothing else of an app's own fields.
//!
//! Everything an agent's turn writes goes through the package's own actions, invoked as the
//! agent: [`answer`] starts its reply, [`say`] appends to the reply's body, [`step`] records an
//! action it called and what it answered, [`finish`] ends the turn (which device takes it
//! [`crate::work::takes_turn`] decides) and [`supersede`] marks a reply another device's answer
//! stands in place of. [`ask`] makes an ask outright, keyed by its
//! cause: what a reminder, an event or a sub-agent asks. [`show`] puts the thing the agent worked
//! on into the reply's body as a card of it, live. [`keep_summary`] keeps what a conversation
//! said before the part a turn was shown, keyed by the conversation and the agent, and
//! [`summaries`] reads back what was kept.

use tilo_actions::MemberId;
use tilolang_tilo::runtime::Value;
use tilolang_tilo::runtime::rich::RichValue;
use tilolang_tilo::runtime::store::Store as _;
use tilolang_tilo::runtime::value::EnumValue;

use crate::tl_records::{Opened, Packages};

/// Who made a resource, and a resource of a record, as the agent package names them.
pub use tilolang_tilo::runtime::{Actor, ResourceRef};

/// What a rich text refers to: a member, or a resource of the workspace by the catalogue's
/// reference to it.
pub use tilolang_tilo::runtime::rich::Reference;

/// The agent package.
pub const AGENT_APP: &str = "so.tilo.agent";

/// The agent package and tilolang Chat, which uses it: the record a mention in a conversation
/// asks in.
///
/// # Errors
/// A package does not compile, which is a bug of this build.
pub fn chat_packages() -> Result<Packages, String> {
    crate::tl_records::built_in(CHAT_APP)?.ok_or_else(|| format!("{CHAT_APP} is not built in"))
}

/// tilolang Chat's app id.
const CHAT_APP: &str = "so.tilo.chat";

/// Where an ask or a reply stands, as the agent package's `AskState` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskState {
    Waiting,
    Working,
    Done,
    Failed,
    Stopped,
    Withdrawn,
    CannotRead,
}

impl AskState {
    /// The variant's name in the package.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Working => "working",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Withdrawn => "withdrawn",
            Self::CannotRead => "cannot_read",
        }
    }

    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "waiting" => Self::Waiting,
            "working" => Self::Working,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "stopped" => Self::Stopped,
            "withdrawn" => Self::Withdrawn,
            "cannot_read" => Self::CannotRead,
            _ => return None,
        })
    }

    /// Whether a turn is still owed on it: asked and not ended.
    #[must_use]
    pub const fn owed(self) -> bool {
        matches!(self, Self::Waiting | Self::Working)
    }
}

/// One ask, as the record holds it now.
#[derive(Clone, Debug)]
pub struct Asked {
    /// The ask itself.
    pub at: ResourceRef,
    /// The agent asked.
    pub agent: MemberId,
    /// What was asked, as it read when asked; `None` where the agent may not read it.
    pub said: Option<String>,
    /// The resources what was asked refers to, in the order it says them: what it carries.
    /// Members it mentions are not among them.
    pub refers: Vec<Reference>,
    /// The mention, or the cause, that names the ask with what it is made from.
    pub mention: String,
    pub state: AskState,
    /// Who asked.
    pub who: Option<Actor>,
    /// What it is about.
    pub from: Option<ResourceRef>,
    /// The files what it is about carries, as that resource's record sends them with it: the
    /// files of a Chat message. Empty where the agent may not read what it is about, and for a
    /// resource that carries none.
    pub files: Vec<CarriedFile>,
    /// When it was asked, ISO-8601 UTC.
    pub asked_at: String,
    /// Its replies, in the order they were asked.
    pub replies: Vec<Replied>,
}

/// A file the resource an ask is about carries, as that resource's record keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedFile {
    /// What it is called, extension included.
    pub name: String,
    /// How many bytes it is; 0 where the record names no size.
    pub size: i64,
    /// Where its bytes read, as `tilo.content:<hash>`: what Files' `read-text` takes. Empty
    /// where the record names no address.
    pub content: String,
}

impl Asked {
    /// Its newest reply, where it has one.
    #[must_use]
    pub fn latest(&self) -> Option<&Replied> {
        self.replies.iter().max_by_key(|reply| reply.attempt)
    }

    /// Which time it is being asked, its first reply being 1: the newest reply's while that one
    /// is still working, else the one after it. A turn keeps the same attempt from before its
    /// reply is written to its end, so every look names it the same.
    #[must_use]
    pub fn attempt(&self) -> u32 {
        let latest = self.latest();
        let newest = latest.map_or(0, |reply| reply.attempt);
        let attempt = if latest.is_some_and(|reply| reply.state == AskState::Working) {
            newest
        } else {
            newest.saturating_add(1)
        };
        u32::try_from(attempt).unwrap_or(u32::MAX)
    }
}

/// One reply, as the record holds it now.
#[derive(Clone, Debug)]
pub struct Replied {
    pub at: ResourceRef,
    pub attempt: i64,
    pub state: AskState,
    /// Its body, as written text.
    pub body: String,
    /// What its model thought on the way, as its provider showed it.
    pub thinking: String,
    /// The views it answered with ([`show_view`]), in order: each one's title and definition.
    pub views: Vec<(String, String)>,
    /// The things it showed as cards ([`show`]), in order: what each card is called, and the
    /// thing.
    pub shown: Vec<(String, Reference)>,
    /// The provider that answered it, as the workspace's settings name it; empty where no model
    /// answered.
    pub provider: String,
    /// The model that answered it, as a person reads it; empty where no model answered.
    pub model: String,
    /// What its model's calls cost, in millionths of the workspace's currency.
    pub cost: u64,
    pub superseded: bool,
    /// What it called on the way, in order.
    pub steps: Vec<Stepped>,
}

/// One step of a reply, as the record holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stepped {
    pub called: String,
    pub answered: String,
    pub failed: bool,
}

/// What sits around what an ask is about: the collection it is in, in order.
#[derive(Clone, Debug, Default)]
pub struct Around {
    /// What holds that collection (a conversation), where something does.
    pub holder: Option<ResourceRef>,
    /// What it is called (a conversation's name).
    pub title: String,
    pub items: Vec<Said>,
    /// How many items before the window the collection holds and the turn is not told.
    pub earlier: usize,
    /// The last item before the window, where any is: what a summary kept of it reaches.
    pub before: Option<Said>,
}

/// One item around an ask.
#[derive(Clone, Debug)]
pub struct Said {
    pub at: ResourceRef,
    /// Who made it.
    pub who: Option<Actor>,
    /// Its first text.
    pub text: String,
    /// When it was made, ISO-8601 UTC.
    pub when: String,
    /// Whether it is what the ask is about.
    pub asked_about: bool,
}

/// What an agent kept of what a conversation said before the part a turn was shown, as the
/// record holds it now.
#[derive(Clone, Debug)]
pub struct Summary {
    /// The summary itself.
    pub at: ResourceRef,
    /// The agent that keeps it.
    pub agent: MemberId,
    /// The conversation it summarizes: what it is made from.
    pub conversation: Option<ResourceRef>,
    /// What the conversation said before, in the agent's words.
    pub text: String,
    /// The item it reaches.
    pub through: String,
    /// When that item was written, ISO-8601 UTC.
    pub through_at: String,
}

fn meta(opened: &Opened) -> Result<tilolang_tilo::runtime::meta::Meta, String> {
    opened
        .host()
        .package(AGENT_APP)
        .map(|package| package.meta)
        .ok_or_else(|| "the agent package is not loaded".to_owned())
}

/// The agent package's root in this record: read as it would start where nothing made it yet.
fn root(meta: &tilolang_tilo::runtime::meta::Meta) -> Result<ResourceRef, String> {
    let ty = meta
        .type_named("Agents")
        .ok_or("the agent package has no Agents")?
        .id
        .clone();
    Ok(ResourceRef::root(format!("root-{ty}"), ty))
}

fn read(opened: &Opened, at: &ResourceRef, field: &str) -> Option<Value> {
    opened.host().runtime().read(at, field).ok()
}

fn text(value: Option<Value>) -> String {
    match value {
        Some(Value::Text(text)) => text,
        Some(Value::Rich(rich)) => rich.written_text(),
        Some(Value::Opt(Some(inner))) => text(Some(*inner)),
        _ => String::new(),
    }
}

fn int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Int(n)) => *n,
        _ => 0,
    }
}

fn target(value: &Value) -> Option<ResourceRef> {
    match value {
        Value::Ref(reference) => Some((*reference.target).clone()),
        _ => None,
    }
}

fn items(opened: &Opened, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match read(opened, at, field) {
        Some(Value::List(items)) => items.iter().filter_map(target).collect(),
        _ => Vec::new(),
    }
}

/// The files `at` carries, each item of its `attachments`: what a Chat message sends with it.
/// Empty for a resource that keeps no such collection (every app but Chat), and for an item
/// that names no address, whose bytes nobody can read.
fn carried_files(opened: &Opened, at: &ResourceRef) -> Vec<CarriedFile> {
    items(opened, at, "attachments")
        .iter()
        .filter_map(|file| {
            let name = text(read(opened, file, "name"));
            let blob = text(read(opened, file, "blob"));
            let hash = blob
                .strip_prefix(tilolang_tilo::runtime::blob::SCHEME)
                .filter(|hash| !hash.is_empty())?;
            if name.trim().is_empty() {
                return None;
            }
            Some(CarriedFile {
                name,
                size: int(read(opened, file, "size").as_ref()),
                content: format!("tilo.content:{hash}"),
            })
        })
        .collect()
}

fn state_of(meta: &tilolang_tilo::runtime::meta::Meta, value: Option<Value>) -> AskState {
    let Some(Value::Enum(state)) = value else {
        return AskState::Waiting;
    };
    let name = meta
        .enums
        .iter()
        .flat_map(|declared| declared.variants.iter())
        .find(|variant| variant.id == state.variant)
        .map_or(state.variant.as_str(), |variant| variant.name.as_str());
    AskState::named(name).unwrap_or(AskState::Waiting)
}

/// Every ask the record holds, with its replies, oldest first.
///
/// # Errors
/// The agent package is not loaded in the record.
pub fn asks(opened: &Opened) -> Result<Vec<Asked>, String> {
    let meta = meta(opened)?;
    let root = root(&meta)?;
    let replies: Vec<(ResourceRef, Option<ResourceRef>)> = items(opened, &root, "replies")
        .into_iter()
        .map(|reply| {
            let ask = read(opened, &reply, "ask").as_ref().and_then(target);
            (reply, ask)
        })
        .collect();
    let mut out = Vec::new();
    for at in items(opened, &root, "asks") {
        let Some(Value::Actor(Actor::Member(agent))) = read(opened, &at, "agent") else {
            continue;
        };
        let (said, refers) = match read(opened, &at, "said") {
            Some(Value::Opt(Some(said))) => {
                let refers = match said.as_ref() {
                    Value::Rich(rich) => rich
                        .references()
                        .into_iter()
                        .filter(|reference| reference.as_member().is_none())
                        .collect(),
                    _ => Vec::new(),
                };
                (Some(text(Some(*said))), refers)
            }
            _ => (None, Vec::new()),
        };
        let made = opened
            .host()
            .store()
            .versions(&at)
            .ok()
            .and_then(|versions| versions.into_iter().next());
        let mut answers: Vec<Replied> = replies
            .iter()
            .filter(|(_, ask)| ask.as_ref().is_some_and(|ask| ask.same(&at)))
            .map(|(reply, _)| Replied {
                at: reply.clone(),
                attempt: int(read(opened, reply, "attempt").as_ref()),
                state: state_of(&meta, read(opened, reply, "state")),
                body: text(read(opened, reply, "body")),
                thinking: text(read(opened, reply, "thinking")),
                views: items(opened, reply, "views")
                    .iter()
                    .map(|view| {
                        (
                            text(read(opened, view, "title")),
                            text(read(opened, view, "definition")),
                        )
                    })
                    .collect(),
                shown: items(opened, reply, "shown")
                    .iter()
                    .filter_map(|shown| {
                        let card = read(opened, shown, "card")?;
                        let Value::Rich(rich) = &card else {
                            return None;
                        };
                        let to = rich.projections().into_iter().next()?;
                        Some((text(Some(card)), to))
                    })
                    .collect(),
                provider: text(read(opened, reply, "provider")),
                model: text(read(opened, reply, "model")),
                cost: u64::try_from(int(read(opened, reply, "cost").as_ref())).unwrap_or(0),
                superseded: matches!(read(opened, reply, "superseded"), Some(Value::Bool(true))),
                steps: items(opened, reply, "steps")
                    .iter()
                    .map(|step| Stepped {
                        called: text(read(opened, step, "called")),
                        answered: text(read(opened, step, "answered")),
                        failed: matches!(read(opened, step, "failed"), Some(Value::Bool(true))),
                    })
                    .collect(),
            })
            .collect();
        answers.sort_by_key(|reply| reply.attempt);
        let from = made.as_ref().and_then(|made| made.from.clone());
        // A file is named only where the words it came with are: an ask the agent may not read
        // carries neither.
        let files = if said.is_some() {
            from.as_ref()
                .map_or_else(Vec::new, |from| carried_files(opened, from))
        } else {
            Vec::new()
        };
        out.push(Asked {
            agent,
            said,
            refers,
            mention: text(read(opened, &at, "mention")),
            state: state_of(&meta, read(opened, &at, "state")),
            who: made.as_ref().and_then(|made| made.who.clone()),
            from,
            files,
            asked_at: made.map(|made| made.at).unwrap_or_default(),
            replies: answers,
            at,
        });
    }
    Ok(out)
}

/// Every summary the record holds: what each agent kept of a conversation longer than a turn is
/// told of it.
///
/// # Errors
/// The agent package is not loaded in the record.
pub fn summaries(opened: &Opened) -> Result<Vec<Summary>, String> {
    let meta = meta(opened)?;
    let root = root(&meta)?;
    let mut out = Vec::new();
    for at in items(opened, &root, "summaries") {
        let Some(Value::Actor(Actor::Member(agent))) = read(opened, &at, "agent") else {
            continue;
        };
        let made = opened
            .host()
            .store()
            .versions(&at)
            .ok()
            .and_then(|versions| versions.into_iter().next());
        out.push(Summary {
            agent,
            conversation: made.as_ref().and_then(|made| made.from.clone()),
            text: text(read(opened, &at, "text")),
            through: text(read(opened, &at, "through")),
            through_at: text(read(opened, &at, "through_at")),
            at,
        });
    }
    Ok(out)
}

/// What sits around `about`: the collection it is in, at most the `most` items up to and
/// including it, each by its first text and who made it. Read by structure alone: no app's
/// field is named.
#[must_use]
pub fn around(opened: &Opened, about: &ResourceRef, most: usize) -> Around {
    let store = opened.host().store();
    let Ok(Some((holder, field))) = store.container(about) else {
        return Around {
            holder: None,
            title: String::new(),
            items: vec![said(opened, about, true)],
            earlier: 0,
            before: None,
        };
    };
    let title = store.first_text(&holder).unwrap_or_default();
    let mut all = items(opened, &holder, &field);
    if let Some(at) = all.iter().position(|item| item.same(about)) {
        all.truncate(at.saturating_add(1));
    }
    let skip = all.len().saturating_sub(most);
    Around {
        holder: Some(holder),
        title,
        items: all
            .iter()
            .skip(skip)
            .map(|item| said(opened, item, item.same(about)))
            .collect(),
        earlier: skip,
        before: skip
            .checked_sub(1)
            .and_then(|at| all.get(at))
            .map(|item| said(opened, item, false)),
    }
}

fn said(opened: &Opened, at: &ResourceRef, asked_about: bool) -> Said {
    let made = opened
        .host()
        .store()
        .versions(at)
        .ok()
        .and_then(|versions| versions.into_iter().next());
    Said {
        at: at.clone(),
        who: made.as_ref().and_then(|made| made.who.clone()),
        text: opened.host().store().first_text(at).unwrap_or_default(),
        when: made.map(|made| made.at).unwrap_or_default(),
        asked_about,
    }
}

/// Whether what `asked` is about is `keeper`'s and `agent`'s alone: every member of the
/// workspace who may read the resource it is made from is one of the two, as the workspace's
/// policy decides (`std.may_read`, [`Opened::readers`]). What an agent learned about its keeper
/// is told and kept only in such a turn. False for an ask made from nothing, and where nobody
/// reads it: a record opened without the policy knows no readers.
#[must_use]
pub fn private_to(opened: &Opened, asked: &Asked, keeper: &MemberId, agent: &MemberId) -> bool {
    asked
        .from
        .as_ref()
        .is_some_and(|from| alone_with(opened, from, keeper, agent))
}

/// The direct conversation `person` made with `agent` in Chat's record, where they made one:
/// where what an event of theirs asks the agent is shown.
#[must_use]
pub fn direct_with(opened: &Opened, person: &MemberId, agent: &MemberId) -> Option<ResourceRef> {
    let meta = opened.host().package(opened.app())?.meta;
    let direct = meta.type_named("Direct")?.id.clone();
    let conversations = meta.type_named("Conversations")?.id.clone();
    let root = ResourceRef::root(format!("root-{conversations}"), conversations);
    items(opened, &root, "channels").into_iter().find(|channel| {
        channel.ty == direct
            && matches!(read(opened, channel, "person"), Some(Value::Actor(Actor::Member(with))) if with == *agent)
            && opened
                .host()
                .store()
                .versions(channel)
                .ok()
                .and_then(|versions| versions.into_iter().next())
                .and_then(|made| made.who)
                .is_some_and(|who| matches!(who, Actor::Member(made_by) if made_by == *person))
    })
}

/// Whether every member of the workspace who may read `at` is `keeper` or `agent`, and somebody
/// does; see [`private_to`].
#[must_use]
pub fn alone_with(opened: &Opened, at: &ResourceRef, keeper: &MemberId, agent: &MemberId) -> bool {
    let readers = opened.readers(at);
    !readers.is_empty()
        && readers.iter().all(
            |reader| matches!(reader, Actor::Member(member) if member == keeper || member == agent),
        )
}

/// Rich text that mentions `member` as `@name`, then says `then`: what a composer writes when
/// someone mentions an agent, and so what asks it wherever it is written.
#[must_use]
pub fn mentioning(member: &MemberId, name: &str, then: &str) -> Value {
    let mut rich = RichValue::default();
    rich.push_reference(
        &format!("@{name}"),
        &tilolang_tilo::runtime::rich::Reference::Member(member.clone()),
    );
    rich.push_text(then);
    Value::Rich(rich)
}

fn agent(id: &MemberId) -> Actor {
    Actor::Member(id.clone())
}

fn state_value(
    meta: &tilolang_tilo::runtime::meta::Meta,
    state: AskState,
) -> Result<Value, String> {
    let declared = meta
        .enums
        .iter()
        .find(|declared| declared.name == "AskState")
        .ok_or("the agent package has no AskState")?;
    let variant = declared
        .variants
        .iter()
        .find(|variant| variant.name == state.name())
        .ok_or_else(|| format!("AskState has no {}", state.name()))?;
    Ok(Value::Enum(EnumValue {
        ty: declared.id.clone(),
        variant: variant.id.clone(),
        fields: std::collections::BTreeMap::new(),
    }))
}

/// Ask `agent` outright as `asker`, about `from`: what a reminder, an event that fired or a
/// sub-agent's lead asks. `key` names the ask with `from`: a cause that fires again asks
/// nothing new while its ask stands, because the package derives the ask's id from both. `said`
/// is what is asked, in words; empty when the agent may not read `from`. Answers the ask, the
/// one that stood already where there is one.
///
/// # Errors
/// The record holds no agent package, or the ask was refused.
pub fn ask(
    opened: &mut Opened,
    asker: &Actor,
    agent: &MemberId,
    said: &str,
    key: &str,
    from: &ResourceRef,
) -> Result<ResourceRef, String> {
    let meta = meta(opened)?;
    let all = root(&meta)?;
    let made = opened.invoke(
        asker,
        (AGENT_APP, "ask"),
        vec![
            Value::resource(&all),
            Value::Actor(self::agent(agent)),
            Value::Rich(RichValue::from_text(said)),
            Value::Text(key.into()),
        ],
        Some(from),
    )?;
    target(&made).ok_or_else(|| "the ask made nothing".to_owned())
}

/// Start `agent`'s reply to `ask`: the ask is working, and the reply is its newest. Answers the
/// reply.
///
/// # Errors
/// The package refused it (the ask is no longer owed), or it failed.
pub fn answer(
    opened: &mut Opened,
    agent: &MemberId,
    ask: &ResourceRef,
) -> Result<ResourceRef, String> {
    let made = opened.invoke(
        &self::agent(agent),
        (AGENT_APP, "answer"),
        vec![Value::resource(ask)],
        None,
    )?;
    target(&made).ok_or_else(|| "the answer made no reply".to_owned())
}

/// Append `more` to the body of `reply`, as `agent`: each new piece of a streamed answer, never
/// the whole body again.
///
/// # Errors
/// The reply is no longer working, or the package refused it.
pub fn say(
    opened: &mut Opened,
    agent: &MemberId,
    reply: &ResourceRef,
    more: &str,
) -> Result<(), String> {
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "say"),
            vec![
                Value::resource(reply),
                Value::Rich(RichValue::from_text(more)),
            ],
            None,
        )
        .map(drop)
}

/// Append `more` to the thinking of `reply`, as `agent`: what its model thought, as its provider
/// shows it, each new piece and never the whole again.
///
/// # Errors
/// The reply is no longer working, or the package refused it.
pub fn think(
    opened: &mut Opened,
    agent: &MemberId,
    reply: &ResourceRef,
    more: &str,
) -> Result<(), String> {
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "think"),
            vec![Value::resource(reply), Value::Text(more.into())],
            None,
        )
        .map(drop)
}

/// Show on `reply`, as `agent`, a card of `to` under `label`: the thing the agent worked on,
/// live where the reply is drawn, which opens it. It grants nothing: each reader reads it with
/// what they hold, and one who may not read it reads only that something is there, never its
/// label.
///
/// # Errors
/// The reply is no longer working, or the package refused it.
pub fn show(
    opened: &mut Opened,
    agent: &MemberId,
    reply: &ResourceRef,
    (label, to): (&str, &Reference),
) -> Result<(), String> {
    let mut card = RichValue::default();
    card.push_projection(label, to);
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "show"),
            vec![Value::resource(reply), Value::Rich(card)],
            None,
        )
        .map(drop)
}

/// Answer on `reply`, as `agent`, with the view `definition` (tilolang source) called `title`:
/// each reader draws it live with what they may read, and may add it to their own views.
///
/// # Errors
/// The reply is no longer working, the view is empty, or the package refused it.
pub fn show_view(
    opened: &mut Opened,
    agent: &MemberId,
    reply: &ResourceRef,
    (title, definition): (&str, &str),
) -> Result<(), String> {
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "show_view"),
            vec![
                Value::resource(reply),
                Value::Text(title.into()),
                Value::Text(definition.into()),
            ],
            None,
        )
        .map(drop)
}

/// Record on `reply` that `agent` called `called` and what it answered.
///
/// # Errors
/// The package refused it.
pub fn step(
    opened: &mut Opened,
    agent: &MemberId,
    reply: &ResourceRef,
    (called, answered, failed): (&str, &str, bool),
) -> Result<(), String> {
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "step"),
            vec![
                Value::resource(reply),
                Value::Text(called.into()),
                Value::Rich(RichValue::from_text(answered)),
                Value::Bool(failed),
            ],
            None,
        )
        .map(drop)
}

/// End `agent`'s turn on `reply` as `state`: done, failed or stopped, answered by `provider`'s
/// `model` where one answered, its calls costing `cost` millionths of the currency.
///
/// # Errors
/// The package refused it.
pub fn finish(
    opened: &mut Opened,
    agent: &MemberId,
    reply: &ResourceRef,
    state: AskState,
    (provider, model, cost): (&str, &str, u64),
) -> Result<(), String> {
    let meta = meta(opened)?;
    let state = state_value(&meta, state)?;
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "finish"),
            vec![
                Value::resource(reply),
                state,
                Value::Text(provider.into()),
                Value::Text(model.into()),
                Value::Int(i64::try_from(cost).unwrap_or(i64::MAX)),
            ],
            None,
        )
        .map(drop)
}

/// Mark `reply`, `agent`'s answer from this device, superseded by another device's answer to
/// the same ask.
///
/// # Errors
/// The package refused it.
pub fn supersede(opened: &mut Opened, agent: &MemberId, reply: &ResourceRef) -> Result<(), String> {
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "supersede"),
            vec![Value::resource(reply)],
            None,
        )
        .map(drop)
}

/// Keep, as `agent`, a summary of what `conversation` said before the part a turn was shown:
/// `text` says it, reaching `through`, the item it last took in. What the agent kept before is
/// what it says now.
///
/// # Errors
/// The package refused it, or the record could not be written.
pub fn keep_summary(
    opened: &mut Opened,
    agent: &MemberId,
    conversation: &ResourceRef,
    (text, reaches, wrote_at): (&str, &str, &str),
) -> Result<(), String> {
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "keep_summary"),
            vec![
                Value::Actor(self::agent(agent)),
                Value::Text(text.into()),
                Value::Text(reaches.into()),
                Value::Text(wrote_at.into()),
            ],
            Some(conversation),
        )
        .map(drop)
}
