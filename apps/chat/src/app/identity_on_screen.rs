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

/// A name the person chooses while the window is open is theirs at once
/// in it, and introduced to the room again, so the roster every member
/// reads names them by it; the room's older name does not take it back.
#[test]
fn a_rename_while_open_reaches_the_roster() {
    let _guard = tilo_test_env::lock();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ui::local_identity::install_source(None);
        }
    }
    let _restore = Restore;
    let named = |name: &str| {
        ui::local_identity::install_source(Some(std::sync::Arc::new(
            ui::local_identity::FixedIdentity(ui::local_identity::LocalIdentity {
                group_public_key: [7; 32],
                display_name: name.to_owned(),
                avatar_color: [0; 3],
            }),
        )));
    };
    named("Ada");
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
    let roster_name = |state: &ui::State| {
        crate::testing::users(&state.doc)
            .into_iter()
            .find(|user| user.id == "person-1")
            .map(|user| (user.name, user.initials))
    };
    assert_eq!(roster_name(&state), Some(("Ada".into(), "A".into())));

    named("Bea Lee");
    Widget::sync(&mut app, &state.doc, &state.root);
    app.land(&mut state.doc);
    assert_eq!(roster_name(&state), Some(("Bea Lee".into(), "BL".into())));
    Widget::sync(&mut app, &state.doc, &state.root);
    assert_eq!(app.display_name("person-1"), "Bea Lee");

    let mut presence = ui::presence::PresenceCtx::default();
    presence.set_self_participant(ui::permissions::UserId("person-1".into()), "Ada");
    app.adopt_own_participant(&presence);
    assert_eq!(app.me.name, "Bea Lee", "the room's older name");
    presence.set_self_participant(ui::permissions::UserId("person-1".into()), "Bea Lee");
    app.adopt_own_participant(&presence);
    assert_eq!(app.renamed_to, None, "the room gave the new name back");
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
