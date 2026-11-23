use super::*;
use tilo_actions::policy::MemberId;
use ui::widget::Widget;

/// A record that does not read is not an empty chat: nothing is derived from the read that
/// failed, and what last read whole stays.
#[test]
fn a_record_that_does_not_read_keeps_the_channels() {
    let mut app = ChatApp::new(
        crate::identity::LocalIdentity::for_member(&MemberId::person("person-1")),
        ChatRuntime::Unavailable,
        ContentStore::memory(),
    );
    app.attach_framework(ui::framework::Dispatcher::detached(
        "so.tilo.chat",
        &ui::tilo_resource::MemberId::person("person-1"),
    ));
    let mut state = ui::State::new();
    crate::testing::starter(&mut state.doc, "owner", "Owner");
    let mut broken = state.doc.clone();
    ui::tilo_resource::write_key(
        &mut broken,
        &automerge::ROOT,
        "read",
        &ui::tilo_resource::Atom("not a set of read marks".into()),
    )
    .expect("the field is written");
    assert!(<crate::chat::Chat as ui::tilo_resource::Resource>::read(&broken).is_err());

    app.reload(&state.doc);
    let channels = app.channels.len();
    assert!(channels > 0, "the starter holds a channel");
    app.dispatched.borrow_mut().clear();

    Widget::sync(&mut app, &broken, &state.root);
    assert!(
        app.last_dispatched().is_none(),
        "nothing is derived from a failed read"
    );
    assert_eq!(
        app.channels.len(),
        channels,
        "the channels that last read stay"
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
        id: if is_agent {
            MemberId::agent(id)
        } else {
            MemberId::person(id)
        },
        name: id.into(),
        initials: "U".into(),
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
    let mut workspace = crate::testing::Workspace::default();
    crate::testing::starter(&mut workspace, stranger, "You");
    crate::testing::say(
        &mut workspace,
        stranger,
        "c_chat",
        "Retained foreign message",
    );
    state.doc = workspace.chat.clone();
    app.attach_workspace(&workspace);
    app.sync(&state.doc, &state.root);
    assert_eq!(
        app.user(&MemberId::person(stranger))
            .expect("observe the actual foreign author in the production roster")
            .name,
        "You"
    );
    let shown = app.user_name(&MemberId::person(stranger));
    assert_ne!(
        shown, "You",
        "another person's stored name must not read as the current reader"
    );
    assert_eq!(
        app.display_name(&MemberId::person(stranger)),
        shown,
        "mentions and message headers name the same foreign person"
    );
    assert!(
        shown.contains("2154"),
        "ambiguous names must distinguish the actual member"
    );
    assert_eq!(app.user_name(&app.me.id), crate::identity::DEMO_NAME);
    assert_eq!(
        crate::testing::users()[0].name,
        "You",
        "presentation must not rewrite saved person data"
    );
}

tilo_test_env::scenario! {
    fn people_on_screen_are_named_honestly_and_you_by_your_name;

    phase roster_vocabulary_never_calls_an_agent_a_person;
    {
        let mut app = ChatApp::new_ephemeral();
        app.users = vec![user("u_me", false), user("u_scribe", true)];
        assert!(
            app.has_other_member(),
            "the agent is another member"
        );
        assert_eq!(app.roster_label(), "Members");

        app.users = vec![user("u_me", false), user("u_aria", false)];
        assert_eq!(app.roster_label(), "People");
    }

    phase an_unnamed_member_is_not_shown_their_routing_token;
    {
        let app = ChatApp::new_ephemeral();
        let stranger = "peer@9007495404254172154";
        let shown = app.user_name(&MemberId::person(stranger));
        assert!(
            !shown.contains("9007495404254172154"),
            "the id leaked onto the screen as `{shown}`"
        );
        assert!(
            shown.starts_with("Someone"),
            "an unnamed peer should read as somebody unnamed, got `{shown}`"
        );
        assert_eq!(
            shown,
            crate::identity::unnamed_label(&MemberId::person(stranger)),
            "an unnamed person reads as on every other surface, got `{shown}`"
        );
    }

    phase a_person_without_a_name_reads_their_own_lines_as_you;
    {
        ui::i18n::I18nManager::init("en-US");
        let mut app = ChatApp::new_ephemeral();
        let me = app.me.id.clone();
        app.me.name = crate::identity::unnamed_label(&me);
        app.users.clear();
        assert_eq!(
            app.user_name(&me),
            "You",
            "their own lines are theirs, never the placeholder others see"
        );
    }

    phase your_own_name_is_known_before_the_workspace_has_heard_of_you;
    {
        let mut app = ChatApp::new_ephemeral();
        app.me.name = "Ada".to_string();
        app.users.clear();
        app.workspace_names.clear();
        assert_eq!(
            app.display_name(&app.me.id.clone()),
            "Ada",
            "the name this process worked out for itself must be reachable",
        );
    }

    phase the_person_reading_the_screen_is_shown_their_name;
    {
        let app = ChatApp::new_ephemeral();
        let me = app.me.id.clone();
        assert_eq!(app.user_name(&me), crate::identity::DEMO_NAME);
    }

    phase people_sharing_a_name_both_go_by_it;
    // A name is only the name: two people called Sam are both "Sam" in every line, the @ list
    // included, told apart by their faces and colours.
    {
        use ui::permissions::MemberId;
        let mut app = ChatApp::new_ephemeral();
        let me = app.me.id.clone();
        let mut presence = PresenceCtx::default();
        presence.set_self_member(me.clone(), "Sam");
        presence.set_display_name(MemberId::person("a-peer"), "sam");
        presence.set_display_name(MemberId::person("z-peer"), "Kim");
        app.adopt_workspace(&presence);
        assert_eq!(app.user_name(&MemberId::person("a-peer")), "sam");
        assert_eq!(app.user_name(&me), "Sam");
        assert_eq!(app.user_name(&MemberId::person("z-peer")), "Kim");
        let offered: Vec<String> = app.members().into_iter().map(|(_, name)| name).collect();
        assert!(
            offered.contains(&"sam".to_owned()),
            "the @ picker offers the name itself, got {offered:?}"
        );
    }
}

/// The call and presence both count the person: the roster lists them once, by the name
/// presence gives them.
#[test]
fn a_call_names_each_person_once() {
    use ui::permissions::MemberId;
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
        direct: false,
        members: Vec::new(),
    });
    let mut presence = PresenceCtx::default();
    presence.set_display_name(MemberId::person("crane"), "Clever Crane");
    presence.set_caret(
        &ChatApp::voice_presence_key(&channel),
        MemberId::person("crane"),
        0,
    );
    // The call itself counts the person, as a browser's roster does.
    app.voice_members.insert(
        channel.clone(),
        vec![crate::model::VoiceMember {
            user: MemberId::person("crane"),
            name: None,
            service: None,
            talking: false,
            muted: false,
            deafened: false,
            audio_only: true,
            sharing: false,
            ticket: None,
            video: None,
        }],
    );
    app.adopt_workspace(&presence);
    assert_eq!(app.voice_people(&channel), vec![MemberId::person("crane")]);
    assert_eq!(app.user_name(&MemberId::person("crane")), "Clever Crane");
    let offered = app
        .members()
        .into_iter()
        .filter(|(_, name)| name == "Clever Crane")
        .count();
    assert_eq!(offered, 1, "the @ picker offers them once");
}

/// Someone who renamed themself is listed among Contacts by the name they
/// go by here now, the one presence carries for their person, not the one
/// the workspace's list was read with; once they are gone, by the workspace's.
#[test]
fn a_contact_goes_by_the_name_they_go_by_here_now() {
    use ui::permissions::MemberId;
    let mut app = ChatApp::new_ephemeral();
    app.users.push(crate::model::User {
        id: MemberId::person("ada-root"),
        name: "Swift Lynx".into(),
        initials: "SL".into(),
        status: crate::model::Presence::default(),
        is_agent: false,
    });
    let mut presence = PresenceCtx::default();
    presence.set_display_name(MemberId::person("ada-root"), "Ada");
    app.adopt_workspace(&presence);
    assert!(
        app.display_name(&MemberId::person("ada-root"))
            .contains("Ada"),
        "the name they go by here now: {:?}",
        app.display_name(&MemberId::person("ada-root"))
    );

    app.adopt_workspace(&PresenceCtx::default());
    assert!(
        app.display_name(&MemberId::person("ada-root"))
            .contains("Swift Lynx"),
        "gone: the workspace's name: {:?}",
        app.display_name(&MemberId::person("ada-root"))
    );
}

/// An agent is a member like a person: its answer reads by the name the workspace's member list
/// gives it — whether or not its device works here — and the conversation with it is named for
/// it. Never by a placeholder that spells part of its routing id.
#[test]
fn an_agents_answer_reads_by_the_name_the_member_list_gives_it() {
    let mut doc = crate::testing::Workspace::default();
    crate::testing::starter(&mut doc, crate::model::ME, "You");
    let mut app = ChatApp::new_ephemeral();
    app.attach_workspace(&doc);
    // The member list names it; its device never worked here.
    crate::testing::agent_member(&mut app, "notes", "Notes");
    app.reload(&doc);
    let conversation = crate::chat::Chat::direct_id([
        MemberId::person(crate::model::ME),
        MemberId::agent("notes"),
    ]);
    let replied = doc
        .say(
            &MemberId::agent("notes"),
            conversation.as_str(),
            "Here it is.",
        )
        .unwrap();
    app.reload(&doc);

    let answer = doc.message_row(&replied).expect("the agent's answer");
    assert_eq!(app.display_name(&MemberId::agent("notes")), "Notes");
    assert_eq!(app.user_name(&MemberId::agent("notes")), "Notes");
    assert_eq!(app.message_author_name(&answer), "Notes");
    assert!(
        matches!(
            app.message_author(&answer, "Notes", None),
            ui::author::Author::Agent { .. }
        ),
        "an agent's answer is drawn as an agent, not as a person"
    );
    assert!(
        !crate::model_stream::is_provider_output(&answer),
        "an agent's own answer is the agent's, not a model's output"
    );

    // The conversation reads by its name, as the bar and the card's header name it.
    let read = crate::projection::read(&doc);
    assert_eq!(
        read.display_names(
            &conversation,
            Some(&MemberId::person(crate::model::ME)),
            &app.workspace_names
        ),
        "Notes"
    );

    // An agent wears the one colour of who it is, as its cursor and caret do in every app.
    assert_eq!(
        app.member_hue(&MemberId::agent("notes")),
        ui::tokens::PresenceHue::of_member(&MemberId::agent("notes"))
    );

    // The sidebar lists the conversation once, by the agent's name and wearing its own face in
    // the colour it wears everywhere in the workspace.
    app.show_sidebar();
    let (sections, _) = app.places.shown().expect("the sidebar is shown");
    let rows: Vec<_> = sections
        .last()
        .expect("the direct messages")
        .items
        .iter()
        .filter(|item| item.label == "Notes")
        .collect();
    assert_eq!(rows.len(), 1, "one row for the one conversation");
    let face = rows[0]
        .person
        .as_ref()
        .expect("the row wears the agent's face");
    assert_eq!(
        face.who,
        MemberId::agent("notes").into(),
        "an agent's face, never a person's"
    );
    assert_eq!(
        face.colour,
        Some(app.identity_colour(&MemberId::agent("notes")))
    );

    // Open, the conversation's bar leads with the same face.
    app.active_channel = conversation.to_string();
    let (_, bar) = app.location_lead();
    let bar = bar.expect("the bar wears the agent's face");
    assert!(bar.agent);
    assert_eq!(
        (bar.name.as_str(), bar.identity.as_str()),
        ("Notes", "notes")
    );

    // An agent the resource names not is never shown a part of its routing id either.
    assert_eq!(
        app.user_name(&MemberId::agent("stranger")),
        crate::identity::unnamed_label(&MemberId::agent("stranger"))
    );
}

/// An app's code answers where no window heard a member list: the members the device lends it
/// name the people it reads, as a window names them.
#[test]
fn an_apps_code_names_a_member_by_the_members_the_device_lends_it() {
    let mut doc = crate::testing::Workspace::default();
    crate::testing::starter(&mut doc, crate::model::ME, "You");
    let mut app = ChatApp::new_ephemeral();
    app.attach_workspace(&doc);
    app.users.clear();
    app.workspace_names.clear();
    let rae = MemberId::person("r".repeat(32));
    ui::tilo_resource::members::know([(rae.clone(), " Rae ".to_owned())]);
    assert_eq!(app.display_name(&rae), "Rae");
    assert_eq!(app.user_name(&rae), "Rae");
    ui::tilo_resource::members::know([]);
}
