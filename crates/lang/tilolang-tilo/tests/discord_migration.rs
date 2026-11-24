//! The Discord app of old and the app of today: one bridging the server the native app bridged,
//! with its joinings, the accounts people linked and the blocked accounts kept, and running the
//! carry again adds nothing.

use crate::fixtures::{Replica, Workspace};
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, Value};

const BEFORE: &str = include_str!("discord_before/app.tl");
const BEFORE_IDS: &str = include_str!("discord_before/app.ids");
const DISCORD: &str = include_str!("../../../../apps/tl/discord/app.tl");
const DISCORD_IDS: &str = include_str!("../../../../apps/tl/discord/app.ids");
const APP: &str = "so.tilo.discord";

/// The app of today under the id the old app kept: the document the server it bridged lives in,
/// which the preview id keeps apart until the app takes its id back.
fn source() -> String {
    DISCORD.replacen("app so.tilo.discord.preview", "app so.tilo.discord", 1)
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

/// The account the app of today bridges after the carry.
fn account(replica: &Replica) -> ResourceRef {
    let root = replica.host.root_of(APP).expect("the root");
    let accounts = items(&replica.host, &root, "accounts");
    assert_eq!(accounts.len(), 1, "one bridged server");
    accounts[0].clone()
}

fn carry(replica: &Replica) {
    let package = replica.host.package(APP).expect("the Discord package");
    let action = package
        .meta
        .actions
        .iter()
        .find(|a| a.name == "carry_native_discord")
        .expect("the carry")
        .id
        .clone();
    let root = replica.host.root_of(APP).expect("the root");
    let outcome = replica.host.runtime().invoke_action(
        &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        (action.as_str(), vec![Value::resource(&root)]),
    );
    assert!(
        matches!(outcome, Ok(InvokeOutcome::Done(_))),
        "the carry runs: {outcome:?}"
    );
    replica.commit();
}

#[test]
fn the_lounges_server_is_carried_whole_and_a_second_carry_adds_nothing() {
    // The workspace as the native app left it.
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(BEFORE, BEFORE_IDS)]);
    let before = ada.host.package(APP).expect("the old package").meta;
    let scenario = before
        .scenarios
        .iter()
        .find(|s| s.name == "the Lounge's server")
        .expect("the old server")
        .action
        .clone();
    ada.host.runtime().set_members(
        vec![tilolang_runtime::members::Member::person(
            "ada",
            tilo_actions::policy::Role::Owner,
        )],
        None,
    );
    let root = ada.host.root_of(APP).expect("the old root");
    ada.commit();
    match ada.host.runtime().invoke_action(
        &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        (scenario.as_str(), vec![]),
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("the old server is staged: {other:?}"),
    }
    let _ = root;
    ada.commit();

    // The same workspace, opened by the app of today.
    let source = source();
    let (later, _) = Replica::new("ada", ada.doc.fork(), &[(source.as_str(), DISCORD_IDS)]);
    let _ = later.view(APP);
    let root = later.host.root_of(APP).expect("the new root");
    // A root the app of today made is migrated by its own record; the server the app of old kept
    // is carried by the migration action, run here as the app does on its first open.
    carry(&later);

    let held = account(&later);
    assert_eq!(text(&later.host, &held, "guild_id"), "123");
    assert_eq!(text(&later.host, &held, "server"), "The Lounge");
    assert_eq!(text(&later.host, &held, "only_channel"), "456");
    assert_eq!(text(&later.host, &held, "bot_user"), "900");

    let map = items(&later.host, &held, "map");
    let mut found: Vec<(String, String, String)> = map
        .iter()
        .map(|entry| {
            let kind = match read(&later.host, entry, "kind") {
                Value::Enum(value) => value.variant,
                other => panic!("a kind: {other:?}"),
            };
            (
                kind,
                text(&later.host, entry, "tilo"),
                text(&later.host, entry, "discord"),
            )
        })
        .collect();
    found.sort();
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

    let links = items(&later.host, &held, "links");
    assert_eq!(links.len(), 1, "the linked account");
    let link = &links[0];
    assert_eq!(text(&later.host, link, "discord_user"), "3001");
    assert_eq!(text(&later.host, link, "person"), "ada");
    assert_eq!(text(&later.host, link, "display_name"), "Ada");
    assert_eq!(text(&later.host, link, "linked_at"), "2023-11-14T22:13:20Z");

    let channels = items(&later.host, &held, "channels");
    assert_eq!(channels.len(), 2, "the channels it observed");
    let voice = channels
        .iter()
        .find(|channel| text(&later.host, channel, "id") == "789")
        .expect("the voice channel");
    assert_eq!(
        match read(&later.host, voice, "kind") {
            Value::Enum(value) => value.variant,
            other => panic!("a kind: {other:?}"),
        },
        "voice",
        "a voice channel stays one: the pair is one call"
    );

    let blocked = match read(&later.host, &held, "blocked_personas") {
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

    // Run it again: nothing is added twice and nothing changes.
    carry(&later);
    assert_eq!(account(&later).id_string(), held.id_string(), "one account still");
    assert_eq!(items(&later.host, &held, "map").len(), 4);
    assert_eq!(items(&later.host, &held, "links").len(), 1);
    assert_eq!(items(&later.host, &held, "channels").len(), 2);
    assert_eq!(text(&later.host, &held, "guild_id"), "123");
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
