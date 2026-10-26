use super::*;
use ui::widget::Widget;

/// A window's person joins the roster through `introduce`.
#[test]
fn a_live_peer_joins_through_the_user_action() {
    let mut app = ChatApp::new(
        crate::identity::LocalIdentity::for_participant("person-1"),
        ChatRuntime::Unavailable,
        ContentStore::memory(),
    )
    .announcing();
    app.attach_framework(ui::framework::Dispatcher::detached(
        "so.tilo.chat",
        "person-1",
    ));
    let mut state = ui::State::new();
    crate::testing::starter(&mut state.doc, "owner", "Owner", "OW");
    Widget::sync(&mut app, &state.doc, &state.root);
    app.land(&mut state.doc);
    let recorded = app
        .last_dispatched()
        .expect("the live peer is submitted as introduce");
    assert_eq!(
        serde_json::to_value(&recorded).unwrap(),
        crate::testing::operation(&crate::resource::Chat::INTRODUCE.with(
            crate::record::chat::Introduce {
                name: app.me.name.clone(),
                initials: app.me.initials.clone(),
                color: u32::try_from(app.me.color_idx).unwrap(),
            }
        ))
    );
    let joined = crate::testing::users(&state.doc)
        .into_iter()
        .find(|user| user.id == app.me.id)
        .expect("the window's person is in the roster");
    assert_eq!(
        (joined.name, joined.initials, joined.status),
        (
            app.me.name.clone(),
            app.me.initials.clone(),
            crate::model::Presence::Online
        )
    );
}

#[test]
fn primary_scroll_maps_bottom_anchor_and_clamps_without_editing() {
    use ui::widget::PrimaryScrollAction;
    let mut app = ChatApp::new_ephemeral();
    app.scroll_content = 1000.0;
    app.scroll_viewport = 200.0;
    app.scroll = 0.0;
    assert_eq!(app.primary_vertical_scroll().unwrap().metrics.offset, 800.0);
    app.apply_primary_vertical_scroll(PrimaryScrollAction::Start);
    assert_eq!(app.scroll, 800.0);
    assert!(!app.timeline_at_end);
    app.apply_primary_vertical_scroll(PrimaryScrollAction::Line(1));
    assert_eq!(app.scroll, 752.0);
    app.apply_primary_vertical_scroll(PrimaryScrollAction::End);
    assert_eq!(app.scroll, 0.0);
    assert!(app.timeline_at_end);
    app.scroll_content = 100.0;
    assert_eq!(
        app.primary_vertical_scroll().unwrap().metrics.max_scroll(),
        0.0
    );
}

fn user(id: &str, is_agent: bool) -> User {
    User {
        id: id.into(),
        name: id.into(),
        initials: "U".into(),
        color_idx: 0,
        status: crate::model::Presence::Online,
        is_agent,
    }
}

#[test]
fn a_remote_saved_you_name_is_not_the_current_reader() {
    use ui::widget::Widget as _;
    let mut app = ChatApp::new_ephemeral();
    let mut state = ui::State::new();
    let stranger = "peer@9007495404254172154";
    let mut room = crate::testing::Room::default();
    crate::testing::starter(&mut room, stranger, "You", "Y");
    crate::testing::say(&mut room, stranger, "c_chat", "Retained foreign message");
    state.doc = room.chat.clone();
    app.attach_room(&room);
    app.sync(&state.doc, &state.root);
    assert_eq!(
        app.user(stranger)
            .expect("observe the actual foreign author in the production roster")
            .name,
        "You"
    );
    let shown = app.user_name(stranger);
    assert_ne!(
        shown, "You",
        "another person's stored name must not read as the current reader"
    );
    assert_eq!(
        app.display_name(stranger),
        shown,
        "mentions and message headers name the same foreign person"
    );
    assert!(
        shown.contains("2154"),
        "ambiguous names must distinguish the actual participant"
    );
    assert_eq!(app.user_name(&app.me.id), crate::identity::DEMO_NAME);
    assert_eq!(
        crate::testing::users(&state.doc)[0].name,
        "You",
        "presentation must not rewrite saved person data"
    );
}

#[test]
fn people_on_screen_are_named_honestly_and_you_by_your_name() {
    // Phase: roster_vocabulary_never_calls_an_agent_a_person.
    {
        let mut app = ChatApp::new_ephemeral();
        app.users = vec![user("u_me", false), user("u_scribe", true)];
        assert!(
            app.has_other_collaborator(),
            "the agent is another collaborator"
        );
        assert_eq!(app.roster_label(), "Collaborators");

        app.users = vec![user("u_me", false), user("u_aria", false)];
        assert_eq!(app.roster_label(), "People");
    }

    // Phase: an_unnamed_participant_is_not_shown_their_routing_token.
    {
        let app = ChatApp::new_ephemeral();
        let stranger = "peer@9007495404254172154";
        let shown = app.user_name(stranger);
        assert!(
            !shown.contains("9007495404254172154"),
            "the id leaked onto the screen as `{shown}`"
        );
        assert!(
            shown.starts_with("Someone"),
            "an unnamed peer should read as somebody unnamed, got `{shown}`"
        );
        assert!(
            shown.contains("2154"),
            "two unnamed peers have to be distinguishable, got `{shown}`"
        );
    }

    // Phase: your_own_name_is_known_before_the_room_has_heard_of_you.
    {
        let mut app = ChatApp::new_ephemeral();
        app.me.name = "Quinten".to_string();
        app.users.clear();
        app.room_names.clear();
        assert_eq!(
            app.display_name(&app.me.id.clone()),
            "Quinten",
            "the name this process worked out for itself must be reachable",
        );
    }

    // Phase: the_person_reading_the_screen_is_shown_their_name.
    {
        let app = ChatApp::new_ephemeral();
        let me = app.me.id.clone();
        assert_eq!(app.user_name(&me), crate::identity::DEMO_NAME);
    }

    // Phase: people_sharing_a_name_are_numbered_as_the_room_numbers_them.
    {
        use ui::permissions::UserId;
        let mut app = ChatApp::new_ephemeral();
        let me = app.me.id.clone();
        let mut presence = PresenceCtx::default();
        presence.set_self_participant(UserId(me.clone()), "Sam");
        presence.set_display_name(UserId("a-peer".into()), "sam");
        presence.set_display_name(UserId("z-peer".into()), "Kim");
        app.adopt_room(&presence);
        assert_eq!(app.user_name("a-peer"), "sam (1)");
        assert_eq!(app.user_name(&me), "Sam (2)");
        assert_eq!(app.user_name("z-peer"), "Kim");
        let offered: Vec<String> = app.participants().into_iter().map(|p| p.name).collect();
        assert!(
            offered.contains(&"sam (1)".to_owned()),
            "the @ picker offers the numbered name, got {offered:?}"
        );

        presence.set_display_name(UserId("a-peer".into()), "Alex");
        app.adopt_room(&presence);
        assert_eq!(app.user_name("a-peer"), "Alex");
        assert_eq!(
            app.user_name(&me),
            "Sam",
            "the number goes once nobody shares it"
        );
    }
}

/// In the browser presence names a person's device while the call
/// counts the person: the roster lists them once, by the device's name.
#[test]
fn a_call_names_each_person_once_by_their_devices_name() {
    use ui::permissions::UserId;
    let mut app = ChatApp::new_ephemeral();
    let channel: Id = "voice".into();
    app.channels.push(crate::model::Channel {
        id: channel.clone(),
        category: String::new(),
        name: "voice".into(),
        topic: String::new(),
        kind: crate::model::ChannelKind::Voice,
        order: 0,
        unread: 0,
        mentions: 0,
        archived: false,
        conversation_with: None,
    });
    let mut presence = PresenceCtx::default();
    presence.set_display_name(UserId("crane-device".into()), "Clever Crane");
    presence.set_persons([(UserId("crane-device".into()), UserId("crane".into()))]);
    presence.set_caret(
        &ChatApp::voice_presence_key(&channel),
        UserId("crane-device".into()),
        0,
    );
    // The call itself counts the person, as a browser's roster does.
    app.voice_members.insert(
        channel.clone(),
        vec![crate::model::VoiceMember {
            user: "crane".into(),
            talking: false,
            muted: false,
            deafened: false,
            audio_only: true,
            sharing: false,
            ticket: None,
            video: None,
        }],
    );
    app.adopt_room(&presence);
    assert_eq!(app.voice_people(&channel), vec!["crane".to_owned()]);
    assert_eq!(app.user_name("crane"), "Clever Crane");
    let offered = app
        .participants()
        .into_iter()
        .filter(|p| p.name == "Clever Crane")
        .count();
    assert_eq!(offered, 1, "the @ picker offers them once");
}

/// A document whose chat record no longer reads: what a newer or broken
/// writer leaves behind.
fn unreadable(doc: &mut AutoCommit) {
    use automerge::transaction::Transactable as _;
    doc.put(automerge::ROOT, "channels", "not a keyed collection")
        .expect("the field is written");
}

#[test]
fn an_unreadable_record_keeps_the_window_that_last_read_whole() {
    let mut app = ChatApp::new_ephemeral();
    let mut state = ui::State::new();
    crate::testing::starter(&mut state.doc, "owner", "Owner", "OW");
    Widget::sync(&mut app, &state.doc, &state.root);
    let channels: Vec<String> = app.channels.iter().map(|c| c.id.clone()).collect();
    let messages: Vec<String> = app.messages.iter().map(|m| m.id.clone()).collect();
    assert!(!channels.is_empty(), "the starter room has a channel");

    unreadable(&mut state.doc);
    app.reload(&state.doc);

    assert!(app.chat_read.failed(), "the record does not read as a chat");
    assert_eq!(
        app.channels
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>(),
        channels,
        "the channels that last read whole stay, not an empty room"
    );
    assert_eq!(
        app.messages
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
        messages,
        "the messages that last read whole stay"
    );
}

#[test]
fn an_unreadable_record_announces_nobody_from_an_empty_roster() {
    let mut app = ChatApp::new(
        crate::identity::LocalIdentity::for_participant("person-1"),
        ChatRuntime::Unavailable,
        ContentStore::memory(),
    )
    .announcing();
    app.attach_framework(ui::framework::Dispatcher::detached(
        "so.tilo.chat",
        "person-1",
    ));
    let mut state = ui::State::new();
    crate::testing::starter(&mut state.doc, "owner", "Owner", "OW");
    unreadable(&mut state.doc);

    Widget::sync(&mut app, &state.doc, &state.root);
    app.land(&mut state.doc);

    assert!(
        app.last_dispatched().is_none(),
        "who is already in the room is not known from a record that does not read"
    );
    assert!(
        app.announce.is_some(),
        "the introduction waits for a record that reads"
    );
}
