//! The Discord app of old and the app of today: one bridging the server the native app bridged,
//! with its settings, its joinings, the accounts people linked and the blocked accounts kept, as
//! one change, once on this device and never twice on another, and the native app's documents
//! left as they were.

use crate::fixtures::{Replica, Workspace, native_documents};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, Value};

const DISCORD: &str = include_str!("../../../../apps/tl/discord/app.tl");
const DISCORD_IDS: &str = include_str!("../../../../apps/tl/discord/app.ids");
const APP: &str = "so.tilo.discord";

/// What the native Discord kept of the Lounge's server, as its own model wrote it.
fn documents() -> Vec<(String, automerge::AutoCommit)> {
    native_documents(include_str!("legacy/rust_discord.hex"))
}

/// The app of today under the id the old app kept: the document the server it bridged lives in,
/// which the preview id keeps apart until the app takes its id back.
fn source() -> String {
    DISCORD.replacen("app so.tilo.discord.preview", "app so.tilo.discord", 1)
}

/// The app of today on a workspace of its own, handed what the native app kept as a device start
/// hands a native app's documents to the package that carries them over.
fn opened(documents: &[(String, automerge::AutoCommit)]) -> Replica {
    let source = source();
    let (replica, _) = Replica::new("ada", Workspace::new(), &[(source.as_str(), DISCORD_IDS)]);
    replica.host.runtime().set_earlier(APP, documents);
    replica
}

/// The heads of each of the native app's documents: what says they were not written.
fn heads(documents: &[(String, automerge::AutoCommit)]) -> Vec<Vec<automerge::ChangeHash>> {
    documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect()
}

/// The field `name` of `at` as the app declares it.
fn read(host: &tilolang_tilo::host::Host, at: &ResourceRef, name: &str) -> Value {
    let package = host.package(APP).expect("the Discord package");
    let declared = package
        .meta
        .types
        .iter()
        .find(|t| t.id == at.ty)
        .expect("the type");
    let field = declared
        .fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("the type declares {name}"));
    host.runtime().read(at, &field.id).expect("the field")
}

/// The items of the collection `name` of `at`.
fn items(host: &tilolang_tilo::host::Host, at: &ResourceRef, name: &str) -> Vec<ResourceRef> {
    match read(host, at, name) {
        Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Ref(reference) => Some((*reference.target).clone()),
                _ => None,
            })
            .collect(),
        other => panic!("{name} is a collection: {other:?}"),
    }
}

fn text(host: &tilolang_tilo::host::Host, at: &ResourceRef, name: &str) -> String {
    match read(host, at, name) {
        Value::Text(text) => text,
        other => panic!("{name} is text: {other:?}"),
    }
}

/// The name of the case `at`'s field `field` holds, as the app declares it.
fn variant(host: &tilolang_tilo::host::Host, at: &ResourceRef, field: &str) -> String {
    let Value::Enum(value) = read(host, at, field) else {
        panic!("{field} is an enum");
    };
    host.package(APP)
        .expect("the Discord package")
        .meta
        .enums
        .iter()
        .find(|declared| declared.id == value.ty)
        .and_then(|declared| {
            declared
                .variants
                .iter()
                .find(|held| held.id == value.variant)
        })
        .map_or_else(
            || panic!("{field} holds a case the app declares"),
            |held| held.name.clone(),
        )
}

/// The account the app of today bridges after the carry.
fn account(replica: &Replica) -> ResourceRef {
    let root = replica.host.root_of(APP).expect("the root");
    let accounts = items(&replica.host, &root, "accounts");
    assert_eq!(accounts.len(), 1, "one bridged server");
    accounts[0].clone()
}

/// The joinings the carry wrote, sorted: each entry's kind and the Tilo and Discord names it
/// joins.
fn joinings(replica: &Replica, held: &ResourceRef) -> Vec<(String, String, String)> {
    let mut found: Vec<(String, String, String)> = items(&replica.host, held, "map")
        .iter()
        .map(|entry| {
            (
                variant(&replica.host, entry, "kind"),
                text(&replica.host, entry, "tilo"),
                text(&replica.host, entry, "discord"),
            )
        })
        .collect();
    found.sort();
    found
}

#[test]
fn the_lounges_server_is_carried_whole_and_a_second_carry_adds_nothing() {
    let documents = documents();
    let before = heads(&documents);
    let replica = opened(&documents);
    let root = replica.host.root_of(APP).expect("the root");
    assert!(
        items(&replica.host, &root, "accounts").is_empty(),
        "nothing is carried before the migration runs"
    );
    assert_eq!(replica.host.migrate(), 1, "the migration carries it");
    assert_eq!(replica.commit(), 1, "as one change");

    let held = account(&replica);
    assert_eq!(text(&replica.host, &held, "guild_id"), "123");
    assert_eq!(text(&replica.host, &held, "server"), "The Lounge");
    assert_eq!(text(&replica.host, &held, "only_channel"), "456");
    assert_eq!(text(&replica.host, &held, "bot_user"), "900");
    assert_eq!(
        text(&replica.host, &held, "token_item"),
        "",
        "this device's keeper holds no bot token, so the setup asks for it again"
    );
    assert_eq!(
        read(&replica.host, &held, "mirror_channels"),
        Value::Bool(true)
    );
    assert_eq!(read(&replica.host, &held, "map_roles"), Value::Bool(true));
    assert_eq!(
        variant(&replica.host, &held, "ownership"),
        "adopted",
        "the server was adopted, not still being imported"
    );

    let map = items(&replica.host, &held, "map");
    let found = joinings(&replica, &held);
    assert_eq!(
        found,
        vec![
            ("channel".into(), "tl:so.tilo.chat:c1".into(), "456".into()),
            ("channel".into(), "tl:so.tilo.chat:c2".into(), "789".into()),
            ("message".into(), "tl:so.tilo.chat:m1".into(), "777".into()),
            ("webhook".into(), "tl:so.tilo.chat:c1".into(), "555".into()),
        ],
        "the same joinings come out"
    );
    let message = map
        .iter()
        .find(|entry| text(&replica.host, entry, "discord") == "777")
        .expect("the mapped message");
    assert_eq!(variant(&replica.host, message, "origin"), "discord");
    assert_eq!(text(&replica.host, message, "persona"), "3001");

    let links = items(&replica.host, &held, "links");
    assert_eq!(links.len(), 1, "the linked account");
    let link = &links[0];
    assert_eq!(text(&replica.host, link, "discord_user"), "3001");
    assert_eq!(text(&replica.host, link, "person"), "ada");
    assert_eq!(text(&replica.host, link, "display_name"), "Ada");
    assert_eq!(
        text(&replica.host, link, "linked_at"),
        "2023-11-14T22:13:20Z"
    );

    let channels = items(&replica.host, &held, "channels");
    assert_eq!(channels.len(), 2, "the channels it observed");
    let voice = channels
        .iter()
        .find(|channel| text(&replica.host, channel, "id") == "789")
        .expect("the voice channel");
    assert_eq!(
        variant(&replica.host, voice, "kind"),
        "voice",
        "a voice channel stays one: the pair is one call"
    );

    let blocked = match read(&replica.host, &held, "blocked_personas") {
        Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Text(user) => Some(user),
                _ => None,
            })
            .collect::<Vec<_>>(),
        other => panic!("blocked accounts: {other:?}"),
    };
    assert_eq!(blocked, vec!["3002".to_owned()]);

    // It runs once, and the native app's documents are left as they were.
    assert_eq!(replica.host.migrate(), 0, "it runs once");
    assert_eq!(replica.commit(), 0, "and changes nothing again");
    assert_eq!(heads(&documents), before, "the native documents stand");
    assert_eq!(
        account(&replica).id_string(),
        held.id_string(),
        "one account"
    );
    assert_eq!(items(&replica.host, &held, "map").len(), 4);
    assert_eq!(items(&replica.host, &held, "links").len(), 1);
    assert_eq!(items(&replica.host, &held, "channels").len(), 2);
}

/// The words the app's window shows: every text any node of it carries.
fn shown(replica: &Replica) -> Vec<String> {
    let view = replica.view(APP);
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_carried_account_asks_for_the_bot_token_again() {
    let documents = documents();
    let replica = opened(&documents);
    assert_eq!(replica.host.migrate(), 1, "the migration carries it");
    replica.host.runtime().set_members(
        vec![tilolang_runtime::members::Member::person(
            "ada",
            tilo_actions::policy::Role::Moderator,
        )],
        None,
    );
    let held = account(&replica);
    // The keeper held no token this device could give the vault, so the account carries none and
    // the person opens the setup to add one.
    assert_eq!(text(&replica.host, &held, "token_item"), "");
    let package = replica.host.package(APP).expect("the Discord package");
    let open = package
        .meta
        .actions
        .iter()
        .find(|action| action.name == "open_setup")
        .expect("the setup opens")
        .id
        .clone();
    match replica.host.runtime().invoke_action(
        &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        (open.as_str(), vec![Value::resource(&held)]),
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("ada opens the setup: {other:?}"),
    }
    let words = shown(&replica);
    assert!(
        words.iter().any(|word| word == "Add the bot's token"),
        "the setup asks for the token again: {words:?}"
    );
}

#[test]
fn a_second_device_makes_no_duplicates() {
    let documents = documents();
    let ada = opened(&documents);
    assert_eq!(ada.host.migrate(), 1, "the migration carries it");
    let held = account(&ada);

    let source = source();
    let grace = ada.fork("grace", &[(source.as_str(), DISCORD_IDS)]);
    grace.host.runtime().set_earlier(APP, &documents);
    assert_eq!(
        grace.host.migrate(),
        0,
        "another device with the same documents finds it done"
    );
    assert_eq!(grace.commit(), 0);
    assert_eq!(account(&grace).id_string(), held.id_string(), "one account");
    assert_eq!(items(&grace.host, &held, "channels").len(), 2);
    assert_eq!(items(&grace.host, &held, "map").len(), 4);
    assert_eq!(items(&grace.host, &held, "links").len(), 1);
}

#[test]
fn a_device_opened_before_the_native_documents_arrive_carries_them_once_they_do() {
    let source = source();
    let (replica, _) = Replica::new("ada", Workspace::new(), &[(source.as_str(), DISCORD_IDS)]);
    replica.host.root_of(APP).expect("the root");
    replica.commit();
    assert!(
        !replica.host.runtime().has_earlier(APP),
        "nothing has arrived yet"
    );
    assert_eq!(replica.host.migrate(), 0, "the carrying waits");
    assert_eq!(replica.commit(), 0, "and records nothing");

    replica.host.runtime().set_earlier(APP, &documents());
    assert_eq!(replica.host.migrate(), 1, "the carrying ran once they did");
    assert_eq!(replica.commit(), 1, "as one change");
    assert_eq!(text(&replica.host, &account(&replica), "guild_id"), "123");
    assert_eq!(replica.host.migrate(), 0, "and runs only once");
}
/// The bridge guard admits exactly the app, never another whose id merely starts the same: an
/// app process that is not this one cannot report the server's layout.
#[test]
fn only_the_discord_apps_own_process_carries_the_bridge() {
    use tilolang_runtime::ids::AppId;

    let (ada, _) = Replica::new("ada", Workspace::new(), &[(DISCORD, DISCORD_IDS)]);
    ada.host.runtime().set_members(
        vec![tilolang_runtime::members::Member::person(
            "ada",
            tilo_actions::policy::Role::Owner,
        )],
        None,
    );
    let root = ada.host.root_of("so.tilo.discord.preview").unwrap();
    let package = ada.host.package("so.tilo.discord.preview").unwrap();
    let action = |name: &str| {
        package
            .meta
            .actions
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("no action {name}"))
            .id
            .clone()
    };
    let begin = action("begin_setup");
    let discord = match ada
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (begin.as_str(), vec![Value::resource(&root)]),
        )
        .unwrap()
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("the owner starts the account: {other:?}"),
    };
    ada.commit();

    let layout = action("observe_layout");
    let layout_args = || {
        vec![
            discord.clone(),
            Value::text("1"),
            Value::List(Vec::new()),
            Value::List(Vec::new()),
            Value::List(Vec::new()),
        ]
    };
    let own = ada.host.runtime().invoke_action(
        &Actor::App(AppId::from("so.tilo.discord.preview")),
        (layout.as_str(), layout_args()),
    );
    assert!(matches!(own, Ok(InvokeOutcome::Done(_))), "{own:?}");
    for other in ["so.tilo.discord.evil", "so.tilo.discordx", "so.tilo.chat"] {
        let refused = ada.host.runtime().invoke_action(
            &Actor::App(AppId::from(other)),
            (layout.as_str(), layout_args()),
        );
        assert!(
            !matches!(refused, Ok(InvokeOutcome::Done(_))),
            "{other} is not the bridge: {refused:?}"
        );
        assert!(
            format!("{refused:?}").contains("Only the bridge keeps the server's layout"),
            "{other}: with the reason: {refused:?}"
        );
    }
}
