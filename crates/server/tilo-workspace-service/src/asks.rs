//! Asks of agents and their replies, read and written through the agent package
//! (`so.tilo.lang.agent`) on a tilolang record ([`crate::tl_records`]).
//!
//! An ask is a resource of the agent package, kept in the record of whichever app it was made
//! in: a mention in a Chat message makes one in Chat's record, one in a note in Notes'. It is
//! made from what it is about (`created(a)?.from`), and who asked is who made it
//! (`created(a)?.who`). The agent engine never reads an app's own fields: what it reads around
//! an ask is where that resource sits ([`around`]): the collection it is in, each item by its
//! first text and who wrote it, in order.
//!
//! Everything an agent's turn writes goes through the package's own actions, invoked as the
//! agent: [`answer`] starts its reply, [`say`] appends to the reply's body, [`step`] records an
//! action it called and what it answered, [`finish`] ends the turn, [`claim`] takes the turn
//! for a device ([`crate::work::takes_turn`] decides who holds it) and [`supersede`] marks a
//! reply another device's answer stands in place of. [`ask`] makes an ask outright, keyed by its
//! cause: what a reminder, an event or a sub-agent asks.

use tilo_actions::MemberId;
use tilolang_tilo::runtime::rich::RichValue;
use tilolang_tilo::runtime::store::Store as _;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::runtime::value::EnumValue;

use crate::tl_records::{Opened, Packages};
use crate::work::Claim;

/// The agent package.
pub const AGENT_APP: &str = "so.tilo.lang.agent";

/// The agent package and tilolang Chat, which uses it: the record a mention in a conversation
/// asks in.
///
/// # Errors
/// A package does not compile, which is a bug of this build.
pub fn chat_packages() -> Result<Packages, String> {
    Packages::compile(&[
        (
            include_str!("../../../../apps/tl/agent/app.tl"),
            include_str!("../../../../apps/tl/agent/app.ids"),
        ),
        (
            include_str!("../../../../apps/tl/chat/app.tl"),
            include_str!("../../../../apps/tl/chat/app.ids"),
        ),
    ])
}

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
    /// The mention, or the cause, that names the ask with what it is made from.
    pub mention: String,
    pub state: AskState,
    pub claims: Vec<Claim>,
    /// Who asked.
    pub who: Option<Actor>,
    /// What it is about.
    pub from: Option<ResourceRef>,
    /// When it was asked, ISO-8601 UTC.
    pub asked_at: String,
    /// Its replies, in the order they were asked.
    pub replies: Vec<Replied>,
}

impl Asked {
    /// Its newest reply, where it has one.
    #[must_use]
    pub fn latest(&self) -> Option<&Replied> {
        self.replies.iter().max_by_key(|reply| reply.attempt)
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
    pub superseded: bool,
}

/// What sits around what an ask is about: the collection it is in, in order.
#[derive(Clone, Debug, Default)]
pub struct Around {
    /// What that collection's holder is called (a conversation's name).
    pub title: String,
    pub items: Vec<Said>,
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
        Some(Value::Text(text)) => text.to_string(),
        Some(Value::Rich(rich)) => rich.written_text(),
        Some(Value::Opt(Some(inner))) => text(Some(*inner)),
        _ => String::new(),
    }
}

fn int(value: Option<Value>) -> i64 {
    match value {
        Some(Value::Int(n)) => n,
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
        let said = match read(opened, &at, "said") {
            Some(Value::Opt(Some(said))) => Some(text(Some(*said))),
            _ => None,
        };
        let claims = items(opened, &at, "claims")
            .iter()
            .map(|claim| Claim {
                device: text(read(opened, claim, "device")),
                at: int(read(opened, claim, "at")),
            })
            .collect();
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
                attempt: int(read(opened, reply, "attempt")),
                state: state_of(&meta, read(opened, reply, "state")),
                body: text(read(opened, reply, "body")),
                superseded: matches!(read(opened, reply, "superseded"), Some(Value::Bool(true))),
            })
            .collect();
        answers.sort_by_key(|reply| reply.attempt);
        out.push(Asked {
            agent,
            said,
            mention: text(read(opened, &at, "mention")),
            state: state_of(&meta, read(opened, &at, "state")),
            claims,
            who: made.as_ref().and_then(|made| made.who.clone()),
            from: made.as_ref().and_then(|made| made.from.clone()),
            asked_at: made.map(|made| made.at).unwrap_or_default(),
            replies: answers,
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
            title: String::new(),
            items: vec![said(opened, about, true)],
        };
    };
    let title = store
        .first_text(&holder)
        .map(|(text, _)| text)
        .unwrap_or_default();
    let mut all = items(opened, &holder, &field);
    if let Some(at) = all.iter().position(|item| item.same(about)) {
        all.truncate(at.saturating_add(1));
    }
    let skip = all.len().saturating_sub(most);
    Around {
        title,
        items: all
            .iter()
            .skip(skip)
            .map(|item| said(opened, item, item.same(about)))
            .collect(),
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
        text: opened
            .host()
            .store()
            .first_text(at)
            .map(|(text, _)| text)
            .unwrap_or_default(),
        when: made.map(|made| made.at).unwrap_or_default(),
        asked_about,
    }
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
/// nothing new while its ask stands. `said` is what is asked, in words; empty when the agent may
/// not read `from`. Answers the ask, the one that stood already where there is one.
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
    if let Some(standing) = asks(opened)?.into_iter().find(|asked| {
        asked.mention == key && asked.from.as_ref().is_some_and(|made| made.same(from))
    }) {
        return Ok(standing.at);
    }
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
pub fn answer(opened: &mut Opened, agent: &MemberId, ask: &ResourceRef) -> Result<ResourceRef, String> {
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
pub fn say(opened: &mut Opened, agent: &MemberId, reply: &ResourceRef, more: &str) -> Result<(), String> {
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "say"),
            vec![Value::resource(reply), Value::Rich(RichValue::from_text(more))],
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

/// End `agent`'s turn on `reply` as `state`: done, failed or stopped.
///
/// # Errors
/// The package refused it.
pub fn finish(
    opened: &mut Opened,
    agent: &MemberId,
    reply: &ResourceRef,
    state: AskState,
) -> Result<(), String> {
    let meta = meta(opened)?;
    let state = state_value(&meta, state)?;
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "finish"),
            vec![Value::resource(reply), state],
            None,
        )
        .map(drop)
}

/// Claim `ask`'s turn for the device `device` at `at` (milliseconds since 1970), as `agent`.
///
/// # Errors
/// The ask is no longer owed, or the package refused it.
pub fn claim(
    opened: &mut Opened,
    agent: &MemberId,
    ask: &ResourceRef,
    (device, at): (&str, i64),
) -> Result<(), String> {
    opened
        .invoke(
            &self::agent(agent),
            (AGENT_APP, "claim"),
            vec![Value::resource(ask), Value::Text(device.into()), Value::Int(at)],
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
