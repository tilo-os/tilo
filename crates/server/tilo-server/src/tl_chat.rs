//! The workspace's Chat as this server reads and changes it: through Chat's own package on the
//! workspace's record ([`tilo_workspace_service::tl_records`]), its resources read through the
//! package's types and every change made by one of Chat's actions, invoked as the member it is
//! made for. That is how the starting channels a persistent workspace opens with are made and
//! how the moderation agent reads and settles reports: the same actions a person's Chat calls.

use std::sync::OnceLock;

use tilo_actions::MemberId;
use tilo_workspace_service::asks;
use tilo_workspace_service::tl_records::runtime::rich::RichValue;
use tilo_workspace_service::tl_records::runtime::store::Store as _;
use tilo_workspace_service::tl_records::runtime::{Actor, Meta, ResourceRef, Value};
use tilo_workspace_service::tl_records::{Opened, Packages};

/// Chat's app id, as its package names it.
#[must_use]
pub fn app() -> &'static str {
    static APP: OnceLock<String> = OnceLock::new();
    APP.get_or_init(|| {
        packages()
            .map(|packages| packages.app().to_owned())
            .unwrap_or_default()
    })
}

/// The packages Chat's record is read and changed through, built into the server.
///
/// # Errors
/// A package does not compile, which is a bug of this build.
pub fn packages() -> Result<Packages, String> {
    asks::chat_packages()
}

/// The member a record is opened as on this server: Chat itself, so nothing a mention in it
/// asks is made here for anyone.
#[must_use]
pub fn viewer() -> Actor {
    Actor::App(tilo_app_id::AppId::new(app()))
}

/// The member `id` acting.
#[must_use]
pub fn member(id: MemberId) -> Actor {
    Actor::Member(id)
}

/// A category of the sidebar.
#[derive(Clone, Debug)]
pub struct Category {
    pub at: ResourceRef,
    pub name: String,
}

/// A text channel, where it stands in the sidebar.
#[derive(Clone, Debug)]
pub struct Channel {
    pub at: ResourceRef,
    pub name: String,
    pub topic: String,
    /// The category it is listed under; `None` for one outside any.
    pub category: Option<ResourceRef>,
}

/// The sidebar: its categories and every text channel, in its order.
#[derive(Clone, Debug, Default)]
pub struct Sidebar {
    pub categories: Vec<Category>,
    pub channels: Vec<Channel>,
}

impl Sidebar {
    /// The channel named `name`, wherever it is listed.
    #[must_use]
    pub fn channel_named(&self, name: &str) -> Option<&Channel> {
        self.channels.iter().find(|channel| channel.name == name)
    }
}

/// One report filed about a message, kept by the conversation the message is in.
#[derive(Clone, Debug)]
pub struct Report {
    pub at: ResourceRef,
    /// The conversation that keeps it.
    pub conversation: ResourceRef,
    /// The message reported; `None` once it was deleted.
    pub message: Option<ResourceRef>,
    /// Who wrote the reported message.
    pub person: Option<Actor>,
    /// What the message said when it was reported.
    pub said: String,
    /// Who filed it.
    pub by: Option<Actor>,
    /// When, ISO-8601 UTC.
    pub when: String,
    pub reason: String,
    /// Empty while it is open.
    pub decision: String,
}

/// A message, as its conversation holds it.
#[derive(Clone, Debug)]
pub struct Message {
    pub at: ResourceRef,
    /// Who wrote it here: whoever posted it, or the bridge that relayed it.
    pub who: Option<Actor>,
    /// When it was posted here, ISO-8601 UTC.
    pub when: String,
    pub text: String,
    /// Who wrote it where it was written, for a relayed or carried message.
    pub by: String,
    pub written_in: String,
    /// When it was written there, ms since 1970; 0 for one written here.
    pub written: i64,
    pub reply_to: Option<ResourceRef>,
    pub thread: Option<ResourceRef>,
    /// Who reacted, by emoji.
    pub reactions: Vec<(String, Vec<Actor>)>,
}

fn meta(opened: &Opened) -> Result<Meta, String> {
    opened
        .host()
        .package(opened.app())
        .map(|package| package.meta)
        .ok_or_else(|| "Chat's package is not loaded".to_owned())
}

fn type_id(meta: &Meta, name: &str) -> Result<String, String> {
    meta.type_named(name)
        .map(|ty| ty.id.clone())
        .ok_or_else(|| format!("Chat has no type {name}"))
}

/// Whether `at` is of Chat's type `name`.
fn is(meta: &Meta, at: &ResourceRef, name: &str) -> bool {
    meta.type_named(name).is_some_and(|ty| ty.id == at.ty)
}

/// Chat's root in this record: the sidebar's conversations.
///
/// # Errors
/// Chat's package is not loaded.
pub fn root(opened: &Opened) -> Result<ResourceRef, String> {
    let ty = type_id(&meta(opened)?, "Conversations")?;
    Ok(ResourceRef::root(format!("root-{ty}"), ty))
}

/// The resource `id` names ([`ResourceRef::id_string`]) as Chat's `ty`: how a mapping kept
/// outside the record names a channel or a message again.
///
/// # Errors
/// Chat has no such type, or `id` names no resource.
pub fn resource(opened: &Opened, ty: &str, id: &str) -> Result<ResourceRef, String> {
    let ty = type_id(&meta(opened)?, ty)?;
    ResourceRef::parse_id(id, ty).ok_or_else(|| format!("{id} names no resource"))
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

fn target(value: &Value) -> Option<ResourceRef> {
    match value {
        Value::Ref(reference) => Some((*reference.target).clone()),
        Value::Opt(Some(inner)) => target(inner),
        _ => None,
    }
}

fn items(opened: &Opened, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match read(opened, at, field) {
        Some(Value::List(items)) => items.iter().filter_map(target).collect(),
        _ => Vec::new(),
    }
}

/// Who made `at`, and when.
fn made(opened: &Opened, at: &ResourceRef) -> (Option<Actor>, String) {
    opened
        .host()
        .store()
        .versions(at)
        .ok()
        .and_then(|versions| versions.into_iter().next())
        .map_or((None, String::new()), |made| (made.who, made.at))
}

/// The sidebar as the record holds it now.
///
/// # Errors
/// Chat's package is not loaded.
pub fn sidebar(opened: &Opened) -> Result<Sidebar, String> {
    let meta = meta(opened)?;
    let root = root(opened)?;
    let mut sidebar = Sidebar::default();
    let channel = |at: ResourceRef, category: Option<ResourceRef>| Channel {
        name: text(read(opened, &at, "name")),
        topic: text(read(opened, &at, "topic")),
        at,
        category,
    };
    for item in items(opened, &root, "channels") {
        if is(&meta, &item, "TextChannel") {
            sidebar.channels.push(channel(item, None));
        } else if is(&meta, &item, "Category") {
            for inner in items(opened, &item, "channels") {
                sidebar.channels.push(channel(inner, Some(item.clone())));
            }
            sidebar.categories.push(Category {
                name: text(read(opened, &item, "name")),
                at: item,
            });
        }
    }
    Ok(sidebar)
}

/// The messages of `conversation` (a channel, a thread or a direct conversation), oldest first.
#[must_use]
pub fn messages(opened: &Opened, conversation: &ResourceRef) -> Vec<Message> {
    items(opened, conversation, "messages")
        .into_iter()
        .map(|at| message(opened, at))
        .collect()
}

/// The message at `at`, as the record holds it now.
#[must_use]
pub fn message(opened: &Opened, at: ResourceRef) -> Message {
    let (who, when) = made(opened, &at);
    let reactions = match read(opened, &at, "reactions") {
        Some(Value::List(entries)) => entries
            .into_iter()
            .filter_map(|entry| match entry {
                Value::Tuple(pair) => match pair.as_slice() {
                    [Value::Text(emoji), Value::List(who)] => Some((
                        emoji.clone(),
                        who.iter()
                            .filter_map(|actor| match actor {
                                Value::Actor(actor) => Some(actor.clone()),
                                _ => None,
                            })
                            .collect(),
                    )),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Message {
        who,
        when,
        text: text(read(opened, &at, "body")),
        by: text(read(opened, &at, "by")),
        written_in: text(read(opened, &at, "written_in")),
        written: match read(opened, &at, "written") {
            Some(Value::Int(ms)) => ms,
            _ => 0,
        },
        reply_to: read(opened, &at, "reply_to").as_ref().and_then(target),
        thread: read(opened, &at, "thread").as_ref().and_then(target),
        reactions,
        at,
    }
}

/// Every message of every channel and of every channel's threads, each with the conversation
/// it is in.
///
/// # Errors
/// Chat's package is not loaded.
pub fn all_messages(opened: &Opened) -> Result<Vec<(ResourceRef, Message)>, String> {
    let mut all = Vec::new();
    for channel in sidebar(opened)?.channels {
        for message in messages(opened, &channel.at) {
            if let Some(thread) = &message.thread {
                for reply in messages(opened, thread) {
                    all.push((thread.clone(), reply));
                }
            }
            all.push((channel.at.clone(), message));
        }
    }
    Ok(all)
}

/// Run Chat's action `action` with `args` as `who`.
fn call(opened: &mut Opened, who: &Actor, action: &str, args: Vec<Value>) -> Result<Value, String> {
    let app = opened.app().to_owned();
    opened.invoke(who, (&app, action), args, None)
}

/// The reports `conversation` keeps, oldest first.
#[must_use]
pub fn reports_in(opened: &Opened, conversation: &ResourceRef) -> Vec<Report> {
    items(opened, conversation, "reports")
        .into_iter()
        .map(|at| {
            let (by, when) = made(opened, &at);
            let person = match read(opened, &at, "person") {
                Some(Value::Opt(Some(actor))) => match *actor {
                    Value::Actor(actor) => Some(actor),
                    _ => None,
                },
                Some(Value::Actor(actor)) => Some(actor),
                _ => None,
            };
            Report {
                conversation: conversation.clone(),
                message: read(opened, &at, "message").as_ref().and_then(target),
                person,
                said: text(read(opened, &at, "said")),
                by,
                when,
                reason: text(read(opened, &at, "reason")),
                decision: text(read(opened, &at, "decision")),
                at,
            }
        })
        .collect()
}

/// Every report the workspace's channels and their threads keep.
///
/// # Errors
/// Chat's package is not loaded.
pub fn reports(opened: &Opened) -> Result<Vec<Report>, String> {
    let mut all = Vec::new();
    for channel in sidebar(opened)?.channels {
        all.extend(reports_in(opened, &channel.at));
        for thread in items(opened, &channel.at, "threads") {
            all.extend(reports_in(opened, &thread));
        }
    }
    Ok(all)
}

fn made_ref(value: &Value, what: &str) -> Result<ResourceRef, String> {
    target(value).ok_or_else(|| format!("{what} made nothing"))
}

fn rich(text: &str) -> Value {
    Value::Rich(RichValue::from_text(text))
}

fn option(reference: Option<&ResourceRef>) -> Value {
    Value::Opt(reference.map(|at| Box::new(Value::resource(at))))
}

/// Make the channel `name` about `topic` as `who`: in `category`, or outside any.
///
/// # Errors
/// Chat refused it.
pub fn new_channel(
    opened: &mut Opened,
    who: &Actor,
    (name, topic): (&str, &str),
    category: Option<&ResourceRef>,
) -> Result<ResourceRef, String> {
    let made = if let Some(category) = category {
        call(
            opened,
            who,
            "add_channel",
            vec![
                Value::resource(category),
                Value::Text(name.into()),
                Value::Text(topic.into()),
            ],
        )?
    } else {
        // The first channel of a new workspace founds Chat's root.
        let all = opened.open_root()?;
        call(
            opened,
            who,
            "new_channel",
            vec![
                Value::resource(&all),
                Value::Text(name.into()),
                Value::Text(topic.into()),
            ],
        )?
    };
    made_ref(&made, "new_channel")
}

/// Make the category `name` as `who`.
///
/// # Errors
/// Chat refused it.
pub fn new_category(opened: &mut Opened, who: &Actor, name: &str) -> Result<ResourceRef, String> {
    let all = opened.open_root()?;
    let made = call(
        opened,
        who,
        "new_category",
        vec![Value::resource(&all), Value::Text(name.into())],
    )?;
    made_ref(&made, "new_category")
}

/// Delete `channel` (listed in `category`, or outside any) as `who`.
///
/// # Errors
/// Chat refused it.
pub fn delete_channel(
    opened: &mut Opened,
    who: &Actor,
    channel: &ResourceRef,
    category: Option<&ResourceRef>,
) -> Result<(), String> {
    let from = match category {
        Some(category) => category.clone(),
        None => root(opened)?,
    };
    call(
        opened,
        who,
        "delete_channel",
        vec![Value::resource(&from), Value::resource(channel)],
    )
    .map(drop)
}

/// Delete the empty `category` as `who`.
///
/// # Errors
/// Chat refused it: the category holds a channel.
pub fn delete_category(
    opened: &mut Opened,
    who: &Actor,
    category: &ResourceRef,
) -> Result<(), String> {
    let all = root(opened)?;
    call(
        opened,
        who,
        "delete_category",
        vec![Value::resource(&all), Value::resource(category)],
    )
    .map(drop)
}

/// Post `text` in `to` as `who`, answering `reply_to`.
///
/// # Errors
/// Chat refused it.
pub fn send(
    opened: &mut Opened,
    who: &Actor,
    to: &ResourceRef,
    text: &str,
    reply_to: Option<&ResourceRef>,
) -> Result<ResourceRef, String> {
    let made = call(
        opened,
        who,
        "send",
        vec![Value::resource(to), rich(text), option(reply_to)],
    )?;
    made_ref(&made, "send")
}

/// Where a relayed message was written: who wrote it, the service and when (ms since 1970).
#[derive(Clone, Debug)]
pub struct WrittenElsewhere<'a> {
    pub by: &'a str,
    pub service: &'a str,
    pub written: i64,
}

/// Relay `text`, written elsewhere, into `to` as `who` (the bridge).
///
/// # Errors
/// Chat refused it.
pub fn relay(
    opened: &mut Opened,
    who: &Actor,
    to: &ResourceRef,
    text: &str,
    from: &WrittenElsewhere<'_>,
    reply_to: Option<&ResourceRef>,
) -> Result<ResourceRef, String> {
    let made = call(
        opened,
        who,
        "relay",
        vec![
            Value::resource(to),
            rich(text),
            Value::Text(from.by.into()),
            Value::Text(from.service.into()),
            Value::Int(from.written),
            Value::List(Vec::new()),
            option(reply_to),
        ],
    )?;
    made_ref(&made, "relay")
}

/// Change what `message` says to `text`, as its author `who`.
///
/// # Errors
/// Chat refused it: `who` did not write it.
pub fn edit(
    opened: &mut Opened,
    who: &Actor,
    message: &ResourceRef,
    text: &str,
) -> Result<(), String> {
    call(
        opened,
        who,
        "edit",
        vec![Value::resource(message), rich(text)],
    )
    .map(drop)
}

/// Delete `message` from `conversation`, as `who`.
///
/// # Errors
/// Chat refused it.
pub fn delete(
    opened: &mut Opened,
    who: &Actor,
    conversation: &ResourceRef,
    message: &ResourceRef,
) -> Result<(), String> {
    call(
        opened,
        who,
        "delete",
        vec![Value::resource(conversation), Value::resource(message)],
    )
    .map(drop)
}

/// Add `who`'s reaction `emoji` to `message`, or take it back where they had it.
///
/// # Errors
/// Chat refused it.
pub fn react(
    opened: &mut Opened,
    who: &Actor,
    message: &ResourceRef,
    emoji: &str,
) -> Result<(), String> {
    call(
        opened,
        who,
        "react",
        vec![Value::resource(message), Value::Text(emoji.into())],
    )
    .map(drop)
}

/// Report `message`, of `conversation`, to the workspace's moderators as `who`, saying `reason`.
///
/// # Errors
/// Chat refused it.
pub fn report(
    opened: &mut Opened,
    who: &Actor,
    conversation: &ResourceRef,
    message: &ResourceRef,
    reason: &str,
) -> Result<ResourceRef, String> {
    let made = call(
        opened,
        who,
        "report",
        vec![
            Value::resource(conversation),
            Value::resource(message),
            Value::Text(reason.into()),
        ],
    )?;
    made_ref(&made, "report")
}

/// Settle `report` with `decision`, as `who`.
///
/// # Errors
/// Chat refused it.
pub fn decide(
    opened: &mut Opened,
    who: &Actor,
    report: &ResourceRef,
    decision: &str,
) -> Result<(), String> {
    call(
        opened,
        who,
        "decide",
        vec![Value::resource(report), Value::Text(decision.into())],
    )
    .map(drop)
}
