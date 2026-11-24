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
    // The shared player comes first: a package compiles against the ones loaded before it.
    let (replica, packages) = Replica::access(
        name,
        doc,
        &[(PLAYBACK_SOURCE, PLAYBACK_IDS), (SOURCE, IDS)],
        Some(access(name)),
    );
    load_platform(&replica.host);
    (replica, packages.last().unwrap().meta.clone())
}

/// The platform's packages the client's source names, as the window's host holds them: its
/// presses reach the session's own actions, served by the device.
fn load_platform(host: &Host) {
    for meta in tilolang_tilo::platform_apps::named_platform_packages(SOURCE)
        .expect("the platform's packages read")
    {
        host.runtime()
            .load_foreign(meta)
            .expect("the platform package loads");
    }
}

const TUBE: &str = "so.tilo.tube";
const SOURCE: &str = include_str!("../../../../apps/tl/tube/app.tl");
const IDS: &str = include_str!("../../../../apps/tl/tube/app.ids");
/// The shared player Tube plays through: the session's own type lives there.
const PLAYBACK: &str = "so.tilo.playback";
const PLAYBACK_SOURCE: &str = include_str!("../../../../apps/tl/playback/app.tl");
const PLAYBACK_IDS: &str = include_str!("../../../../apps/tl/playback/app.ids");

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
    // The client's first scenario is its seed: what YouTube holds comes in through the YouTube
    // origin, from the sample YouTube, before the scenario follows, likes and watches it.
    if meta.scenarios.first().is_some_and(|first| first.name == name) {
        replica.host.root_of(TUBE).unwrap();
        tilolang_youtube::sample::take_in(&replica.host, TUBE, "2026-11-22T12:00:00Z").unwrap();
    }
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

/// Every node the window shows, the rows of its lists and grids included: a virtual list or grid
/// builds the rows it shows only when the window asks for them, as `view.row` does, so a read of
/// the tree alone would miss every card a person sees.
fn shown_nodes(view: &mut tilolang_runtime::ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let mut out: Vec<UiNode> = nodes(view.tree()).into_iter().cloned().collect();
    let lists: Vec<String> = out
        .iter()
        .filter(|node| {
            matches!(
                node.kind.as_str(),
                "list" | "ui.VirtualList" | "ui.TreeView" | "ui.MediaGrid"
            )
        })
        .map(|node| node.key.clone())
        .collect();
    for key in lists {
        let len = view.list_len(&key).unwrap_or(0);
        for at in 0..len {
            if let Ok(Some(row)) = view.row(&key, at) {
                out.extend(nodes(row).into_iter().cloned());
            }
        }
    }
    out
}

/// The text `view` draws of the nodes of `kind`, by the prop it shows it in, in the order they
/// stand.
fn named(view: &mut tilolang_runtime::ViewInstance, kind: &str, prop: &str) -> Vec<String> {
    shown_nodes(view)
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

/// Presses the page's Play, as a person starts the video, and answers the window: the page draws
/// the session's player once the video plays, and the poster and Play stand there until it does.
fn play_it(page: &mut tilolang_runtime::ViewInstance, replica: &Replica) {
    let play = button(page, "Play");
    page.press(&play).unwrap();
    replica.commit();
    page.poll_patches().unwrap();
}

/// The page's player, after the person started the video there.
fn started(page: &mut tilolang_runtime::ViewInstance, replica: &Replica) -> UiNode {
    play_it(page, replica);
    drawn(page, "ui.VideoPlayer")
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
    let player = started(&mut page, &ada).key;

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
    let player = started(&mut page, &ada).key;

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

/// Playback preferences are each member's own: one turns autoplay on, the other picks a quality,
/// and neither reads the other's.
#[test]
fn playback_preferences_are_each_members_own() {
    let client = client();
    let Client { ada, meta, root } = &client;
    let (bo, _) = replica("bo", ada.doc.fork());
    ada.sync_with(&bo);

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
                    replica.read(&at, &field_id(meta, "PlaybackPrefs", "autoplay")),
                    replica.read(&at, &field_id(meta, "PlaybackPrefs", "quality")),
                ]
                .to_vec()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        prefs(&bo, "ada"),
        [Value::Bool(true), Value::text("auto")],
        "ada's record reached bo's device, with her autoplay and no quality of her own"
    );
    assert_eq!(
        prefs(ada, "bo"),
        [Value::Bool(false), Value::text("480p")],
        "and bo's is his: no autoplay, and the quality he chose"
    );
}

/// The playback package's metadata, where the shared session's own type lives.
fn playback_meta(replica: &Replica) -> Meta {
    replica
        .host
        .packages()
        .iter()
        .find(|package| package.meta.app == PLAYBACK)
        .expect("the shared player is loaded")
        .meta
        .clone()
}

/// The session the client root plays through.
fn session_of(replica: &Replica, meta: &Meta, root: &ResourceRef) -> ResourceRef {
    match replica.read(root, &field_id(meta, "YouTube", "playing")) {
        Value::Opt(Some(held)) => match *held {
            Value::Ref(at) => (*at.target).clone(),
            other => panic!("the session is a resource: {other:?}"),
        },
        other => panic!("the client plays nothing: {other:?}"),
    }
}

/// A field of the session, as the player's reports left it.
fn played(replica: &Replica, playback: &Meta, session: &ResourceRef, field: &str) -> Value {
    replica.read(session, &field_id(playback, "Playback", field))
}

/// Playing a video is the shared session, and the player's own reports move it: what plays,
/// where its bytes come from and how long it runs are the session's, so the shell's live card,
/// the media keys and another device read what the person watches here.
#[test]
fn playing_a_video_is_the_shared_session_and_the_player_moves_it() {
    let (ada, meta) = replica("ada", Workspace::new());
    let root = ada.host.root_of(TUBE).unwrap();
    seed(&ada, &meta, "a video playing");
    ada.commit();
    let playback = playback_meta(&ada);
    let magnet = video(&ada, &meta, &root, "v-magnet");

    call(&ada, &meta, "play", vec![Value::resource(&magnet)]);
    ada.commit();
    let session = session_of(&ada, &meta, &root);
    assert_eq!(played(&ada, &playback, &session, "track"), Value::text("v-magnet"));
    assert_eq!(
        played(&ada, &playback, &session, "title"),
        Value::text("Why every magnet you own is lying to you")
    );
    assert_eq!(played(&ada, &playback, &session, "artist"), Value::text("Veritasium"));
    assert_eq!(
        played(&ada, &playback, &session, "source"),
        Value::text("https://media.example/v-magnet.mp4"),
        "the address the player opens is the session's, so every device plays the same bytes"
    );
    assert_eq!(played(&ada, &playback, &session, "length"), Value::Float(1342.0));
    // The session's state is the player's own kind of playback, kept as the package declares it:
    // the record reads as the case `playing`, beside the session it stands in.
    let playing = playback
        .enums
        .iter()
        .flat_map(|declared| declared.variants.iter())
        .find(|variant| variant.name == "playing")
        .map(|variant| variant.id.clone())
        .expect("the player declares its playing state");
    match played(&ada, &playback, &session, "state") {
        Value::Enum(case) => assert_eq!(
            case.variant, playing,
            "the session plays the video: {case:?}"
        ),
        other => panic!("the session's state is a playback state: {other:?}"),
    }

    // The page's player is the session's: its play state and place come from it, and what the
    // person presses there writes back into it.
    let mut page = ada.host.open_resource(&magnet, None).unwrap();
    let player = drawn(&mut page, "ui.VideoPlayer");
    assert!(
        matches!(
            player.props.get("playing"),
            Some(Value::Bool(true) | Value::Int(1))
        ),
        "the page's player plays what the session plays: {:?}",
        player.props.get("playing")
    );
    page.fire_with(&player.key, "progress", Some(&Value::Float(0.6)))
        .unwrap();
    ada.commit();
    let Value::Float(at) = played(&ada, &playback, &session, "at") else {
        panic!("the session holds a place");
    };
    assert!(
        (at - 805.2).abs() < 0.01,
        "six tenths of the video, as the player reported it: {at}"
    );
    page.fire_with(&player.key, "seek", Some(&Value::Float(0.25)))
        .unwrap();
    ada.commit();
    let Value::Float(at) = played(&ada, &playback, &session, "at") else {
        panic!("the session holds a place");
    };
    assert!(
        (at - 335.5).abs() < 0.01,
        "a quarter of the way in, where the person moved the playhead: {at}"
    );
    page.fire_with(&player.key, "set_playing", Some(&Value::Bool(false)))
        .unwrap();
    ada.commit();
    let paused = playback
        .enums
        .iter()
        .flat_map(|declared| declared.variants.iter())
        .find(|variant| variant.name == "paused")
        .map(|variant| variant.id.clone())
        .expect("the player declares its paused state");
    match played(&ada, &playback, &session, "state") {
        Value::Enum(case) => assert_eq!(
            case.variant, paused,
            "pausing in the player pauses the session: {case:?}"
        ),
        other => panic!("the session's state is a playback state: {other:?}"),
    }
}

/// Autoplay moves the session on to the next video of its queue, and that video is what the
/// person watched: the session plays it and the history keeps it.
#[test]
fn autoplay_moves_the_session_to_the_next_video() {
    let client = client();
    let Client { ada, meta, root } = &client;
    let playback = playback_meta(ada);
    call(ada, meta, "play_queue", vec![Value::resource(root)]);
    call(
        ada,
        meta,
        "set_autoplay",
        vec![Value::resource(root), Value::Bool(true)],
    );
    ada.commit();
    let session = session_of(ada, meta, root);
    assert_eq!(
        played(ada, &playback, &session, "track"),
        Value::text("v-rehearsal"),
        "the queue's oldest video plays first"
    );
    let queue = field_id(meta, "YouTube", "queue");
    assert_eq!(items(ada, root, &queue).len(), 2, "two videos wait on the queue");

    // The player reports the end of the video: what follows plays.
    call(
        ada,
        meta,
        "autoplay_next",
        vec![Value::resource(root), Value::resource(&session), Value::Float(1.0)],
    );
    ada.commit();
    assert_eq!(
        played(ada, &playback, &session, "track"),
        Value::text("v-bread"),
        "the session moved on to the queue's next video"
    );
    history_entry(ada, meta, root, "v-bread", Some("ada"))
        .expect("the next video is what the person watched now");
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

// The tests below drive the client as a session does: the effects its actions hand over go to
// the real YouTube origin, over a service that answers from a script (the connector's own
// boundary, with no network), and what it says is written back into the client's records.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tilolang_runtime::MemorySink;
use tilolang_tilo::OpenEffect;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::origin::{OriginCx, RoutingSink, SharedOrigin};
use tilolang_tilo::runtime::NameHost;
use tilolang_youtube::{
    Branding, CommentItem, CommentsPage, Scripted, SegmentItem, SegmentPage, Votes, scripted,
};

/// Hand the app's effects over to the device that carries them out, as a window's frames do: an
/// effect is marked as being handed over before it goes, so it takes two settlements.
fn settled(replica: &Replica) {
    replica.host.settle_outbox().unwrap();
    replica.commit();
    replica.host.settle_outbox().unwrap();
    replica.commit();
}

/// The client over the real YouTube origin, answered by a scripted service.
struct Connected {
    ada: Replica,
    meta: Meta,
    root: ResourceRef,
    origin: SharedOrigin,
}

fn connected(script: Scripted, scenario: &str) -> Connected {
    let (ada, meta) = replica("ada", Workspace::new());
    let origin = tilolang_youtube::origin(scripted(script), Arc::new(AtomicBool::new(false)));
    let shared: SharedOrigin = origin.clone();
    let origins = Rc::new(RefCell::new(vec![shared]));
    let sink = MemorySink::new();
    ada.host.runtime().set_sink(RoutingSink {
        origins,
        rest: Box::new(sink),
    });
    let root = ada.host.root_of(TUBE).unwrap();
    seed(&ada, &meta, scenario);
    ada.commit();
    Connected {
        ada,
        meta,
        root,
        origin,
    }
}

impl Connected {
    /// Hand the app's effects over and write what the service said into the records: what a
    /// window's frames do around the connector. An effect is marked as being handed over before
    /// it is handed over, so it takes two settlements.
    fn answered(&self) {
        settled(&self.ada);
        let waker: Waker = Arc::new(|| {});
        let cx = OriginCx {
            host: &self.ada.host,
            app: TUBE,
            waker: &waker,
            now: "2026-11-22T12:00:00Z".to_owned(),
        };
        self.origin.borrow_mut().poll(&cx);
        self.ada.commit();
    }

    /// The video keyed `key` the client holds.
    fn video(&self, key: &str) -> ResourceRef {
        video(&self.ada, &self.meta, &self.root, key)
    }

    /// Every item of the root's collection `collection`, by the key it holds.
    fn with_key(&self, collection: &str, ty: &str, key: &str) -> ResourceRef {
        items(
            &self.ada,
            &self.root,
            &field_id(&self.meta, "YouTube", collection),
        )
        .into_iter()
        .find(|at| self.ada.read(at, &field_id(&self.meta, ty, "key")) == Value::text(key))
        .unwrap_or_else(|| panic!("the {ty} keyed {key}"))
    }
}

/// Open `at` the way the system does: run its type's `opened action` as the person, which is
/// what every opening does, wherever it came from.
fn opened(replica: &Replica, at: &ResourceRef) {
    let action = replica
        .host
        .runtime()
        .opened_action(&at.ty)
        .unwrap_or_else(|| panic!("opening a {} asks for what it shows", at.ty));
    match replica
        .host
        .runtime()
        .invoke_action(
            &person(&replica.name),
            (action.as_str(), vec![Value::resource(at)]),
        )
        .unwrap()
    {
        InvokeOutcome::Done(_) => {}
        other => panic!("opening {at:?}: {other:?}"),
    }
}

/// What the view draws as text, in order.
fn texts(view: &mut tilolang_runtime::ViewInstance) -> Vec<String> {
    view.poll_patches().unwrap();
    nodes(view.tree())
        .into_iter()
        .filter(|node| node.kind == "ui.Text" || node.kind == "text")
        .filter_map(|node| match node.props.get("value") {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Invoke `action` as the replica's person and answer what became of it. An action the app does
/// not have is a test's own fault, said where it is named.
#[allow(
    clippy::panic_in_result_fn,
    reason = "the action list is the test's fixture: a missing one is not the call's failure"
)]
fn invoked(
    replica: &Replica,
    meta: &Meta,
    action: &str,
    args: Vec<Value>,
) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    replica
        .host
        .runtime()
        .invoke_action(&person(&replica.name), (id.as_str(), args))
}

/// The client over a sink only: its effects are recorded where they are handed over, for what
/// the app asks of the connector rather than what it writes of the answer.
struct SinkClient {
    ada: Replica,
    meta: Meta,
    root: ResourceRef,
    sink: MemorySink,
}

fn sink_client(scenario: &str) -> SinkClient {
    let (ada, meta) = replica("ada", Workspace::new());
    let sink = MemorySink::new();
    ada.host.runtime().set_sink(sink.clone());
    let root = ada.host.root_of(TUBE).unwrap();
    seed(&ada, &meta, scenario);
    ada.commit();
    SinkClient {
        ada,
        meta,
        root,
        sink,
    }
}

impl SinkClient {
    /// The video keyed `key` the client holds.
    fn video(&self, key: &str) -> ResourceRef {
        video(&self.ada, &self.meta, &self.root, key)
    }
}

/// What the client asked of the connector, in order.
fn asked(sink: &MemorySink) -> Vec<(String, serde_json::Value)> {
    sink.delivered()
}

/// A window of the client, for the one path only the instance owns: a resource opened from
/// search.
struct TubeWindow;

impl Platform for TubeWindow {
    fn load(&self, host: &Host) -> u64 {
        load_platform(host);
        host.load_builtin(PLAYBACK_SOURCE, PLAYBACK_IDS).unwrap();
        host.load_builtin(SOURCE, IDS).unwrap();
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-22T12:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Actor) -> String {
        who.id().to_owned()
    }
}

/// A playlist a search found is known by its name and cover alone. Opening it asks the service
/// what it holds, so its page fills with its videos instead of the empty state until someone
/// presses Refresh.
#[test]
fn a_playlist_opened_from_a_search_asks_for_its_videos() {
    let sink = MemorySink::new();
    let (app_end, _window) = tilo_render_protocol::patch::local::pair();
    let mut window = Instance::new(
        Config {
            app: TUBE.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            link_home: None,
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            root: RootView::default(),
            sink: Some(Box::new(sink.clone())),
        },
        Rc::new(TubeWindow),
        Rc::new(Alone),
        Box::new(app_end),
    );
    window.set_access(Rc::new(|| Some(access("ada"))));
    window.sync();

    {
        let host = window.host().expect("the window opened");
        let meta = host.package(TUBE).unwrap().meta;
        let scenario = meta
            .scenarios
            .iter()
            .find(|scenario| scenario.name == "search results")
            .expect("the search scenario")
            .action
            .clone();
        host.runtime()
            .invoke_action(&person("ada"), (scenario.as_str(), vec![]))
            .unwrap();
    }
    window.sync();
    let playlist = {
        let host = window.host().unwrap();
        let root = host.root_of(TUBE).unwrap();
        let Value::List(playlists) = host.runtime().read(&root, "playlists").unwrap() else {
            panic!("the client holds its playlists");
        };
        playlists
            .into_iter()
            .filter_map(|item| match item {
                Value::Ref(r) => Some((*r.target).clone()),
                _ => None,
            })
            .find(|at| host.runtime().read(at, "key").unwrap() == Value::text("pl-phones"))
            .expect("the playlist the search found")
    };

    // The system opens it: the window shows it, and what opening it does asks the service.
    window.open_requests().borrow_mut().push(OpenEffect::Open {
        anchor: playlist.id_string(),
    });
    for _ in 0..3 {
        window.update();
        window.sync();
    }
    assert!(
        sink.delivered().iter().any(|(capability, payload)| {
            capability == "youtube.playlist" && payload == &serde_json::json!("pl-phones")
        }),
        "opening the playlist asked YouTube what it holds: {:?}",
        sink.delivered()
    );
}

/// A video's comments are read from the service, drawn on its page, and a member replies to one;
/// only the member who wrote a reply takes it back.
#[test]
fn comments_are_read_and_only_their_writer_takes_a_reply_back() {
    let mut script = Scripted::default();
    script.comments.insert(
        "v-cache".to_owned(),
        CommentsPage {
            video: "v-cache".to_owned(),
            comments: vec![
                CommentItem {
                    key: "c-love".to_owned(),
                    video_key: "v-cache".to_owned(),
                    author: "Sol".to_owned(),
                    text: "This finally made caches click".to_owned(),
                    likes: 12,
                    published: "2026-11-20T10:00:00Z".to_owned(),
                    ..CommentItem::default()
                },
                CommentItem {
                    key: "c-meh".to_owned(),
                    video_key: "v-cache".to_owned(),
                    author: "Bo".to_owned(),
                    text: "Too fast for me".to_owned(),
                    likes: 1,
                    published: "2026-11-21T10:00:00Z".to_owned(),
                    ..CommentItem::default()
                },
            ],
        },
    );
    let client = connected(script, "a subscriptions feed");
    let cache = client.video("v-cache");
    opened(&client.ada, &cache);
    client.answered();
    let mut page = client.ada.host.open_resource(&cache, None).unwrap();

    let shown = texts(&mut page);
    assert!(
        shown
            .iter()
            .any(|text| text == "This finally made caches click"),
        "the page draws what people said: {shown:?}"
    );
    assert!(shown.iter().any(|text| text == "Sol"), "{shown:?}");

    let love = client.with_key("comments", "Comment", "c-love");
    let answer = call(
        &client.ada,
        &client.meta,
        "reply_to_comment",
        vec![Value::resource(&love), Value::text("Same here")],
    );
    let Value::Ref(reply) = answer else {
        panic!("a reply names what it made: {answer:?}");
    };
    let reply = (*reply.target).clone();
    let mut page = client.ada.host.open_resource(&cache, None).unwrap();
    let shown = texts(&mut page);
    assert!(
        shown.iter().any(|text| text == "Same here"),
        "the reply stands under the comment it answers: {shown:?}"
    );

    // A reply is its writer's to take back: Bo did not write it.
    let (bo, _) = replica("bo", client.ada.doc.fork());
    let refused = invoked(
        &bo,
        &client.meta,
        "retract_reply",
        vec![Value::resource(&reply)],
    );
    assert!(
        !matches!(refused, Ok(InvokeOutcome::Done(_))),
        "someone else's reply is not Bo's to take back: {refused:?}"
    );

    call(
        &client.ada,
        &client.meta,
        "retract_reply",
        vec![Value::resource(&reply)],
    );
    let mut page = client.ada.host.open_resource(&cache, None).unwrap();
    let shown = texts(&mut page);
    assert!(
        !shown.iter().any(|text| text == "Same here"),
        "the writer took their reply back: {shown:?}"
    );
}

/// The segments SponsorBlock marks are skipped as playback reaches them, and stay where they are
/// when the person turns skipping off.
#[test]
fn a_segment_the_community_marks_is_skipped_as_it_plays() {
    let mut script = Scripted::default();
    script.segments.insert(
        "v-magnet".to_owned(),
        SegmentPage {
            video: "v-magnet".to_owned(),
            segments: vec![SegmentItem {
                video_key: "v-magnet".to_owned(),
                category: "sponsor".to_owned(),
                start: 0.0,
                end: 120.0,
                votes: 30,
            }],
        },
    );
    let client = connected(script, "a video playing");
    let magnet = client.video("v-magnet");
    let mut page = client.ada.host.open_resource(&magnet, None).unwrap();
    opened(&client.ada, &magnet);
    client.answered();
    let player = started(&mut page, &client.ada).key;

    // Sixty seconds in is a sponsor read: the playhead moves to where it ends.
    page.fire_with(&player, "progress", Some(&Value::Float(0.0447)))
        .unwrap();
    client.ada.commit();
    let entry = history_entry(
        &client.ada,
        &client.meta,
        &client.root,
        "v-magnet",
        Some("ada"),
    )
    .expect("the video being watched");
    let Value::Float(at) = progress_of(&client.ada, &client.meta, &entry) else {
        panic!("a share of the video");
    };
    assert!(
        at > 0.08 && at < 0.1,
        "the segment was skipped to its end: {at} of the video"
    );

    // Turned off, the same report is where the person was.
    call(
        &client.ada,
        &client.meta,
        "set_sponsorblock",
        vec![Value::resource(&client.root), Value::Bool(false)],
    );
    page.fire_with(&player, "progress", Some(&Value::Float(0.0447)))
        .unwrap();
    client.ada.commit();
    let entry = history_entry(
        &client.ada,
        &client.meta,
        &client.root,
        "v-magnet",
        Some("ada"),
    )
    .expect("the video being watched");
    let Value::Float(at) = progress_of(&client.ada, &client.meta, &entry) else {
        panic!("a share of the video");
    };
    assert!(
        at < 0.05,
        "nothing is skipped once the person turns it off: {at} of the video"
    );
}

/// DeArrow's community title and Return YouTube Dislike's count are drawn on the page, and the
/// person can show the maker's own title again.
#[test]
fn the_community_names_the_video_and_counts_its_dislikes() {
    let mut script = Scripted::default();
    script.branding.insert(
        "v-magnet".to_owned(),
        Branding {
            video: "v-magnet".to_owned(),
            title: "Magnetism, without the lie".to_owned(),
            thumbnail: String::new(),
        },
    );
    script.votes.insert(
        "v-magnet".to_owned(),
        Votes {
            video: "v-magnet".to_owned(),
            dislikes: 4200,
            likes: Some(90000),
        },
    );
    let client = connected(script, "a video playing");
    let magnet = client.video("v-magnet");
    opened(&client.ada, &magnet);
    client.answered();
    let mut page = client.ada.host.open_resource(&magnet, None).unwrap();

    let shown = texts(&mut page);
    assert!(
        shown
            .iter()
            .any(|text| text == "Magnetism, without the lie"),
        "the page shows the community's title: {shown:?}"
    );
    assert!(
        !shown
            .iter()
            .any(|text| text == "Why every magnet you own is lying to you"),
        "and not the maker's: {shown:?}"
    );
    let facts = named(&mut page, "ui.Fact", "text");
    assert!(
        facts.iter().any(|text| text == "4.2K"),
        "the page shows how many disliked it: {facts:?}"
    );

    call(
        &client.ada,
        &client.meta,
        "set_dearrow",
        vec![Value::resource(&client.root), Value::Bool(false)],
    );
    let mut page = client.ada.host.open_resource(&magnet, None).unwrap();
    let shown = texts(&mut page);
    assert!(
        shown
            .iter()
            .any(|text| text == "Why every magnet you own is lying to you"),
        "the maker's title stands again: {shown:?}"
    );
}

/// The kind a search shows narrows the page at once: videos alone, not the channels it also
/// found.
#[test]
fn the_search_narrows_to_the_kind_the_person_chose() {
    let client = client();
    seed(&client.ada, &client.meta, "search results");
    client.ada.commit();
    let mut feed = client.ada.view(TUBE);
    let channels = named(&mut feed, "ui.ListRow", "title");
    assert!(
        channels.iter().any(|name| name == "MKBHD"),
        "the search shows the channel it found: {channels:?}"
    );

    call(
        &client.ada,
        &client.meta,
        "filter_search",
        vec![Value::resource(&client.root), Value::text("video")],
    );
    let mut feed = client.ada.view(TUBE);
    let videos = named(&mut feed, "ui.MediaCard", "title");
    assert!(
        videos
            .iter()
            .any(|title| title == "The phone that finally lasts a week"),
        "the videos show: {videos:?}"
    );
    let channels = named(&mut feed, "ui.ListRow", "title");
    assert!(
        !channels.iter().any(|name| name == "MKBHD"),
        "and the channels do not: {channels:?}"
    );
}

/// A length or a date narrows what YouTube is asked for: the search is asked again with the
/// filter the person chose.
#[test]
fn the_search_asks_youtube_with_its_length_and_date() {
    let client = sink_client("a subscriptions feed");
    call(
        &client.ada,
        &client.meta,
        "search",
        vec![Value::resource(&client.root), Value::text("phone reviews")],
    );
    settled(&client.ada);
    assert!(
        asked(&client.sink).iter().any(|(capability, payload)| {
            capability == "youtube.search"
                && payload == &serde_json::json!(["phone reviews", "all", "any", "any"])
        }),
        "the search asked YouTube as it stood: {:?}",
        asked(&client.sink)
    );

    call(
        &client.ada,
        &client.meta,
        "filter_length",
        vec![Value::resource(&client.root), Value::text("short")],
    );
    settled(&client.ada);
    assert!(
        asked(&client.sink).iter().any(|(capability, payload)| {
            capability == "youtube.search"
                && payload == &serde_json::json!(["phone reviews", "all", "short", "any"])
        }),
        "the search was asked again with the length: {:?}",
        asked(&client.sink)
    );

    call(
        &client.ada,
        &client.meta,
        "filter_published",
        vec![Value::resource(&client.root), Value::text("week")],
    );
    settled(&client.ada);
    assert!(
        asked(&client.sink).iter().any(|(capability, payload)| {
            capability == "youtube.search"
                && payload == &serde_json::json!(["phone reviews", "all", "short", "week"])
        }),
        "and with the date: {:?}",
        asked(&client.sink)
    );
}

/// The person's preferred quality is what the service is asked for a stream at: 480p is what the
/// player opens, not the tallest picture YouTube serves.
#[test]
fn the_preferred_quality_is_what_the_stream_is_asked_at() {
    let client = sink_client("a subscriptions feed");
    call(
        &client.ada,
        &client.meta,
        "set_quality",
        vec![Value::resource(&client.root), Value::text("480p")],
    );
    let cache = client.video("v-cache");
    opened(&client.ada, &cache);
    settled(&client.ada);
    assert!(
        asked(&client.sink).iter().any(|(capability, payload)| {
            capability == "youtube.stream" && payload == &serde_json::json!(["v-cache", "480p"])
        }),
        "the stream is asked at the quality the person chose: {:?}",
        asked(&client.sink)
    );
}
