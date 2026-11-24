//! Chat in a hosted window: the bar's New channel, run by the window as its own action — an
//! address on the app's root and no argument — makes a channel the app's own read holds.

use super::*;

const CHAT: &str = "so.tilo.chat";

/// Chat's instance in a workspace of its own.
fn chatting() -> (TilolangProcess, Window, Sent, Inbox, RouteSpec) {
    running_app("ada", CHAT, shipped(&shipped_dir().join("chat")).1)
}

/// The runtime the hosted instance runs over, as a handle of its own.
fn runtime(host: &TilolangProcess) -> tilolang_tilo::host::Host {
    host.instances()
        .next()
        .expect("the instance runs")
        .instance()
        .host()
        .expect("the runtime")
}

/// The channels the app's own read holds.
fn channels_of(runtime: &tilolang_tilo::host::Host) -> Vec<Value> {
    let root = runtime.root_of(CHAT).expect("the chat root");
    match runtime
        .runtime()
        .read(&root, "channels")
        .expect("the channels")
    {
        Value::List(items) => items,
        other => panic!("a list: {other:?}"),
    }
}

#[test]
fn the_bars_new_channel_makes_one() {
    let (mut host, mut window, _sent, _inbox, _route) = chatting();
    settle(&mut host, &mut window);

    // What the window does on the bar's New channel: the app's address and no argument. Every
    // argument the action takes has a default, so nothing is asked and the channel is made.
    let address = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == "New channel")
        .expect("the app publishes the action that makes a channel")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address,
        argument: None,
    }]);
    settle(&mut host, &mut window);
    assert!(
        window.form.is_none(),
        "nothing is asked of the person: {:?}",
        window.form.as_ref().map(|form| form.title.clone())
    );
    let chat = runtime(&host);
    let channels = channels_of(&chat);
    assert_eq!(
        channels.len(),
        1,
        "the bar made a channel: {channels:?}; the app said {:?}",
        window.notices
    );
    let Value::Ref(channel) = &channels[0] else {
        panic!("a channel is a resource: {:?}", channels[0]);
    };
    assert_eq!(
        chat.runtime()
            .read(&channel.target, "name")
            .expect("its name"),
        Value::text("new-channel"),
        "the channel is named by the words the action defaults to"
    );
}

// A member's picture, as the picture the toolkit's fixture carries: the image's own bytes.
fn picture() -> collab_types::AvatarBytes {
    collab_types::AvatarBytes(
        collab_types::photo_bytes(ui::headless::FIXTURE_PICTURE).expect("the fixture picture"),
    )
}

// The Chat instance of `person`, as the host runs it.
fn chat_of(host: &TilolangProcess, person: &str) -> tilolang_tilo::host::Host {
    host.instances()
        .find(|hosted| hosted.spec().person == person)
        .unwrap_or_else(|| panic!("{person}'s Chat runs"))
        .instance()
        .host()
        .expect("the runtime")
}

// Open `place`, the sidebar item labelled so, in `window`.
fn open_place(window: &mut Window, place: &str) {
    let item = window
        .root
        .as_ref()
        .expect("the window shows the app")
        .of_kind("ui.NavItem")
        .into_iter()
        .find(|node| node.props.get("label") == Some(&patch::Value::Text(place.to_owned())))
        .unwrap_or_else(|| panic!("the sidebar holds {place}"))
        .clone();
    assert!(
        item.events.contains_key("select"),
        "a place is selected: {:?}",
        item.events.keys().collect::<Vec<_>>()
    );
    window.send(&[ToApp::Fire {
        level: Level::Root,
        key: item.key,
        event: "select".to_owned(),
        value: None,
        gesture: None,
    }]);
}

// The key of the composer's rich input in the open conversation.
fn composer(window: &Window) -> String {
    window
        .detail
        .as_ref()
        .expect("the conversation is open")
        .first_of_kind("ui.RichInput")
        .expect("the conversation has a composer")
        .key
        .clone()
}

// The key the transcript's list stands under in the open conversation.
fn transcript(window: &Window) -> String {
    window
        .detail
        .as_ref()
        .expect("the conversation is open")
        .first_of_kind("ui.VirtualList")
        .expect("the conversation has a transcript")
        .key
        .clone()
}

/// A member's picture reaches another member's view of a message said before they set it: the
/// transcript's row names Ada as its author, and draws the picture she sets afterwards.
#[test]
fn a_members_picture_reaches_another_members_view_of_an_earlier_message() {
    let workspace = uuid::Uuid::new_v4();
    let route = RouteSpec::app(CHAT, workspace);
    let bus = Rc::new(Bus::default());
    let mut host = TilolangProcess::new(wake());
    let (mut ada, ..) = instance_on(
        &mut host,
        spec(1, "ada", &route),
        shipped(&shipped_dir().join("chat")).1,
        &route,
        Rc::default(),
        &bus,
    );
    let (mut bo, ..) = instance_on(
        &mut host,
        spec(2, "bo", &route),
        shipped(&shipped_dir().join("chat")).1,
        &route,
        Rc::default(),
        &bus,
    );
    let ada_id = ui::permissions::MemberId::person("ada");
    let bo_id = ui::permissions::MemberId::person("bo");
    bus.member(ada_id.clone(), "Ada");
    bus.member(bo_id, "Bo");
    settle_all(&mut host, &mut [&mut ada, &mut bo], |_| false);

    // Ada makes a channel and says something in it, through the window she looks at.
    let address = host
        .instances()
        .find(|hosted| hosted.spec().person == "ada")
        .expect("Ada's Chat runs")
        .published()
        .iter()
        .find(|action| action.action_label == "New channel")
        .expect("the app offers New channel")
        .address
        .clone();
    ada.send(&[ToApp::Invoke {
        address,
        argument: None,
    }]);
    settle_all(&mut host, &mut [&mut ada, &mut bo], |_| false);
    open_place(&mut ada, "new-channel");
    settle_all(&mut host, &mut [&mut ada, &mut bo], |_| false);
    let input = composer(&ada);
    let target = ada.detail_target.clone().expect("the channel is shown");
    ada.send(&[ToApp::EditRich {
        level: Level::Detail,
        target,
        key: input.clone(),
        rich: typed_rich("Lunch is at noon."),
        gesture: None,
    }]);
    settle_all(&mut host, &mut [&mut ada, &mut bo], |_| false);
    ada.send(&[ToApp::Fire {
        level: Level::Detail,
        key: input,
        event: "submit".to_owned(),
        value: None,
        gesture: None,
    }]);
    settle_all(&mut host, &mut [&mut ada, &mut bo], |_| false);
    assert_eq!(
        messages_of(&chat_of(&host, "ada")),
        1,
        "the message is said in the channel"
    );

    // Bo opens the same conversation and reads Ada's message: its row names her, and her
    // picture is not one she set yet.
    open_place(&mut bo, "new-channel");
    settle_all(&mut host, &mut [&mut ada, &mut bo], |_| false);
    let list = transcript(&bo);
    let said = bo.detail.clone().expect("the conversation is open");
    bo.request_rows(Level::Detail, &said, 0, 10);
    settle_all(&mut host, &mut [&mut ada, &mut bo], |_| false);
    let author = |rows: &[patch::Node]| {
        let of = |value: &patch::Value| match value {
            patch::Value::Actor(author) => Some(author.clone()),
            patch::Value::Opt(Some(inner)) => match inner.as_ref() {
                patch::Value::Actor(author) => Some(author.clone()),
                _ => None,
            },
            _ => None,
        };
        // Each row the list draws wraps the item that names its author.
        rows.iter()
            .filter_map(|row| row.first_of_kind("ui.ListItem"))
            .find_map(|item| item.props.get("author").and_then(of))
            .expect("the transcript's row names its author")
    };
    let before = author(&bo.rows_of(&list));
    assert_eq!(before.name, "Ada", "the row names the member who said it");
    assert_eq!(before.picture, None, "she set no picture yet");

    // Ada sets a picture. Bo's view of the message she said earlier draws it, without the
    // message changing.
    bus.member_with_avatar(ada_id, "Ada", Some(picture()));
    settle_all(&mut host, &mut [&mut ada, &mut bo], |_| false);
    bo.request_rows(Level::Detail, &said, 0, 10);
    settle_all(&mut host, &mut [&mut ada, &mut bo], |_| false);
    let after = author(&bo.rows_of(&list));
    assert_eq!(after.name, "Ada");
    assert_eq!(
        after.picture.as_deref(),
        Some(ui::headless::FIXTURE_PICTURE),
        "the message said before her picture now draws it"
    );
}

// How many messages the channel of Ada's Chat holds.
fn messages_of(host: &tilolang_tilo::host::Host) -> usize {
    let root = host.root_of(CHAT).expect("the chat root");
    let Value::List(channels) = host
        .runtime()
        .read(&root, "channels")
        .expect("the channels")
    else {
        panic!("the channels are a list");
    };
    channels
        .into_iter()
        .filter_map(|channel| match channel {
            Value::Ref(channel) => Some(channel.target),
            _ => None,
        })
        .filter_map(|channel| match host.runtime().read(&channel, "messages") {
            Ok(Value::List(messages)) => Some(messages.len()),
            _ => None,
        })
        .sum()
}
