#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

//! The Discord bridge bot's program (discord-bridge.md §3), built here and
//! kept as the canonical file `bridge/program.json` that the bot directory
//! publishes. The test checks it against the real Chat and Discord action
//! surfaces, stores the grant its steps derive, and fails when the file is
//! stale; `BLESS=1` rewrites it.
//!
//! The program is a fixed run of loops: `plan-route` names the Chat message
//! the event is about, then up to six passes each read the room's chat, ask
//! `plan-event` what to do and loop over its lists. A pass runs only while
//! the previous plan asked for another, so a reply waits for its thread and
//! a post for its author's avatar.

use std::collections::BTreeMap;

use chat::resource::ChatShell;
use discord::resource::Discord;
use discord::{APP_ID, Guild, KIND};
use serde_json::{Value, json};
use tilo_automation::grant::{EDIT_PARAM, FIELD_EDIT};
use tilo_automation::{
    ActorFilter, Binding, Condition, Selector, Shortcut, Step, StepKind, TriggerSpec,
};
use ui::framework::testing::TestApp;
use ui::tilo_actions::ActionKey;
use ui::tilo_actions::params::{ActionParam, ParamType};
use ui::tilo_actions::remote::CatalogueRow;

const CHAT_APP: &str = "so.tilo.chat";
const CHAT_KIND: &str = "chat.workspace";
const MESSAGES_KIND: &str = "chat.messages";
const PASSES: usize = 6;

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

fn invoke(
    id: &str,
    key: ActionKey,
    resource: Selector,
    args: &[(&str, Binding)],
    appearance: Option<Binding>,
) -> Step {
    Step::new(
        id,
        StepKind::Invoke {
            action: key,
            resource,
            args: args
                .iter()
                .map(|(name, binding)| ((*name).to_owned(), binding.clone()))
                .collect::<BTreeMap<_, _>>(),
            appearance,
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
        None,
    )
}

/// A Chat verb on the bridged Chat record, naming its item by id.
fn chat(id: &str, action: &str, args: &[(&str, Binding)], shown: bool) -> Step {
    invoke(
        id,
        ActionKey::new(CHAT_APP, CHAT_KIND, action),
        Selector::FirstOfKind(CHAT_KIND.into()),
        args,
        shown.then(|| item("appearance")),
    )
}

/// A Chat verb on the chat's messages, which the chat keeps in a chain of
/// parts beside its record and publishes as one resource, naming its
/// message by id.
fn message(id: &str, action: &str, args: &[(&str, Binding)], shown: bool) -> Step {
    invoke(
        id,
        ActionKey::new(CHAT_APP, MESSAGES_KIND, action),
        Selector::FirstOfKind(MESSAGES_KIND.into()),
        args,
        shown.then(|| item("appearance")),
    )
}

/// The framework's `op` field edit (`react` or `unreact`) of the reactions
/// of the loop item's message, with its emoji, as the Discord member it
/// shows. The path is spelled out so every check decides the step where the
/// room does, at the message's reactions.
fn reaction(id: &str, op: &str) -> Step {
    let step = |kind: &str, binding: Binding| {
        Binding::Object(BTreeMap::from([(kind.to_owned(), binding)]))
    };
    let edit = Binding::Object(BTreeMap::from([
        (
            "path".to_owned(),
            Binding::List(vec![
                step("field", text("messages")),
                step("item", item("message")),
                step("field", text("reactions")),
            ]),
        ),
        ("op".to_owned(), text(op)),
        ("emoji".to_owned(), item("emoji")),
    ]));
    message(id, FIELD_EDIT, &[(EDIT_PARAM, edit)], true)
}

/// The id of the item step `step` created, as text.
fn created(step: &str) -> Binding {
    Binding::Template(format!("{{{{step.{step}}}}}"))
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

/// Mirror Discord messages into Chat: post with the author's appearance,
/// then map the message the post created to its Discord counterpart.
fn mirrored_post(step: &str, extra: &[(&str, Binding)]) -> Vec<Step> {
    let mut args = vec![
        ("channel", item("channel")),
        ("text", item("text")),
        ("parent", item("parent")),
        ("written", item("written")),
    ];
    args.extend(extra.iter().cloned());
    let posted = format!("{step}-post");
    let mut mapped = map(
        &format!("{step}-map"),
        "message",
        created(&posted),
        item("discord"),
        "discord",
    );
    if let StepKind::Invoke { args, .. } = &mut mapped.kind {
        args.insert("persona".into(), item("persona"));
        args.insert("root".into(), item("parent"));
    }
    vec![message(&posted, "post", &args, true), mapped]
}

#[allow(
    clippy::too_many_lines,
    reason = "one loop per planned verb, read top to bottom"
)]
fn pass() -> Vec<Step> {
    let plan_step = |id: &str, pointer: &str| Binding::step(id, pointer);
    vec![
        invoke(
            "read",
            ActionKey::new(CHAT_APP, CHAT_KIND, "read-room"),
            Selector::FirstOfKind(CHAT_KIND.into()),
            &[
                ("message", plan_step("route", "/message")),
                ("channel", plan_step("route", "/channel")),
            ],
            None,
        ),
        guild(
            "plan",
            "plan-event",
            &[
                ("event", input("event")),
                ("chat", plan_step("read", "")),
                ("context", input("context")),
                ("room", input("room")),
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
            vec![guild("send", "send", &[("op", item("op"))]), mapped]
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
        each("posts", mirrored_post("in", &[])),
        each("files", {
            let mut body = vec![guild("fetch-file", "fetch-asset", &[("op", item("fetch"))])];
            body.extend(mirrored_post(
                "file",
                &[("attachment", plan_step("fetch-file", "/attachment"))],
            ));
            body
        }),
        each(
            "chat_edits",
            vec![message(
                "edit-message",
                "edit-message",
                &[("message", item("message")), ("text", item("text"))],
                false,
            )],
        ),
        each(
            "chat_deletes",
            vec![
                message(
                    "delete-message",
                    "delete-message",
                    &[("message", item("message"))],
                    false,
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
                message(
                    "remove-message",
                    "remove-message",
                    &[("message", item("message"))],
                    false,
                ),
                guild(
                    "unmap-removed",
                    "unmap",
                    &[("kind", text("message")), ("tilo", item("message"))],
                ),
            ],
        ),
        each("chat_reacts", vec![reaction("chat-react", "react")]),
        each("chat_unreacts", vec![reaction("chat-unreact", "unreact")]),
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
        each("import_categories", {
            let mut body = vec![chat(
                "create-category",
                "create-category",
                &[("name", item("name"))],
                false,
            )];
            body.push(map(
                "map-category",
                "category",
                created("create-category"),
                item("discord"),
                "discord",
            ));
            body
        }),
        each(
            "import_channels",
            vec![
                chat(
                    "create-channel",
                    "create-channel",
                    &[
                        ("name", item("name")),
                        ("category", item("category")),
                        ("kind", item("kind")),
                    ],
                    false,
                ),
                map(
                    "map-imported",
                    "channel",
                    created("create-channel"),
                    item("discord"),
                    "discord",
                ),
            ],
        ),
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
            vec![chat(
                "file-channel",
                "set-channel-category",
                &[("channel", item("channel")), ("category", item("category"))],
                false,
            )],
        ),
        each(
            "drop_channels",
            vec![chat(
                "drop-channel",
                "delete-channel",
                &[("channel", item("channel"))],
                false,
            )],
        ),
        each(
            "drop_categories",
            vec![chat(
                "drop-category",
                "delete-category",
                &[("category", item("category"))],
                false,
            )],
        ),
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

fn triggers() -> Vec<TriggerSpec> {
    let mut triggers = vec![
        // Every Chat change by anyone but the bridge itself, which the
        // dispatcher never fires on its own output: to the chat record
        // (channels, people) and to its messages.
        TriggerSpec::RecordChanged {
            kind: CHAT_KIND.into(),
            actions: Vec::new(),
            fields: Vec::new(),
            by: ActorFilter::Anyone,
        },
        TriggerSpec::RecordChanged {
            kind: MESSAGES_KIND.into(),
            actions: Vec::new(),
            fields: Vec::new(),
            by: ActorFilter::Anyone,
        },
        TriggerSpec::RecordChanged {
            kind: "tilo.room".into(),
            actions: ["add-member", "set-role", "remove-member", "ban", "unban"]
                .map(str::to_owned)
                .to_vec(),
            fields: Vec::new(),
            by: ActorFilter::Anyone,
        },
        // The workspace's emoji, which the bridge uploads to the server.
        TriggerSpec::RecordChanged {
            kind: discord::bridge::EMOJI_KIND.into(),
            actions: Vec::new(),
            fields: Vec::new(),
            by: ActorFilter::Anyone,
        },
        TriggerSpec::RecordChanged {
            kind: KIND.into(),
            actions: vec!["block-persona".to_owned(), "complete-link".to_owned()],
            fields: Vec::new(),
            by: ActorFilter::Anyone,
        },
    ];
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
        step_timeout_secs: None,
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
                "room",
                ParamType::Object,
                "The room's members with their roles: members [{person, role}]",
            ),
            // The host binds it on each firing from the room's workspace
            // emoji, keeping each picture where the worker can upload it.
            ActionParam::optional(
                "emoji",
                ParamType::Object,
                "The workspace's emoji: emoji [{name, picture}]",
            ),
        ],
        steps: vec![
            guild("route", "plan-route", &[("event", input("event"))]),
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
    }
}

/// Every action a real Chat record (with a channel and a message) and a
/// connected guild publish.
fn catalog() -> tilo_automation::ActionCatalog {
    let mut chat = TestApp::<ChatShell>::new().unwrap();
    chat.call(
        "begin-chat",
        json!({"name": "Ada", "initials": "A", "color": 1}),
    )
    .unwrap();
    let state = chat.state();
    let (channel, _) = state.channels.iter().next().unwrap();
    chat.call_on(
        &ui::framework::chain_resource_id(&chat::parts::chain()),
        "post",
        json!({"channel": channel.as_str(), "text": "hello"}),
    )
    .unwrap();
    let mut guild = TestApp::<Discord>::new().unwrap();
    guild
        .invoke(Guild::CONNECT.with(discord::record::guild::Connect {
            guild_id: "1".into(),
            name: "Tilo".into(),
            only_channel: None,
            mirror_channels: None,
            map_roles: None,
        }))
        .unwrap();
    let mut rows: Vec<CatalogueRow> = Vec::new();
    for (app_id, actions) in [
        (CHAT_APP, chat.actions().unwrap()),
        (APP_ID, guild.actions().unwrap()),
    ] {
        rows.extend(
            actions
                .into_iter()
                .map(|action| CatalogueRow::for_action(format!("{app_id}#1"), app_id, action)),
        );
    }
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

/// The bridge reacts for Discord people through the field edit of Chat's
/// messages, and no other record of Chat's: the messages' other fields are
/// read-only, so a reaction is all that field edit reaches.
#[test]
fn the_bridge_field_edits_only_chats_messages() {
    let shortcut = program();
    let grant = tilo_automation::grant::derived_rules(&shortcut, &catalog());
    let field_edits: Vec<String> = grant
        .iter()
        .map(|rule| rule.node.to_string())
        .filter(|node| node.contains("app:so.tilo.chat") && node.ends_with(FIELD_EDIT))
        .collect();
    assert_eq!(
        field_edits,
        ["room:*/app:so.tilo.chat/kind:chat.messages/resource:*/**/action:field-edit"]
    );
    // Where the bot's own door offers and admits the field edit (the
    // resource), and where the room decides a reaction (its field).
    let messages = ui::permissions::TreePath::room("room")
        .app(CHAT_APP)
        .kind(MESSAGES_KIND)
        .resource("part-0");
    let decided = |node: &ui::permissions::TreePath| {
        tilo_automation::grant_answer(&grant, node, ui::permissions::ActionKind::Undoable).answer
    };
    assert_eq!(
        decided(&messages.clone().action(FIELD_EDIT)),
        ui::permissions::Answer::Allow
    );
    assert_eq!(
        decided(&messages.item("m1").field("reactions").action(FIELD_EDIT)),
        ui::permissions::Answer::Allow
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
    assert!(
        requirements.appearance,
        "the bridge posts under Discord authors' names"
    );
    let _: Value = serde_json::to_value(program()).unwrap();
}

/// A Moderator's `block-persona` fires the bridge, and its plan removes from
/// Chat every message mirrored from that Discord author, and only those.
#[test]
fn blocking_a_persona_removes_what_was_mirrored_of_them() {
    use discord::record::guild;
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
    let record = event_of("room", "guild", KIND, &change, &version.context.unwrap());
    let shortcut = program();
    let armed = [Armed {
        firer: Firer::Bot { installation: "b1" },
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
            chat: &json!({}),
            context: &discord::outbound::Context::default(),
            room: &Value::Null,
            emoji: None,
        },
        &firing.inputs["event"],
    );
    assert_eq!(
        plan.chat_deletes,
        [json!({"message": "m1"}), json!({"message": "m2"})]
    );
}
