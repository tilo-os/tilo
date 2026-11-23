//! The `YouTube` client (`apps/tl/tube`), as people and the player drive it: a video watched
//! without touching the playhead is remembered where it got to, a playback that fails says why
//! and can be asked for again, a playlist shows its own videos in its maker's order and plays
//! from the first, each member's watch history and playback preferences are their own, and the
//! watch queue plays in the order videos were added to it.

use tilo_action_policy::{
    Answer, Grant, Grantee, KindSet, MemberAccess, Role, TreePath, WorkspacePolicy,
};
use tilolang_runtime::store::Store as _;
use tilolang_runtime::{Actor, InvokeOutcome, Meta, ResourceRef, UiNode, Value};
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::{Replica, Workspace, field_id, nodes};

/// `name`'s access in the workspace: what the app's actions reach outside for is allowed, as an
/// install grants the effects an app declares, so these tests drive the app's own logic.
fn access(name: &str) -> MemberAccess {
    let mut policy = WorkspacePolicy::new("workspace", "ada");
    policy.members.insert("bo".to_owned(), Role::Editor);
    policy.grants.push(Grant::new(
        "the app's effects",
        Grantee::person(name),
        TreePath::workspace("workspace").app(TUBE).rest(),
        KindSet::OUTSIDE,
        Answer::Allow,
        Grantee::person("ada"),
    ));
    MemberAccess::person(policy, name)
}

/// A replica of `doc` for `name`, running the client with its effects allowed.
fn replica(name: &str, doc: Workspace) -> (Replica, Meta) {
    let (replica, packages) = Replica::access(name, doc, &[(SOURCE, IDS)], Some(access(name)));
    (replica, packages[0].meta.clone())
}

const TUBE: &str = "so.tilo.tube";
const SOURCE: &str = include_str!("../../../../apps/tl/tube/app.tl");
const IDS: &str = include_str!("../../../../apps/tl/tube/app.ids");

fn person(name: &str) -> Actor {
    Actor::from(MemberId::person(name))
}

fn call(replica: &Replica, meta: &Meta, action: &str, args: Vec<Value>) -> Value {
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match replica
        .host
        .runtime()
        .invoke_action(&person(&replica.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{action}: {other:?}"),
    }
}

fn seed(replica: &Replica, meta: &Meta, name: &str) {
    let scenario = meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == name)
        .unwrap_or_else(|| panic!("the scenario {name}"))
        .action
        .clone();
    replica
        .host
        .runtime()
        .invoke_action(&person(&replica.name), (scenario.as_str(), vec![]))
        .unwrap();
}

/// Every item of the collection `field` of `at`.
fn items(replica: &Replica, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match replica.read(at, field) {
        Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Ref(at) => Some((*at.target).clone()),
                _ => None,
            })
            .collect(),
        other => panic!("a collection: {other:?}"),
    }
}

fn text(value: Value) -> String {
    match value {
        Value::Text(text) => text,
        other => panic!("not a text: {other:?}"),
    }
}

/// The video keyed `key` the client holds.
fn video(replica: &Replica, meta: &Meta, root: &ResourceRef, key: &str) -> ResourceRef {
    let key_id = field_id(meta, "Video", "key");
    items(replica, root, &field_id(meta, "YouTube", "videos"))
        .into_iter()
        .find(|at| replica.read(at, &key_id) == Value::text(key))
        .unwrap_or_else(|| panic!("the video {key} is held"))
}

/// The first node of `kind` in `view`, after what the view drew since the last look.
fn drawn(view: &mut tilolang_runtime::ViewInstance, kind: &str) -> UiNode {
    view.poll_patches().unwrap();
    let node = view
        .tree()
        .first_of_kind(kind)
        .unwrap_or_else(|| panic!("a {kind} is drawn"));
    (*node).clone()
}

/// The key of the button `view` draws with `label`.
fn button(view: &mut tilolang_runtime::ViewInstance, label: &str) -> String {
    view.poll_patches().unwrap();
    nodes(view.tree())
        .into_iter()
        .find(|node| {
            node.kind == "ui.Button" && node.props.get("label") == Some(&Value::text(label))
        })
        .unwrap_or_else(|| panic!("a button saying {label}"))
        .key
        .clone()
}

/// The text `view` draws of the nodes of `kind`, by the prop it shows it in, in the order they
/// stand.
fn named(view: &mut tilolang_runtime::ViewInstance, kind: &str, prop: &str) -> Vec<String> {
    view.poll_patches().unwrap();
    nodes(view.tree())
        .into_iter()
        .filter(|node| node.kind == kind)
        .filter_map(|node| match node.props.get(prop) {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The rows of the window's list, each as it builds when the window asks for it: a long list
/// builds only the rows in view.
fn rows(view: &mut tilolang_runtime::ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let Some(list) = view.tree().first_of_kind("ui.VirtualList") else {
        return Vec::new();
    };
    let list = list.key.clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|at| view.row(&list, at).unwrap().cloned())
        .collect()
}

/// What each row of the window's list is called, in the order they stand.
fn row_titles(view: &mut tilolang_runtime::ViewInstance) -> Vec<String> {
    rows(view)
        .into_iter()
        .flat_map(|row| {
            nodes(&row)
                .into_iter()
                .filter(|node| node.kind == "ui.ListRow")
                .filter_map(|node| match node.props.get("title") {
                    Some(Value::Text(title)) => Some(title.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The client seeded from its first scenario, its root and the videos it holds.
struct Client {
    ada: Replica,
    meta: Meta,
    root: ResourceRef,
}

fn client() -> Client {
    let (ada, meta) = replica("ada", Workspace::new());
    let root = ada.host.root_of(TUBE).unwrap();
    seed(&ada, &meta, "a subscriptions feed");
    ada.commit();
    Client { ada, meta, root }
}

/// One watch entry of the client, by the video's key and whose it is.
fn history_entry(
    replica: &Replica,
    meta: &Meta,
    root: &ResourceRef,
    key: &str,
    whos: Option<&str>,
) -> Option<ResourceRef> {
    let key_id = field_id(meta, "WatchEntry", "key");
    let member_id = field_id(meta, "WatchEntry", "member");
    items(replica, root, &field_id(meta, "YouTube", "history"))
        .into_iter()
        .find(|at| {
            if replica.read(at, &key_id) != Value::text(key) {
                return false;
            }
            let member = replica.read(at, &member_id);
            match (whos, member) {
                (Some(who), Value::Opt(Some(held))) => *held == Value::Actor(person(who)),
                (None, Value::Opt(None)) => true,
                _ => false,
            }
        })
}

/// What a history entry says the person's place in the video was.
fn progress_of(replica: &Replica, meta: &Meta, entry: &ResourceRef) -> Value {
    replica.read(entry, &field_id(meta, "WatchEntry", "progress"))
}

/// The player reports where it got to while it played, and the video's history keeps it: a video
/// watched without touching the playhead is left where it was watched to.
#[test]
fn the_player_reporting_where_it_got_to_is_remembered_in_the_history() {
    let (ada, meta) = replica("ada", Workspace::new());
    let root = ada.host.root_of(TUBE).unwrap();
    seed(&ada, &meta, "a video playing");
    ada.commit();
    let magnet = video(&ada, &meta, &root, "v-magnet");
    let mut page = ada.host.open_resource(&magnet, None).unwrap();
    let player = drawn(&mut page, "ui.VideoPlayer").key;
    ada.commit();

    // The player says it got six tenths of the way in: the history entry the page's own person
    // watched moves there.
    page.fire_with(&player, "progress", Some(&Value::Float(0.6)))
        .unwrap();
    ada.commit();
    let entry = history_entry(&ada, &meta, &root, "v-magnet", Some("ada"))
        .expect("the entry the scenario seeded");
    assert_eq!(progress_of(&ada, &meta, &entry), Value::Float(0.6));

    // Played to its last seconds, the video is watched whole rather than left at the credits: a
    // resume starts it again.
    page.fire_with(&player, "progress", Some(&Value::Float(0.995)))
        .unwrap();
    ada.commit();
    let entry = history_entry(&ada, &meta, &root, "v-magnet", Some("ada"))
        .expect("the entry the scenario seeded");
    assert_eq!(progress_of(&ada, &meta, &entry), Value::Float(0.0));
}

/// A video the player cannot show says why, in its own words, and Try again asks the service for
/// the address again.
#[test]
fn a_playback_that_fails_says_why_and_try_again_asks_again() {
    let (ada, meta) = replica("ada", Workspace::new());
    let root = ada.host.root_of(TUBE).unwrap();
    seed(&ada, &meta, "a video playing");
    ada.commit();
    let magnet = video(&ada, &meta, &root, "v-magnet");
    let mut page = ada.host.open_resource(&magnet, None).unwrap();
    let player = drawn(&mut page, "ui.VideoPlayer").key;
    ada.commit();

    page.fire_with(
        &player,
        "fail",
        Some(&Value::text("YouTube refused this stream (403)")),
    )
    .unwrap();
    ada.commit();
    let banner = drawn(&mut page, "ui.Banner");
    assert_eq!(
        banner.props.get("title"),
        Some(&Value::text("This video won't play here")),
        "the page says the video will not play here"
    );
    assert_eq!(
        banner.props.get("message"),
        Some(&Value::text("YouTube refused this stream (403)")),
        "and why, in the engine's words"
    );
    assert_eq!(
        banner.props.get("action"),
        Some(&Value::text("Try again")),
        "with the way to ask again"
    );

    page.fire(&banner.key, "act").unwrap();
    ada.commit();
    let stream = field_id(&meta, "Video", "stream");
    assert_eq!(
        ada.read(&magnet, &stream),
        Value::text("https://media.example/v-magnet.mp4"),
        "the address stays the service's to write: only it writes a video's `stream`"
    );
    assert!(
        !ada.host.store().outbox_pending().is_empty(),
        "and the service is asked for the address again"
    );
}

/// A playlist shows the videos it holds, in the order its maker put them, and Play all plays its
/// first: not the videos of whoever made it.
#[test]
fn a_playlist_shows_its_own_videos_in_order_and_plays_the_first() {
    let client = client();
    let Client { ada, meta, root } = &client;
    let playlists = items(ada, root, &field_id(meta, "YouTube", "playlists"));
    let science = playlists
        .into_iter()
        .find(|at| ada.read(at, &field_id(meta, "Playlist", "key")) == Value::text("pl-science"))
        .expect("the playlist the scenario seeded");
    let mut page = ada.host.open_resource(&science, None).unwrap();
    let titles = named(&mut page, "ui.MediaCard", "title");
    assert_eq!(
        titles,
        [
            "Why undersea cables do not break",
            "Why every magnet you own is lying to you"
        ],
        "the playlist's own videos, older first, as its maker ordered them"
    );

    ada.commit();
    let play_all = button(&mut page, "Play all");
    page.press(&play_all).unwrap();
    ada.commit();
    let first = video(ada, meta, root, "v-wires");
    let entry = history_entry(ada, meta, root, "v-wires", Some("ada"))
        .expect("playing the playlist's first video watched it");
    assert_eq!(
        ada.read(&first, &field_id(meta, "Video", "key")),
        Value::text("v-wires")
    );
    assert_eq!(progress_of(ada, meta, &entry), Value::Float(0.0));
}

/// Each member reads their own watch history: what one watched is not in the other's.
#[test]
fn a_members_watch_history_is_their_own() {
    let client = client();
    let Client { ada, meta, root } = &client;
    let (bo, _) = replica("bo", ada.doc.fork());
    ada.sync_with(&bo);

    let cache = video(ada, meta, root, "v-cache");
    let bread = video(ada, meta, root, "v-bread");
    call(
        ada,
        meta,
        "progress",
        vec![Value::resource(&cache), Value::Float(0.25)],
    );
    call(
        &bo,
        meta,
        "progress",
        vec![Value::resource(&bread), Value::Float(0.5)],
    );
    ada.commit();
    bo.commit();
    ada.sync_with(&bo);

    let cache_entry =
        history_entry(ada, meta, root, "v-cache", Some("ada")).expect("ada's own entry");
    let bread_entry =
        history_entry(&bo, meta, root, "v-bread", Some("bo")).expect("bo's own entry");
    let mut hers = ada.host.open_resource(&cache_entry, None).unwrap();
    let hers = row_titles(&mut hers);
    assert!(
        hers.contains(&"A cache is a lie your computer tells you".to_owned()),
        "ada's history holds what she watched: {hers:?}"
    );
    assert!(
        !hers.contains(&"Bread in a one-metre kitchen".to_owned()),
        "and not what bo watched: {hers:?}"
    );
    let mut his = bo.host.open_resource(&bread_entry, None).unwrap();
    let his = row_titles(&mut his);
    assert!(
        his.contains(&"Bread in a one-metre kitchen".to_owned()),
        "bo's history holds what he watched: {his:?}"
    );
    assert!(
        !his.contains(&"A cache is a lie your computer tells you".to_owned()),
        "and not what ada watched: {his:?}"
    );
}

/// Playback preferences are each member's own: one sets a speed and autoplay, the other sets a
/// quality, and neither reads the other's.
#[test]
fn playback_preferences_are_each_members_own() {
    let client = client();
    let Client { ada, meta, root } = &client;
    let (bo, _) = replica("bo", ada.doc.fork());
    ada.sync_with(&bo);

    call(
        ada,
        meta,
        "set_speed",
        vec![Value::resource(root), Value::Float(1.5)],
    );
    call(
        ada,
        meta,
        "set_autoplay",
        vec![Value::resource(root), Value::Bool(true)],
    );
    call(
        &bo,
        meta,
        "set_quality",
        vec![Value::resource(root), Value::text("480p")],
    );
    ada.commit();
    bo.commit();
    ada.sync_with(&bo);

    let prefs = |replica: &Replica, who: &str| -> Vec<Value> {
        let member_id = field_id(meta, "PlaybackPrefs", "member");
        items(replica, root, &field_id(meta, "YouTube", "playback"))
            .into_iter()
            .find(|at| replica.read(at, &member_id) == Value::Actor(person(who)))
            .map(|at| {
                [
                    replica.read(&at, &field_id(meta, "PlaybackPrefs", "speed")),
                    replica.read(&at, &field_id(meta, "PlaybackPrefs", "autoplay")),
                    replica.read(&at, &field_id(meta, "PlaybackPrefs", "quality")),
                ]
                .to_vec()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        prefs(&bo, "ada"),
        [Value::Float(1.5), Value::Bool(true), Value::text("auto")],
        "ada's record reached bo's device, with her speed and autoplay and no quality of her own"
    );
    assert_eq!(
        prefs(ada, "bo"),
        [Value::Float(1.0), Value::Bool(false), Value::text("480p")],
        "and bo's is his: the default speed, no autoplay, and the quality he chose"
    );
}

/// The watch queue keeps the order videos were added in: Play all plays the video that was put on
/// it first, not the one added last.
#[test]
fn the_watch_queue_plays_in_the_order_videos_were_added() {
    let client = client();
    let Client { ada, meta, root } = &client;
    // The scenario puts the rehearsal on the queue first, then the bread; two more go on it in
    // the order they are added.
    let queue_field = field_id(meta, "YouTube", "queue");
    for key in ["v-phone", "v-cache"] {
        let at = video(ada, meta, root, key);
        call(
            ada,
            meta,
            "watch_later",
            vec![Value::resource(&at), Value::Bool(true)],
        );
    }
    ada.commit();
    let queued: Vec<String> = items(ada, root, &queue_field)
        .into_iter()
        .map(|at| text(ada.read(&at, &field_id(meta, "QueuedVideo", "key"))))
        .collect();
    assert_eq!(queued, ["v-rehearsal", "v-bread", "v-phone", "v-cache"]);

    ada.commit();
    call(ada, meta, "play_queue", vec![Value::resource(root)]);
    ada.commit();
    let entry = history_entry(ada, meta, root, "v-rehearsal", Some("ada"))
        .expect("the queue's oldest video was watched");
    assert_eq!(progress_of(ada, meta, &entry), Value::Float(0.0));
}
