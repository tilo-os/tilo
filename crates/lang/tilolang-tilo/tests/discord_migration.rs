//! The Discord app of old and the app of today: one bridging the server the native app bridged,
//! with its joinings, the accounts people linked and the blocked accounts kept, and running the
//! carry again adds nothing.

use crate::fixtures::{Replica, Workspace};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, Value};

const DISCORD: &str = include_str!("../../../../apps/tl/discord/app.tl");
const DISCORD_IDS: &str = include_str!("../../../../apps/tl/discord/app.ids");
const APP: &str = "so.tilo.discord";

/// The app of today under the id the old app kept: the document the server it bridged lives in,
/// which the preview id keeps apart until the app takes its id back.
fn source() -> String {
    DISCORD.replacen("app so.tilo.discord.preview", "app so.tilo.discord", 1)
}

/// The document the native app kept of the Lounge's server, in the platform's resource layout: the
/// server's settings at the document's root, and each of its joinings, the accounts people linked
/// and the channels it observed as an item of its own collection, each with its order key (`k`)
/// and fields (`v`). What a session hands a package that took over a native app's id.
fn native_server() -> Vec<(String, automerge::AutoCommit)> {
    use automerge::transaction::Transactable as _;
    use automerge::{ObjType, ROOT};

    /// Add an item with the order key `order` to `collection`, giving its fields to `fields`.
    fn item(
        doc: &mut automerge::AutoCommit,
        collection: &automerge::ObjId,
        id: &str,
        order: &str,
        fields: impl FnOnce(&mut automerge::AutoCommit, &automerge::ObjId),
    ) {
        let entry = doc.put_object(collection, id, ObjType::Map).unwrap();
        doc.put(&entry, "k", order).unwrap();
        let held = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        fields(doc, &held);
    }

    let mut doc = automerge::AutoCommit::new();
    doc.put(ROOT, "tilo:version", 4_i64).unwrap();
    doc.put(ROOT, "guild_id", "123").unwrap();
    doc.put(ROOT, "name", "The Lounge").unwrap();
    doc.put(ROOT, "only_channel", "456").unwrap();
    doc.put(ROOT, "mirror_channels", true).unwrap();
    doc.put(ROOT, "map_roles", true).unwrap();
    doc.put(ROOT, "ownership", "adopted").unwrap();
    doc.put(ROOT, "bot_user", "900").unwrap();
    doc.put(ROOT, "sign_in_page", "").unwrap();
    let blocked = doc
        .put_object(ROOT, "blocked_personas", ObjType::Map)
        .unwrap();
    doc.put(&blocked, "3002", true).unwrap();
    let map = doc.put_object(ROOT, "map", ObjType::Map).unwrap();
    for (id, order, kind, tilo, discord, origin, persona) in [
        ("m1", "a", "channel", "tl:so.tilo.chat:c1", "456", "tilo", ""),
        ("m2", "b", "channel", "tl:so.tilo.chat:c2", "789", "tilo", ""),
        ("m3", "c", "webhook", "tl:so.tilo.chat:c1", "555", "tilo", ""),
        (
            "m4",
            "d",
            "message",
            "tl:so.tilo.chat:m1",
            "777",
            "discord",
            "3001",
        ),
    ] {
        item(&mut doc, &map, id, order, |doc, held| {
            doc.put(held, "kind", kind).unwrap();
            doc.put(held, "tilo", tilo).unwrap();
            doc.put(held, "discord", discord).unwrap();
            doc.put(held, "origin", origin).unwrap();
            doc.put(held, "persona", persona).unwrap();
            doc.put(held, "root", "").unwrap();
            doc.put(held, "picture", "").unwrap();
        });
    }
    let links = doc.put_object(ROOT, "links", ObjType::Map).unwrap();
    item(&mut doc, &links, "l1", "a", |doc, held| {
        doc.put(held, "discord_user", "3001").unwrap();
        doc.put(held, "person", "ada").unwrap();
        doc.put(held, "linked_at", 1_700_000_000_000_i64).unwrap();
        doc.put(held, "user_name", "ada").unwrap();
        doc.put(held, "display_name", "Ada").unwrap();
        doc.put(held, "avatar", "abc").unwrap();
        doc.put(held, "interaction", "").unwrap();
        doc.put(held, "photo", "").unwrap();
        doc.put(held, "photo_avatar", "").unwrap();
    });
    let channels = doc.put_object(ROOT, "channels", ObjType::Map).unwrap();
    for (id, order, kind, name) in [
        ("456", "a", "text", "general"),
        ("789", "b", "voice", "Lounge"),
    ] {
        item(&mut doc, &channels, id, order, |doc, held| {
            doc.put(held, "id", id).unwrap();
            doc.put(held, "kind", kind).unwrap();
            doc.put(held, "name", name).unwrap();
            doc.put(held, "parent", "").unwrap();
            doc.put(held, "position", 0_i64).unwrap();
            doc.put(held, "topic", "").unwrap();
        });
    }
    vec![("the lounges server".to_owned(), doc)]
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
        .and_then(|declared| declared.variants.iter().find(|held| held.id == value.variant))
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
    // What the native app kept on this device: the server it bridged, its joinings, the accounts
    // people linked and the ones it blocks.
    let earlier = native_server();

    // The package of today on a workspace of its own, handed what the app of old kept, as a
    // start hands a native app's documents to the package that carries them over.
    let source = source();
    let (later, _) = Replica::new("ada", Workspace::new(), &[(source.as_str(), DISCORD_IDS)]);
    later.host.runtime().set_earlier(APP, &earlier);
    assert!(
        later.host.runtime().has_earlier(APP),
        "the earlier documents are held"
    );
    assert_eq!(later.host.migrate(), 1, "the migration runs the carry");
    later.commit();

    let held = account(&later);
    assert_eq!(text(&later.host, &held, "guild_id"), "123");
    assert_eq!(text(&later.host, &held, "server"), "The Lounge");
    assert_eq!(text(&later.host, &held, "only_channel"), "456");
    assert_eq!(text(&later.host, &held, "bot_user"), "900");
    assert_eq!(
        read(&later.host, &held, "mirror_channels"),
        Value::Bool(true)
    );
    assert_eq!(read(&later.host, &held, "map_roles"), Value::Bool(true));
    assert_eq!(
        variant(&later.host, &held, "ownership"),
        "adopted",
        "the server was adopted, not still being imported"
    );

    let map = items(&later.host, &held, "map");
    let mut found: Vec<(String, String, String)> = map
        .iter()
        .map(|entry| {
            (
                variant(&later.host, entry, "kind"),
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
        variant(&later.host, voice, "kind"),
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

    // Run it again: the migration already ran, and carrying by hand adds nothing twice.
    assert_eq!(later.host.migrate(), 0, "the migration runs once");
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
