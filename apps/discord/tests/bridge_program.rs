//! The Discord bridge bot's program (discord-bridge.md §3), built here and
//! kept as the canonical file `bridge/program.json` that the bot directory
//! publishes. The test checks it against the real Chat and Discord action
//! surfaces, stores the grant its steps derive, and fails when the file is
//! stale; `BLESS=1` rewrites it.
//!
//! The program is a fixed run of loops: up to six passes each read tl Chat's tree through the
//! core read, ask `plan-event` what to do and loop over its lists. A pass runs only while the
//! previous plan asked for another, so a reply waits for its thread and a post for its
//! author's avatar.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::collections::BTreeMap;

use discord::bridge;
use discord::resource::Discord;
use discord::{APP_ID, Guild, KIND};
use serde_json::{Value, json};
use tilo_automation::{
    ActorFilter, Binding, Condition, Selector, Shortcut, Step, StepKind, TriggerSpec,
};
use tilo_workspace_service::tl_records::runtime::Actor;
use tilo_workspace_service::tl_records::runtime::Value as TlValue;
use tilo_workspace_service::tl_records::{Opened, ResourceStore};
use ui::framework::testing::TestApp;
use ui::tilo_actions::ActionKey;
use ui::tilo_actions::params::{ActionParam, ParamType};
use ui::tilo_actions::remote::CatalogueRow;
use ui::tilo_resource::MemberId;

const CHAT_APP: &str = "so.tilo.chat";
/// Chat's own package resource: the actions whose first parameter takes any of several kinds are
/// published there, with the resource among their arguments.
const PACKAGE: &str = "tl.so.tilo.chat";
/// Chat's root kind: the sidebar's conversations.
const CONVERSATIONS: &str = "tl.so.tilo.chat.Conversations";
const CATEGORY: &str = "tl.so.tilo.chat.Category";
const TEXT_CHANNEL: &str = "tl.so.tilo.chat.TextChannel";
const VOICE_CHANNEL: &str = "tl.so.tilo.chat.VoiceChannel";
const THREAD: &str = "tl.so.tilo.chat.Thread";
const MESSAGE: &str = "tl.so.tilo.chat.Message";
const DIRECT: &str = "tl.so.tilo.chat.Direct";
const REPORT: &str = "tl.so.tilo.chat.Report";
/// Every kind of Chat's the bridge follows.
const CHAT_KINDS: [&str; 8] = [
    CONVERSATIONS,
    CATEGORY,
    TEXT_CHANNEL,
    VOICE_CHANNEL,
    THREAD,
    MESSAGE,
    DIRECT,
    REPORT,
];
const PASSES: usize = 6;
/// The workspace's governance verbs the bridge follows.
const MEMBER_VERBS: [&str; 5] = ["add-member", "set-role", "remove-member", "ban", "unban"];

fn input(name: &str) -> Binding {
    Binding::Input(name.into())
}

/// The loop's item at `pointer`, or the whole item for `""`: `item/` would
/// point at a key named "".
fn item(pointer: &str) -> Binding {
    if pointer.is_empty() {
        Binding::Var("item".into())
    } else {
        Binding::Var(format!("item/{pointer}"))
    }
}

fn text(value: &str) -> Binding {
    Binding::Literal(json!(value))
}

fn invoke(id: &str, key: ActionKey, resource: Selector, args: &[(&str, Binding)]) -> Step {
    Step::new(
        id,
        StepKind::Invoke {
            action: key,
            resource,
            args: args
                .iter()
                .map(|(name, binding)| ((*name).to_owned(), binding.clone()))
                .collect::<BTreeMap<_, _>>(),
            appearance: None,
        },
    )
}

/// A verb or effect of the bridged guild.
fn guild(id: &str, action: &str, args: &[(&str, Binding)]) -> Step {
    invoke(
        id,
        ActionKey::new(APP_ID, KIND, action),
        Selector::FirstOfKind(KIND.into()),
        args,
    )
}

/// A record named by a reference, as a parameter that picks one takes it
/// ([`tilo_actions::params::resource_reference_schema`]): its app, its kind, its id, and the
/// label, which nothing reads.
fn reference(kind: Binding, id: Binding) -> Binding {
    Binding::Object(BTreeMap::from([
        ("app".to_owned(), Binding::Literal(json!(CHAT_APP))),
        ("id".to_owned(), id),
        ("kind".to_owned(), kind),
        ("label".to_owned(), Binding::Literal(json!(""))),
    ]))
}

/// A Chat action on the target `selector`.
fn chat(id: &str, kind: &str, action: &str, selector: Selector, args: &[(&str, Binding)]) -> Step {
    invoke(id, ActionKey::new(CHAT_APP, kind, action), selector, args)
}

/// A Chat action whose first parameter takes a resource of several kinds: it is published on the
/// package, with that resource among its arguments.
fn on_package(id: &str, action: &str, args: &[(&str, Binding)]) -> Step {
    chat(
        id,
        PACKAGE,
        action,
        Selector::FirstOfKind(PACKAGE.into()),
        args,
    )
}

/// A Chat action on the sidebar's root: the package's own verb.
fn on_chat(id: &str, action: &str, args: &[(&str, Binding)]) -> Step {
    chat(
        id,
        CONVERSATIONS,
        action,
        Selector::FirstOfKind(CONVERSATIONS.into()),
        args,
    )
}

/// A Chat action on the conversation or message a plan element names.
fn on_item(id: &str, kind: &str, action: &str, pointer: &str, args: &[(&str, Binding)]) -> Step {
    chat(id, kind, action, Selector::FromBinding(item(pointer)), args)
}

/// The id of the item step `step` created, as text.
fn created(step: &str) -> Binding {
    Binding::Template(format!("{{{{step.{step}}}}}"))
}

/// A resource named by its published id, of one of a union's kinds: a `from` or `ch` argument
/// that takes several kinds is given `{kind, id}`.
fn union(kind: Binding, id: Binding) -> Binding {
    Binding::Object(BTreeMap::from([
        ("kind".to_owned(), kind),
        ("id".to_owned(), id),
    ]))
}

/// A file for Chat to keep with a relayed message: what the fetch answered, as Chat reads a
/// `File`.
fn fetched(step: &str) -> Binding {
    Binding::Object(BTreeMap::from([(
        "file".to_owned(),
        Binding::Object(BTreeMap::from([
            ("name".to_owned(), Binding::step(step, "/attachment/name")),
            ("media".to_owned(), Binding::step(step, "/content_type")),
            ("size".to_owned(), Binding::step(step, "/size")),
            ("hash".to_owned(), Binding::step(step, "/blob")),
        ])),
    )]))
}

/// `map` a result: `tilo` and `discord` as bound, of `kind` and `origin`.
fn map(id: &str, kind: &str, tilo: Binding, discord: Binding, origin: &str) -> Step {
    guild(
        id,
        "map",
        &[
            ("kind", text(kind)),
            ("tilo", tilo),
            ("discord", discord),
            ("origin", text(origin)),
        ],
    )
}

fn each(list: &str, body: Vec<Step>) -> Step {
    Step::new(
        list,
        StepKind::ForEach {
            items: Binding::step("plan", format!("/{list}")),
            item: "item".into(),
            body,
        },
    )
}

/// Relay what Discord wrote into the conversation the plan names, then map the message it made.
fn relayed(step: &str, with_files: bool, extra: &[(&str, Binding)]) -> Vec<Step> {
    let posted = format!("{step}-relay");
    let mut args: Vec<(&str, Binding)> = vec![
        ("to", union(item("channel_kind"), item("channel"))),
        ("body", item("text")),
        ("by", item("by")),
        ("written_in", item("written_in")),
        ("written", item("written")),
        ("reply_to", item("reply_to")),
    ];
    if with_files {
        let fetch = if step.starts_with("file-import") {
            "fetch-imported-file"
        } else {
            "fetch-file"
        };
        args.push(("attachments", Binding::List(vec![fetched(fetch)])));
    }
    args.extend(extra.iter().cloned());
    let mut mapped = map(
        &format!("{step}-map"),
        "message",
        created(&posted),
        item("discord"),
        "discord",
    );
    if let StepKind::Invoke { args, .. } = &mut mapped.kind {
        args.insert("persona".into(), item("persona"));
    }
    vec![on_package(&posted, "relay", &args), mapped]
}

#[allow(
    clippy::too_many_lines,
    reason = "one loop per planned verb, read top to bottom"
)]
fn pass() -> Vec<Step> {
    let plan_step = |id: &str, pointer: &str| Binding::step(id, pointer);
    vec![
        // Chat's tree: every conversation with its channels, messages and threads.
        Step::new(
            "chat",
            StepKind::Read {
                app: CHAT_APP.into(),
                record: "tree".into(),
                joined: false,
            },
        ),
        guild(
            "plan",
            "plan-event",
            &[
                ("event", input("event")),
                ("chat", plan_step("chat", "")),
                ("context", input("context")),
                // The host tells a program the workspace's members and
                // their roles under this input's name.
                ("workspace", input("workspace")),
                ("emoji", input("emoji")),
            ],
        ),
        each(
            "observe",
            vec![guild(
                "observe-layout",
                "observe-layout",
                &[
                    ("bot_user", item("bot_user")),
                    ("channels", item("channels")),
                    ("roles", item("roles")),
                    ("emojis", item("emojis")),
                ],
            )],
        ),
        each(
            "threads",
            vec![
                guild("start-thread", "start-thread", &[("op", item(""))]),
                map(
                    "map-thread",
                    "thread",
                    item("tilo"),
                    plan_step("start-thread", "/thread_id"),
                    "tilo",
                ),
            ],
        ),
        each("sends", {
            let mut mapped = map(
                "map-send",
                "message",
                item("op/tilo"),
                plan_step("send", "/message_id"),
                "tilo",
            );
            if let StepKind::Invoke { args, .. } = &mut mapped.kind {
                args.insert("root".into(), item("root"));
            }
            // A send that found the channel's webhook gone from Discord
            // went out through a new one, which takes the old one's place.
            let replaced = Step::new(
                "replaced-webhook",
                StepKind::If {
                    cond: Condition::Truthy(plan_step("send", "/webhook_id")),
                    then: vec![map(
                        "map-replaced-webhook",
                        "webhook",
                        item("channel"),
                        plan_step("send", "/webhook_id"),
                        "tilo",
                    )],
                    otherwise: Vec::new(),
                },
            );
            vec![
                guild("send", "send", &[("op", item("op"))]),
                replaced,
                mapped,
            ]
        }),
        each("edits", vec![guild("edit", "edit", &[("op", item(""))])]),
        each(
            "deletes",
            vec![
                guild("delete", "delete", &[("op", item("op"))]),
                guild(
                    "unmap-deleted",
                    "unmap",
                    &[("kind", text("message")), ("tilo", item("tilo"))],
                ),
            ],
        ),
        each("reacts", vec![guild("react", "react", &[("op", item(""))])]),
        each(
            "unreacts",
            vec![guild("unreact", "unreact", &[("op", item(""))])],
        ),
        each(
            "apply_channels",
            vec![
                guild("apply-channel", "apply-channel", &[("op", item("op"))]),
                guild(
                    "map-channel",
                    "map",
                    &[
                        ("kind", item("kind")),
                        ("tilo", item("op/tilo")),
                        ("discord", plan_step("apply-channel", "/channel_id")),
                        ("origin", text("tilo")),
                    ],
                ),
            ],
        ),
        each(
            "arrange_channels",
            vec![guild(
                "arrange-channels",
                "arrange-channels",
                &[("op", item(""))],
            )],
        ),
        each(
            "delete_channels",
            vec![guild(
                "delete-channel",
                "delete-channel",
                &[("op", item(""))],
            )],
        ),
        each(
            "unmaps",
            vec![guild(
                "unmap",
                "unmap",
                &[
                    ("kind", item("kind")),
                    ("tilo", item("tilo")),
                    ("keep_apart", item("keep_apart")),
                ],
            )],
        ),
        each(
            "webhooks",
            vec![
                guild("create-webhook", "create-webhook", &[("op", item(""))]),
                map(
                    "map-webhook",
                    "webhook",
                    item("tilo"),
                    plan_step("create-webhook", "/webhook_id"),
                    "tilo",
                ),
            ],
        ),
        each(
            "apply_roles",
            vec![
                guild("apply-role", "apply-role", &[("op", item("op"))]),
                map(
                    "map-role",
                    "role",
                    item("op/tilo"),
                    plan_step("apply-role", "/role_id"),
                    "tilo",
                ),
            ],
        ),
        each(
            "overwrites",
            vec![guild("set-overwrite", "set-overwrite", &[("op", item(""))])],
        ),
        // Deletes before uploads: a picture replaced under one name frees
        // its slot first.
        each(
            "delete_emojis",
            vec![
                guild("delete-emoji", "delete-emoji", &[("op", item(""))]),
                guild(
                    "settle-deleted-emoji",
                    "settle-emoji",
                    &[
                        ("tilo", item("tilo")),
                        ("result", plan_step("delete-emoji", "")),
                        ("deleted", Binding::Literal(json!(true))),
                    ],
                ),
            ],
        ),
        each(
            "apply_emojis",
            vec![
                guild("apply-emoji", "apply-emoji", &[("op", item("op"))]),
                guild(
                    "settle-emoji",
                    "settle-emoji",
                    &[
                        ("tilo", item("op/tilo")),
                        ("result", plan_step("apply-emoji", "")),
                        ("picture", item("picture")),
                        ("was", item("was")),
                    ],
                ),
            ],
        ),
        each(
            "member_roles",
            vec![guild(
                "set-member-roles",
                "set-member-roles",
                &[("op", item(""))],
            )],
        ),
        each("kicks", vec![guild("kick", "kick", &[("op", item(""))])]),
        each("bans", vec![guild("ban", "ban", &[("op", item(""))])]),
        each("unbans", vec![guild("unban", "unban", &[("op", item(""))])]),
        each(
            "avatars",
            vec![
                guild("fetch-avatar", "fetch-asset", &[("op", item("op"))]),
                guild(
                    "remember-avatar",
                    "remember-avatar",
                    &[
                        ("hash", item("hash")),
                        ("blob", plan_step("fetch-avatar", "/blob")),
                    ],
                ),
            ],
        ),
        each("posts", relayed("in", false, &[])),
        each("files", {
            let mut body = vec![guild("fetch-file", "fetch-asset", &[("op", item("fetch"))])];
            body.extend(relayed("file", true, &[]));
            body
        }),
        each("imports", relayed("import", false, &[])),
        each("file_imports", {
            let mut body = vec![guild(
                "fetch-imported-file",
                "fetch-asset",
                &[("op", item("fetch"))],
            )];
            body.extend(relayed("file-import", true, &[]));
            body
        }),
        each(
            "chat_edits",
            vec![on_item(
                "edit-message",
                MESSAGE,
                "edit",
                "message",
                &[("body", item("text"))],
            )],
        ),
        each(
            "chat_deletes",
            vec![
                on_package(
                    "delete-message",
                    "delete",
                    &[
                        ("to", union(item("channel_kind"), item("channel"))),
                        ("m", item("message")),
                    ],
                ),
                guild(
                    "unmap-chat-deleted",
                    "unmap",
                    &[("kind", text("message")), ("tilo", item("message"))],
                ),
            ],
        ),
        each(
            "chat_removes",
            vec![
                on_package(
                    "remove-message",
                    "delete",
                    &[
                        ("to", union(item("channel_kind"), item("channel"))),
                        ("m", item("message")),
                    ],
                ),
                guild(
                    "unmap-removed",
                    "unmap",
                    &[("kind", text("message")), ("tilo", item("message"))],
                ),
            ],
        ),
        each(
            "chat_reacts",
            vec![on_item(
                "chat-react",
                MESSAGE,
                "relay_reaction",
                "message",
                &[
                    ("emoji", item("emoji")),
                    ("persona", item("persona")),
                    ("by", item("by")),
                    ("given", Binding::Literal(json!(true))),
                ],
            )],
        ),
        each(
            "chat_unreacts",
            vec![on_item(
                "chat-unreact",
                MESSAGE,
                "relay_reaction",
                "message",
                &[
                    ("emoji", item("emoji")),
                    ("persona", item("persona")),
                    ("by", item("by")),
                    ("given", Binding::Literal(json!(false))),
                ],
            )],
        ),
        each(
            "blocks",
            vec![guild(
                "block-persona",
                "block-persona",
                &[("discord_user", item("discord_user"))],
            )],
        ),
        each(
            "offers",
            vec![guild(
                "offer-link",
                "offer-link",
                &[
                    ("check", item("check")),
                    ("discord_user", item("discord_user")),
                    ("user_name", item("user_name")),
                    ("display_name", item("display_name")),
                    ("avatar", item("avatar")),
                    ("interaction", item("interaction")),
                ],
            )],
        ),
        each(
            "sign_in_offers",
            vec![guild(
                "offer-sign-in",
                "offer-sign-in",
                &[
                    ("check", item("check")),
                    ("discord_user", item("discord_user")),
                    ("user_name", item("user_name")),
                    ("display_name", item("display_name")),
                    ("avatar", item("avatar")),
                    ("state_sha256", item("state_sha256")),
                ],
            )],
        ),
        each(
            "sign_ins",
            vec![guild(
                "set-sign-in",
                "set-sign-in",
                &[("page", item("page"))],
            )],
        ),
        each(
            "answers",
            vec![guild(
                "answer-interaction",
                "answer-interaction",
                &[("op", item(""))],
            )],
        ),
        each(
            "unlinks",
            vec![guild(
                "unlink",
                "unlink",
                &[("discord_user", item("discord_user"))],
            )],
        ),
        each(
            "renames",
            vec![guild(
                "rename-link",
                "rename-link",
                &[
                    ("discord_user", item("discord_user")),
                    ("user_name", item("user_name")),
                    ("display_name", item("display_name")),
                    ("avatar", item("avatar")),
                ],
            )],
        ),
        each(
            "photos",
            vec![
                guild("fetch-photo", "fetch-asset", &[("op", item("op"))]),
                guild(
                    "set-link-photo",
                    "set-link-photo",
                    &[
                        ("discord_user", item("discord_user")),
                        ("avatar", item("avatar")),
                        ("photo", plan_step("fetch-photo", "/data_url")),
                    ],
                ),
            ],
        ),
        each(
            "import_categories",
            vec![
                on_chat("create-category", "new_category", &[("name", item("name"))]),
                map(
                    "map-category",
                    "category",
                    created("create-category"),
                    item("discord"),
                    "discord",
                ),
            ],
        ),
        each("import_channels", imported_channel()),
        each(
            "adopt",
            vec![guild(
                "adopt-map",
                "map",
                &[
                    ("kind", item("kind")),
                    ("tilo", item("tilo")),
                    ("discord", item("discord")),
                    ("origin", text("discord")),
                ],
            )],
        ),
        each(
            "file_channels",
            vec![on_chat(
                "file-channel",
                "move_channel",
                &[
                    ("ch", union(item("kind"), item("channel"))),
                    ("category", item("category")),
                    ("at", Binding::Literal(json!(-1))),
                ],
            )],
        ),
        each("drop_channels", dropped_channel()),
        each(
            "drop_categories",
            vec![on_chat(
                "drop-category",
                "delete_category",
                &[("c", reference(text(CATEGORY), item("category")))],
            )],
        ),
        each("chat_moves", moved()),
        each(
            "finish",
            vec![guild(
                "finish-import",
                "finish-import",
                &[("tilo_only", item("tilo_only"))],
            )],
        ),
        Step::new(
            "again",
            StepKind::SetVar {
                name: "more".into(),
                value: plan_step("plan", "/again"),
            },
        ),
    ]
}

/// A Discord channel imported into Chat: a category's channel or a top-level one, text or voice,
/// then mapped to its Discord channel.
fn imported_channel() -> Vec<Step> {
    let made = |action: &str, target: Selector, id: &str| {
        let args: Vec<(&str, Binding)> = if action.contains("voice") {
            vec![("name", item("name"))]
        } else {
            vec![
                ("name", item("name")),
                ("topic", Binding::Literal(json!(""))),
            ]
        };
        let mut steps = vec![chat(id, &kind_of(action), action, target, &args)];
        steps.push(map(
            &format!("map-{id}"),
            "channel",
            created(id),
            item("discord"),
            "discord",
        ));
        steps
    };
    let in_category =
        |action: &str, id: &str| made(action, Selector::FromBinding(item("category")), id);
    let at_top =
        |action: &str, id: &str| made(action, Selector::FirstOfKind(CONVERSATIONS.into()), id);
    vec![Step::new(
        "import-channel-kind",
        StepKind::If {
            cond: Condition::Equals {
                left: item("kind"),
                right: text(VOICE_CHANNEL),
            },
            then: vec![Step::new(
                "import-voice-place",
                StepKind::If {
                    cond: Condition::Truthy(item("category")),
                    then: in_category("add_voice_channel", "add-voice"),
                    otherwise: at_top("new_voice_channel", "new-voice"),
                },
            )],
            otherwise: vec![Step::new(
                "import-text-place",
                StepKind::If {
                    cond: Condition::Truthy(item("category")),
                    then: in_category("add_channel", "add-text"),
                    otherwise: at_top("new_channel", "new-text"),
                },
            )],
        },
    )]
}

/// The kind an action is published under: the type of the resource it is invoked on.
fn kind_of(action: &str) -> String {
    match action {
        "add_voice_channel" | "add_channel" => CATEGORY.to_owned(),
        _ => CONVERSATIONS.to_owned(),
    }
}

/// A channel of the workspace's the server had no match for and nothing was written in: deleted
/// from the category it is listed in, or from the sidebar.
fn dropped_channel() -> Vec<Step> {
    vec![on_package(
        "drop-channel",
        "delete_channel",
        &[
            ("from", union(item("from_kind"), item("from"))),
            ("ch", union(item("kind"), item("channel"))),
        ],
    )]
}

/// One move of Chat's sidebar into Discord's order: a category put at its place, or a channel put
/// at its place in its category (or at the top).
fn moved() -> Vec<Step> {
    vec![Step::new(
        "chat-move-kind",
        StepKind::If {
            cond: Condition::Equals {
                left: item("kind"),
                right: text("category"),
            },
            then: vec![on_chat(
                "move-a-category",
                "move_category",
                &[("c", item("id")), ("at", item("at"))],
            )],
            otherwise: vec![on_chat(
                "move-a-channel",
                "move_channel",
                &[
                    ("ch", union(item("resource_kind"), item("id"))),
                    ("category", item("category")),
                    ("at", item("at")),
                ],
            )],
        },
    )]
}

fn triggers() -> Vec<TriggerSpec> {
    let mut triggers: Vec<TriggerSpec> = CHAT_KINDS
        .iter()
        .map(|kind| TriggerSpec::ResourceChanged {
            kind: (*kind).into(),
            actions: Vec::new(),
            fields: Vec::new(),
            by: ActorFilter::Anyone,
        })
        .collect();
    // The workspace's own governance: who joined, whose role changed,
    // who was removed or banned.
    triggers.push(TriggerSpec::ResourceChanged {
        kind: "tilo.workspace".into(),
        actions: MEMBER_VERBS.map(str::to_owned).to_vec(),
        fields: Vec::new(),
        by: ActorFilter::Anyone,
        filter: tilo_automation::ChangeFilter::default(),
    });
    // The workspace's emoji, which the bridge uploads to the server.
    triggers.push(TriggerSpec::ResourceChanged {
        kind: bridge::EMOJI_KIND.into(),
        actions: Vec::new(),
        fields: Vec::new(),
        by: ActorFilter::Anyone,
        filter: tilo_automation::ChangeFilter::default(),
    });
    triggers.push(TriggerSpec::ResourceChanged {
        kind: KIND.into(),
        actions: vec!["block-persona".to_owned(), "complete-link".to_owned()],
        fields: Vec::new(),
        by: ActorFilter::Anyone,
        filter: tilo_automation::ChangeFilter::default(),
    });
    triggers.extend(
        discord_wire::event::source::ALL
            .iter()
            .map(|source| TriggerSpec::Connector {
                app_id: APP_ID.into(),
                source: (*source).into(),
                filter: serde_json::Map::new(),
            }),
    );
    triggers
}

fn program() -> Shortcut {
    let param =
        |name: &'static str, ty, about: &'static str| ActionParam::required(name, ty, about);
    Shortcut {
        id: "discord-bridge".into(),
        name: "Discord Bridge".into(),
        inputs: vec![
            param("event", ParamType::Object, "What fired"),
            ActionParam::optional(
                "context",
                ParamType::Object,
                "Where avatars and message links point: avatars, link",
            ),
            // The host binds it on each firing, so a Discord-side delete is
            // judged by the deleter's role in Tilo now.
            ActionParam::optional(
                "workspace",
                ParamType::Object,
                "The workspace's members with their roles: members [{person, role}]",
            ),
            // The host binds it on each firing from the workspace's own
            // emoji, keeping each picture where the worker can upload it.
            ActionParam::optional(
                "emoji",
                ParamType::Object,
                "The workspace's emoji: emoji [{name, picture}]",
            ),
        ],
        steps: vec![
            Step::new(
                "start",
                StepKind::SetVar {
                    name: "more".into(),
                    value: Binding::Literal(json!(true)),
                },
            ),
            Step::new(
                "passes",
                StepKind::ForEach {
                    items: Binding::Literal(json!((1..=PASSES).collect::<Vec<_>>())),
                    item: "pass".into(),
                    body: vec![Step::new(
                        "pass",
                        StepKind::If {
                            cond: Condition::Truthy(Binding::Var("more".into())),
                            then: pass(),
                            otherwise: Vec::new(),
                        },
                    )],
                },
            ),
        ],
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: triggers(),
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

/// tl Chat's record with a category, a text channel and a message, a thread and its reply, and a
/// voice channel: every kind of resource its actions are published on.
fn chat_record() -> Opened {
    let packages = tilo_workspace_service::tl_records::built_in(CHAT_APP)
        .unwrap()
        .expect("Chat is built into this build");
    let mut opened = Opened::open(
        ResourceStore::in_memory(),
        &packages,
        Actor::App(tilo_workspace_service::tl_records::runtime::ids::AppId::from(CHAT_APP)),
        None,
    )
    .expect("Chat opens on an empty record");
    let root = opened.open_root().expect("Chat has its root");
    let who = Actor::Member(MemberId::person("ada"));
    let call = |opened: &mut Opened, action: &str, args: Vec<TlValue>| {
        opened
            .invoke(&who, (CHAT_APP, action), args, None)
            .unwrap_or_else(|error| panic!("Chat's {action} runs: {error}"))
    };
    let chats = TlValue::resource(&root);
    let general = call(
        &mut opened,
        "new_channel",
        vec![
            chats.clone(),
            TlValue::Text("general".into()),
            TlValue::Text(String::new()),
        ],
    );
    let general = match general {
        TlValue::Ref(reference) => (*reference.target).clone(),
        other => panic!("new_channel made a channel, not {other:?}"),
    };
    call(
        &mut opened,
        "new_category",
        vec![chats.clone(), TlValue::Text("talk".into())],
    );
    let message = call(
        &mut opened,
        "send",
        vec![
            TlValue::resource(&general),
            TlValue::Rich(
                tilo_workspace_service::tl_records::runtime::rich::RichValue::from_text("hello"),
            ),
            TlValue::Opt(None),
        ],
    );
    let message = match message {
        TlValue::Ref(reference) => (*reference.target).clone(),
        other => panic!("send made a message, not {other:?}"),
    };
    call(
        &mut opened,
        "start_thread",
        vec![
            TlValue::resource(&general),
            TlValue::resource(&message),
            TlValue::Text("thread".into()),
        ],
    );
    call(
        &mut opened,
        "new_voice_channel",
        vec![chats, TlValue::Text("voice".into())],
    );
    opened
}

/// Every action a connected guild publishes and tl Chat's own.
fn catalog() -> tilo_automation::ActionCatalog {
    let chat = chat_record();
    let mut guild = TestApp::<Discord>::new().unwrap();
    guild
        .invoke(Guild::CONNECT.with(discord::guild::guild::Connect {
            guild_id: "1".into(),
            name: "Tilo".into(),
            only_channel: None,
            mirror_channels: None,
            map_roles: None,
        }))
        .unwrap();
    let mut rows: Vec<CatalogueRow> = chat.rows();
    rows.extend(
        guild
            .actions()
            .unwrap()
            .into_iter()
            .map(|action| CatalogueRow::for_action(format!("{APP_ID}#1"), APP_ID, action)),
    );
    tilo_automation::ActionCatalog::new(rows)
}

fn file() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bridge/program.json")
}

#[test]
fn the_bridge_program_checks_against_chat_and_discord_and_is_current() {
    let catalog = catalog();
    let mut shortcut = program();
    shortcut.grant = tilo_automation::grant::derived_rules(&shortcut, &catalog);
    if let Err(errors) = tilo_automation::check_bot(&shortcut, &catalog) {
        panic!("the bridge program does not check: {errors:#?}");
    }
    let bytes = tilo_automation::program::canonical_bytes(&shortcut).unwrap();
    assert_eq!(
        tilo_automation::program::parse_program(&bytes).unwrap(),
        shortcut
    );
    if std::env::var_os("BLESS").is_some() {
        std::fs::create_dir_all(file().parent().unwrap()).unwrap();
        std::fs::write(file(), &bytes).unwrap();
    }
    let stored = std::fs::read(file()).unwrap_or_default();
    assert!(
        stored == bytes,
        "bridge/program.json is stale; run this test with BLESS=1, or write the line\nPROGRAM {}",
        String::from_utf8_lossy(&bytes)
    );
}

#[test]
fn the_program_names_only_the_discord_connector_and_chat() {
    let requirements = tilo_automation::requirements::requirements(&program());
    assert_eq!(
        requirements.connectors.into_iter().collect::<Vec<_>>(),
        [APP_ID]
    );
    assert!(requirements.apps.contains(CHAT_APP));
    let _: Value = serde_json::to_value(program()).unwrap();
}

/// A Moderator's `block-persona` fires the bridge, and its plan removes from
/// Chat every message mirrored from that Discord author, and only those.
#[test]
fn blocking_a_persona_removes_what_was_mirrored_of_them() {
    use discord::guild::guild;
    use discord::{MappedKind, Origin};
    use tilo_automation::dispatch::{Armed, Event, Firer, dispatch, event_of};
    use ui::tilo_resource::Replica;

    let mut ada = Replica::<Guild>::new().as_author("u_ada");
    ada.apply(Guild::CONNECT.with(guild::Connect {
        guild_id: "1".into(),
        name: "Tilo".into(),
        only_channel: None,
        mirror_channels: None,
        map_roles: None,
    }))
    .unwrap();
    let mut bot = ada.fork().as_bot_for("b1", "u_ada");
    for (tilo, discord, persona) in [
        ("m1", "dm1", "d_spam"),
        ("m2", "dm2", "d_spam"),
        ("m3", "dm3", "d_kind"),
    ] {
        bot.apply(Guild::MAP.with(guild::Map {
            kind: MappedKind::Message,
            tilo: tilo.into(),
            discord: discord.into(),
            origin: Origin::Discord,
            persona: Some(persona.into()),
            root: None,
        }))
        .unwrap();
    }
    let mut ada = bot.fork().as_author("u_ada");
    ada.apply(Guild::BLOCK_PERSONA.with(guild::BlockPersona {
        discord_user: "d_spam".into(),
    }))
    .unwrap();

    let version = ui::tilo_resource::history::versions(&mut ada.doc().clone())
        .pop()
        .unwrap();
    let change = version.hash.to_string();
    let record = event_of("w", "guild", KIND, &change, &version.context.unwrap());
    let shortcut = program();
    let armed = [Armed {
        firer: Firer {
            shortcut: "b1",
            saver: "u_ada",
        },
        shortcut: &shortcut,
    }];
    let fired = dispatch(&armed, Event::Record(&record));
    let [firing] = fired.firings.as_slice() else {
        panic!("the block fires the bridge once: {fired:?}");
    };

    let state = ada.state().unwrap();
    let plan = discord::bridge::plan(
        discord::bridge::Reading {
            guild: &state,
            chat: &discord::tilo::ChatRead::default(),
            context: &discord::outbound::Context::default(),
            workspace: &Value::Null,
            emoji: None,
        },
        &firing.inputs["event"],
    );
    let messages: Vec<Value> = plan
        .chat_deletes
        .iter()
        .map(|item| item["message"].clone())
        .collect();
    assert_eq!(messages, [json!("m1"), json!("m2")]);
}
