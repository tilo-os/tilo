use super::*;

fn member(publisher: &str, kind: &str, label: &str) -> LiveMember {
    LiveMember {
        publisher: publisher.into(),
        kind: kind.into(),
        label: label.into(),
        device: String::new(),
        focus: None,
        activity: (kind == "agent").then(|| "Reading the notes".into()),
        automatic: false,
        place: None,
        user: None,
        member: None,
        wave: None,
        layout: None,
        following: None,
        availability: None,
        call: None,
        colour: String::new(),
    }
}

tilo_test_env::scenario! {
    fn an_agent_here_is_met_once;

    phase an_mcp_agent_session_is_present_as_an_agent_and_met_once;
    // An MCP agent connected for this page's person is in the workspace's presence as an agent:
    // listed after the people, in its client's hue, saying what it does now, and naming the app
    // of the record it acts in, which following it opens. It is met once: the page follows it and
    // opens its card the first time only, and someone else's agent is never theirs to be shown
    // into.
    {
        let mut here = HereState::default();
        let own = LiveMember {
            user: Some("aa".into()),
            ..member("me", "person", "Pat")
        };
        let claude = LiveMember {
            focus: Some(Focus {
                app: "so.tilo.notes".into(),
                document: "0123456789abcdef".into(),
            }),
            activity: Some("Editing notes/Welcome".into()),
            user: Some("aa".into()),
            ..member("claude", "agent", "Claude Code")
        };
        here.set_members(
            Some(own.clone()),
            Some(vec![
                own,
                member("ada", "person", "Ada"),
                claude,
                LiveMember {
                    user: Some("bb".into()),
                    ..member("theirs", "agent", "Codex")
                },
            ]),
        );
        let people = here.people(&Layout::default(), &[], &|_| None);
        let ids: Vec<&str> = people.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["ada", "claude", "theirs"],
            "people first, then agents"
        );
        let agent = &people[1];
        assert!(
            agent.agent,
            "an MCP session is an agent, not this person's own device"
        );
        assert_eq!(
            agent.rgb,
            tilo_ui_base::presence::identity_rgb(tilo_ui_base::tokens::PresenceHue::Orange),
            "Claude Code wears its client's hue"
        );
        assert_eq!(
            agent.status.as_ref().map(|line| line.text.as_str()),
            Some("Editing notes/Welcome")
        );
        assert_eq!(agent.focus_app.as_deref(), Some("so.tilo.notes"));
        assert_eq!(
            here.take_own_agents_arrived(),
            [("claude".to_owned(), "Claude Code".to_owned())],
            "only this person's own agent is met"
        );
        assert!(here.take_own_agents_arrived().is_empty(), "met once");
    }
}

tilo_test_env::scenario! {
    fn people_are_named_and_placed_as_their_presence_says;

    phase a_person_is_named_by_their_pages_presence;
    // A person's screen is their device's layout; an agent has none, even when its frame says one.
    {
        let mut here = HereState::default();
        let device = |publisher: &str, user: &str, label: &str| LiveMember {
            user: Some(user.into()),
            ..member(publisher, "person", label)
        };
        here.set_members(
            Some(device("pat-laptop", "pat-root", "Pat")),
            Some(vec![
                device("ada-device", "ada-root", "Ada"),
                device("cy-device", "cy-root", ""),
                device("pat-phone", "pat-root", "Pat"),
            ]),
        );
        // The apps are told every person once, the page's own too: Pat's window calls Pat's
        // messages Pat's, as everyone else's window does.
        let told = here.persons().into_iter().map(|(person, name, _)| (person, name));
        let pat = ("pat-root".to_owned(), "Pat".to_owned());
        assert_eq!(told.collect::<Vec<_>>()[..2], [pat, ("ada-root".into(), "Ada".into())]);
        assert_eq!(
            here.person("ada-root"),
            Some(("Ada".to_owned(), here.colors()["ada-device"])),
            "named and coloured as the bar draws her"
        );
        assert_eq!(
            here.person("cy-root").map(|(name, _)| name),
            Some(unnamed_person_label())
        );
        assert_eq!(here.person("someone-who-left"), None);
    }

    phase where_someone_is_never_repeats_their_status;
    // Where someone is says only what their status line does not: no strip in a workspace of one,
    // and no app their status already names.
    {
        use tilo_desktop_model::{Strip, StripId, Tab, WindowGroup, Workspace, WorkspaceId};
        let chat = Tab {
            id: 7,
            title: "chat".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.chat".into(),
                label: "Chat".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let mut main = Strip::new(StripId(1), "Main");
        main.window_groups.push(WindowGroup {
            id: 8,
            active_tab: chat.id,
            tabs: vec![chat],
            width: tilo_desktop_model::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Design");
        workspace.strips = vec![main];
        let mut tree = Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Layout::default()
        };
        let app = |id: &str| (id == "so.tilo.chat").then(|| "Chat".to_owned());
        let status = |text: &str| Line {
            text: text.into(),
            source: crate::desktop::people::LineSource::Observed,
        };
        let place =
            |tree: &Layout, line: Option<&Line>| worded_place(tree, 7, None, &app, line);
        assert_eq!(
            place(&tree, None),
            Some("Chat".into()),
            "one strip is no news"
        );
        assert_eq!(
            place(&tree, Some(&status("Working in Chat"))),
            None,
            "the status already says Chat"
        );
        assert_eq!(
            place(&tree, Some(&status("Working on Welcome in Notes"))),
            Some("Chat".into())
        );
        tree.workspaces[0]
            .strips
            .push(Strip::new(StripId(2), "Second"));
        assert_eq!(place(&tree, None), Some("Main · Chat".into()));
        assert_eq!(
            place(&tree, Some(&status("Working in chat"))),
            Some("Main".into()),
            "whatever the case"
        );
        assert!(!names("Chatting", "Chat"), "a word, not part of one");
    }
}

tilo_test_env::scenario! {
    fn everyone_here_wears_the_colour_of_who_they_are;

    phase an_agent_wears_one_colour_on_every_surface;
    {
        let helper = "agent:helper@host";
        let (r, g, b) = tilo_ui_base::presence::member_color(&tilo_ui_base::permissions::MemberId::agent(helper, "bo-root"));
        let announced = format!("#{r:02x}{g:02x}{b:02x}");
        let me = LiveMember {
            user: Some("me-root".into()),
            ..member("me", "person", "Pat")
        };
        // The agent's own session publishes its colour with its face, as every session does.
        let agent = LiveMember {
            user: Some("helper-root".into()),
            colour: announced,
            ..member(helper, "agent", "Helper #1")
        };
        let mut here = HereState::default();
        here.set_members(Some(me), Some(vec![agent]));

        let tree = Layout::default();
        let people = here.people(&tree, &[], &|_| None);
        let listed = people
            .iter()
            .find(|person| person.id == helper)
            .expect("the agent is listed");
        let mut presence = PresenceCtx::empty();
        here.register_windows(&people, &mut presence);
        assert_eq!(
            (
                here.colors().get(helper).copied(),
                listed.rgb,
                presence.color_for(&MemberId::agent("helper@host", "bo-root")),
                tilo_ui_base::presence::member_color(&tilo_ui_base::permissions::MemberId::agent(helper, "bo-root")),
            ),
            (Some((r, g, b)), (r, g, b), (r, g, b), (r, g, b)),
            "the board, its people row, the windows and every app settle one colour"
        );
    }

    phase a_person_wears_the_colour_they_chose;
    // The colour someone chose in their own card is the colour they wear here,
    // on their own page and on everyone else's: one person, one colour, on the
    // bar, in the People list and in every app they edit in.
    {
        let violet = tilo_ui_base::presence::identity_rgb(tilo_ui_base::tokens::PresenceHue::Violet);
        let hex = format!("#{:02x}{:02x}{:02x}", violet.0, violet.1, violet.2);
        let claiming = |device: &str, root: &str| LiveMember {
            user: Some(root.into()),
            colour: hex.clone(),
            ..member(device, "person", device)
        };
        let quiet = |device: &str, root: &str| LiveMember {
            user: Some(root.into()),
            ..member(device, "person", device)
        };
        let mut own_page = HereState::default();
        own_page.set_members(
            Some(claiming("me", "me-root")),
            Some(vec![quiet("bo", "bo-root")]),
        );
        assert_eq!(own_page.colors()["me"], violet, "they wear what they chose");
        assert_eq!(own_page.own_rgb(), Some(violet), "and see themselves in it");
        let mut peer_page = HereState::default();
        peer_page.set_members(
            Some(quiet("bo", "bo-root")),
            Some(vec![claiming("me", "me-root")]),
        );
        assert_eq!(
            peer_page.colors()["me"],
            violet,
            "every page paints them in it too"
        );
    }

    phase a_face_is_neutral_until_the_workspace_knows_whose_it_is;
    // A device live here before its certificate names its person is drawn grey, and its windows
    // get no colour, until the person is known.
    {
        use ui::ColorCompat as _;
        let me = LiveMember {
            user: Some("me-root".into()),
            ..member("me", "person", "Quinn")
        };
        let mut here = HereState::default();
        here.set_members(
            Some(me.clone()),
            Some(vec![me, member("ben-tab", "person", "Ben")]),
        );
        let tree = Layout::default();
        let shown = |here: &mut HereState| {
            let people = here.people(&tree, &[], &|_| None);
            let mut presence = PresenceCtx::empty();
            here.register_windows(&people, &mut presence);
            (
                people[0].rgb,
                presence.color_overrides.get(&MemberId::person("ben-root")).copied(),
            )
        };
        assert_eq!(
            shown(&mut here),
            (tilo_ui_base::presence::UNSETTLED_RGB, None),
            "not yet known whose device it is"
        );
        assert!(!here.colors().contains_key("ben-tab"));
        assert_eq!(
            here.own_rgb(),
            Some(tilo_ui_base::presence::user_color("me-root")),
            "this page knows its own person"
        );
        here.set_members(
            here.own.clone(),
            Some(vec![
                here.own.clone().expect("this page"),
                LiveMember {
                    user: Some("ben-root".into()),
                    ..member("ben-tab", "person", "Ben")
                },
            ]),
        );
        let ben = tilo_ui_base::presence::user_color("ben-root");
        assert_eq!(shown(&mut here), (ben, Some(ben)));
        // A neutral face paints grey, not a ring colour.
        let palette = tilo_ui_base::tokens::colors::Colors::dark().presence;
        let painted = palette.of_rgb(tilo_ui_base::presence::UNSETTLED_RGB);
        assert!(painted.r().abs_diff(painted.g()) <= 1, "{painted:?}");
        assert!(painted.g().abs_diff(painted.b()) <= 1, "{painted:?}");
    }

    phase a_persons_devices_share_their_colour_in_presence;
    // A person's devices wear one colour (the person's, once each device's certificate names
    // them), and the presence every window draws from learns it for the person.
    {
        let mut here = HereState::default();
        here.set_members(
            Some(member("me", "person", "Me")),
            Some(
                ["ada-laptop", "ada-phone"]
                    .map(|device| LiveMember {
                        user: Some("ada-root".into()),
                        ..member(device, "person", "Ada")
                    })
                    .to_vec(),
            ),
        );
        let colors = here.colors().clone();
        assert_eq!(colors["ada-laptop"], colors["ada-phone"]);
        let tree = Layout::default();
        let people = here.people(&tree, &[], &|_| None);
        let mut presence = PresenceCtx::empty();
        here.register_windows(&people, &mut presence);
        assert_eq!(
            presence.color_for(&MemberId::person("ada-root")),
            colors["ada-laptop"]
        );
    }
}

tilo_test_env::scenario! {
    fn the_roster_says_who_is_here_and_where_they_are;

    phase their_own_words_and_their_devices_reading_are_told_apart;
    // What someone is doing is the one line their device publishes, marked by who wrote it: their
    // own words, or what their device read off their steps, so nothing automatic passes for their
    // words.
    {
        use crate::desktop::people::LineSource;
        let mut here = HereState::default();
        let mut ada = member("ada", "person", "Ada");
        let mut cy = member("cy", "person", "Cy");
        ada.activity = Some("  Reviewing the launch plan ".into());
        cy.activity = Some("Working in Chat".into());
        cy.automatic = true;
        here.set_members(Some(member("me", "person", "Me")), Some(vec![ada, cy]));
        let people = here.people(&Layout::default(), &[], &|_| None);
        let said: Vec<(&str, LineSource)> = people
            .iter()
            .filter_map(|person| person.status.as_ref())
            .map(|line| (line.text.as_str(), line.source))
            .collect();
        assert_eq!(
            said,
            [
                ("Reviewing the launch plan", LineSource::Written),
                ("Working in Chat", LineSource::Observed),
            ]
        );
        assert!(!people[0].status.as_ref().is_some_and(Line::automatic));
        assert!(people[1].status.as_ref().is_some_and(Line::automatic));
    }

    phase this_persons_own_words_lead_unless_they_share_nothing;
    // This person's own words are what their page publishes and what
    // everyone reads of them, unless they share nothing; without words,
    // what their device read off their steps, only while they share it.
    {
        use crate::desktop::people::LineSource;
        let mut here = HereState::default();
        here.set_members(Some(member("me", "person", "Me")), Some(Vec::new()));
        here.set_own_reading(Some("Working in Notes".into()));
        assert_eq!(
            here.own_doing().map(|line| (line.text, line.source)),
            Some(("Working in Notes".into(), LineSource::Observed))
        );
        here.set_own_words(Some("  Fixing\nthe onboarding   copy ".into()));
        assert_eq!(here.shared_words(), Some("Fixing the onboarding copy"));
        assert_eq!(
            here.own_doing().map(|line| (line.text, line.source)),
            Some(("Fixing the onboarding copy".into(), LineSource::Written))
        );
        here.set_sharing(ui::StatusSharing::ManualOnly);
        assert_eq!(here.shared_words(), Some("Fixing the onboarding copy"));
        here.set_sharing(ui::StatusSharing::Off);
        assert_eq!(here.shared_words(), None, "sharing nothing hides them");
        assert_eq!(here.own_doing(), None);
        assert_eq!(here.own_words(), Some("Fixing the onboarding copy"));
        here.set_sharing(ui::StatusSharing::Automatic);
        here.set_own_words(Some("   ".into()));
        assert_eq!(here.own_words(), None, "only spaces clear them");
        assert_eq!(
            here.own_doing().map(|line| line.text),
            Some("Working in Notes".into()),
            "their device's reading again"
        );
        here.set_sharing(ui::StatusSharing::ManualOnly);
        assert_eq!(here.own_doing(), None, "a reading only while they share it");
        here.set_sharing(ui::StatusSharing::Automatic);
        here.set_own_words(Some("x".repeat(ui::MAX_STATUS_CHARS + 20)));
        assert_eq!(
            here.own_words().map(|words| words.chars().count()),
            Some(ui::MAX_STATUS_CHARS)
        );
        here.set_own_words(Some("é".repeat(80)));
        assert_eq!(
            here.own_words().map(str::len),
            Some(96),
            "as many bytes as their presence carries"
        );
    }

    phase people_are_the_live_members_and_leave_out_this_page;
    {
        let mut here = HereState::default();
        here.set_members(
            Some(member("me", "person", "Me")),
            Some(vec![
                member("bot", "agent", "Scribe"),
                member("me", "person", "Me"),
                LiveMember {
                    device: "Framework 16".into(),
                    ..member("ann", "person", "")
                },
            ]),
        );
        let tree = Layout::default();
        let people = here.people(&tree, &[], &|_| None);
        let ids: Vec<_> = people.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["ann", "bot"],
            "people first, agents after, never this page"
        );
        assert_eq!(
            people[0].label,
            unnamed_person_label(),
            "a person who has not named themselves is unnamed, never their device's name"
        );
        assert!(people[1].agent);
        assert_eq!(
            people[1].status.as_ref().map(|l| l.text.as_str()),
            Some("Reading the notes"),
            "an agent's line is its own activity"
        );
    }

    phase one_agent_is_one_row_whichever_way_it_is_heard;
    // One agent is one row, however it reaches this session: its live presence lists it with its
    // colour and what it says it is doing, the person's own agent included; the same agent heard
    // through a second publisher is not drawn twice; a second session of the client stays its own
    // row.
    {
        let tree = Layout::default();
        let live_agent = |publisher: &str, label: &str| LiveMember {
            user: Some("ann-root".into()),
            activity: Some("Looking at the apps".into()),
            ..member(publisher, "agent", label)
        };
        let rows = |here: &HereState, label: &str| {
            here.people(&tree, &[], &|_| None)
                .into_iter()
                .filter(|person| person.label == label)
                .collect::<Vec<_>>()
        };
        let me = LiveMember {
            user: Some("ann-root".into()),
            ..member("me", "person", "Me")
        };

        // The person's own agent, heard on the live layer.
        let mut here = HereState::default();
        here.set_members(
            Some(me.clone()),
            Some(vec![
                member("bob", "person", "Bob"),
                live_agent("claude", "Claude Code"),
            ]),
        );
        let listed = rows(&here, "Claude Code");
        assert_eq!(listed.len(), 1, "one row for one agent: {listed:?}");
        assert!(listed[0].agent, "never one of the person's devices");
        assert_eq!(
            listed[0].rgb,
            tilo_ui_base::presence::identity_rgb(tilo_ui_base::tokens::PresenceHue::Orange),
            "in its client's colour"
        );
        assert_eq!(
            listed[0]
                .status
                .as_ref()
                .map(|line| (line.text.as_str(), line.source)),
            Some(("Looking at the apps", crate::desktop::people::LineSource::Written)),
            "its activity line, as its own words"
        );

        // The same agent through a second publisher, and a second session of the client.
        here.set_members(
            Some(me),
            Some(vec![
                live_agent("claude", "Claude Code"),
                live_agent("claude-presence", "Claude Code"),
                live_agent("claude-2", "Claude Code #2"),
            ]),
        );
        assert_eq!(
            rows(&here, "Claude Code").len(),
            1,
            "one agent heard twice is one row"
        );
        assert_eq!(
            rows(&here, "Claude Code #2").len(),
            1,
            "a second session is its own row"
        );
    }

    phase a_members_certificate_pairs_their_devices;
    // A member's own certificate names the person it is a device of: their devices count as one
    // person.
    {
        let mut here = HereState::default();
        let certified = |publisher: &str, label: &str| LiveMember {
            user: Some("ann-root".into()),
            ..member(publisher, "person", label)
        };
        here.set_members(
            Some(member("me", "person", "Me")),
            Some(vec![
                certified("laptop", "Framework 16"),
                certified("phone", "Phone"),
            ]),
        );
        let people = here.people(&Layout::default(), &[], &|_| None);
        assert_eq!(people.len(), 1, "two devices of one person are one face");
        assert_eq!(people[0].user.as_deref(), Some("ann-root"));
        assert_eq!(people[0].label, "Framework 16");
        let colors = here.colors();
        assert_eq!(
            colors["laptop"], colors["phone"],
            "a person's devices share their person's colour"
        );
    }

    phase someone_whose_window_is_not_here_is_in_their_app;
    // In a workspace whose layout is personal nobody's window is on this desktop: a person's row
    // still says where they are, from the app their presence focuses, unless their status already
    // names it.
    {
        let mut here = HereState::default();
        let working = |publisher: &str, app: &str| LiveMember {
            focus: Some(Focus {
                app: app.into(),
                document: "welcome".into(),
            }),
            activity: None,
            ..member(publisher, "person", publisher)
        };
        let mut ben = working("ben", "so.tilo.notes");
        ben.activity = Some("Drafting Welcome in Notes".into());
        ben.automatic = true;
        here.set_members(
            None,
            Some(vec![
                working("ann", "so.tilo.notes"),
                ben,
                working("cy", "so.tilo.unknown"),
            ]),
        );
        let app = |id: &str| (id == "so.tilo.notes").then(|| "Notes".to_owned());
        let people = here.people(&Layout::default(), &[], &app);
        let place = |id: &str| {
            people
                .iter()
                .find(|person| person.id == id)
                .and_then(|person| person.place.clone())
        };
        assert_eq!(place("ann"), Some("In Notes".into()));
        assert_eq!(place("ben"), None, "the status already says Notes");
        assert_eq!(place("cy"), None, "an app this page cannot name");
    }

    phase a_focus_naming_a_window_here_puts_them_in_that_window;
    // A member's focus names the record its window shows by that record's short digest, as
    // presence names it: the host that holds the same window knows which window they are in.
    {
        use tilo_desktop_model::{
            Strip, StripId, Tab, WindowGroup, WindowWidth, Workspace, WorkspaceId,
        };
        let digest = "0123456789abcdef";
        let welcome = Tab {
            id: 7,
            title: "Welcome".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let mut strip = Strip::new(StripId(1), "Main");
        strip.window_groups.push(WindowGroup {
            id: 8,
            active_tab: welcome.id,
            tabs: vec![welcome],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Design");
        workspace.strips = vec![strip];
        let tree = Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Layout::default()
        };
        let mut here = HereState::default();
        here.set_members(
            None,
            Some(vec![LiveMember {
                focus: Some(Focus {
                    app: "so.tilo.notes".into(),
                    document: digest.into(),
                }),
                activity: None,
                ..member("ann", "person", "Ann")
            }]),
        );
        let app = |id: &str| (id == "so.tilo.notes").then(|| "Notes".to_owned());
        let held = [(7, "so.tilo.notes".to_owned(), digest.to_owned())];
        let people = here.people(&tree, &held, &app);
        let ann = people
            .iter()
            .find(|person| person.id == "ann")
            .expect("ann is here");
        assert_eq!(ann.tab, Some(7), "the window her focus names");
        assert_eq!(ann.place.as_deref(), Some("Notes"));
    }
}

tilo_test_env::scenario! {
    fn a_persons_devices_are_one_person_and_presence_is_placed;

    phase people_sharing_a_name_both_go_by_it_on_every_page;
    // Two people going by one name are both called it, on every page and in the roster: their
    // faces and colours tell them apart, never a number joined to the name.
    {
        let mut workspace = Workspace::new(tilo_desktop_model::WorkspaceId::from_u128(1), "Studio");
        for (person, admitted) in [("person-z", 10_u64), ("person-a", 20)] {
            workspace
                .policy
                .members
                .insert(person.into(), tilo_action_policy::Role::Editor.into());
            workspace.policy.joined.insert(person.into(), admitted);
            workspace.member_here(Member::named(collab_types::MemberId::person(person), "Alex"));
        }
        let tree = Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Layout::default()
        };
        let alex = |publisher: &str, person: &str| LiveMember {
            user: Some(person.into()),
            ..member(publisher, "person", "Alex")
        };
        let mut mine = HereState::default();
        mine.set_members(Some(alex("dev-a", "person-a")), Some(vec![alex("dev-z", "person-z")]));
        let seen: HashMap<String, String> = mine
            .people(&tree, &[], &|_| None)
            .into_iter()
            .map(|person| (person.user.expect("certified"), person.label))
            .collect();
        assert_eq!(seen["person-z"], "Alex", "as everyone here calls them");
        assert_eq!(mine.own_shown().as_deref(), Some("Alex"));
    }

    phase a_persons_devices_are_one_person;
    // Two tabs of one person are two devices of theirs: listed once to everyone else, and to
    // either tab never as someone else.
    {
        let on = |publisher: &str, place: &str| LiveMember {
            place: Some(place.into()),
            user: Some(
                if publisher == "ben-tab" {
                    "ben-root"
                } else {
                    "ada-root"
                }
                .into(),
            ),
            ..member(publisher, "person", "Ada")
        };
        let members = vec![
            on("ada-tab-1", "10"),
            on("ada-tab-2", "11"),
            on("ben-tab", "12"),
            member("bot", "agent", "Scribe"),
        ];
        let tree = Layout::default();

        // Ben sees Ada once, in the order she arrived.
        let mut ben = HereState::default();
        ben.set_members(
            Some(LiveMember {
                user: Some("ben-root".into()),
                ..on("ben-tab", "12")
            }),
            Some(members.clone()),
        );
        let ids: Vec<_> = ben
            .people(&tree, &[], &|_| None)
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["ada-tab-1", "bot"]);

        // Ada's second tab: her first tab is her, not someone else.
        let mut ada = HereState::default();
        ada.set_members(
            Some(LiveMember {
                user: Some("ada-root".into()),
                ..on("ada-tab-2", "11")
            }),
            Some(members),
        );
        let ids: Vec<_> = ada
            .people(&tree, &[], &|_| None)
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["ben-tab", "bot"]);
    }

    phase a_persons_devices_are_listed_under_their_name_and_never_name_them;
    // A person's devices are listed under their name, each as it calls itself and as it said it
    // was: what the card's device row and a pointer over a tile read.
    {
        let mut here = HereState::default();
        here.set_members(
            None,
            Some(vec![
                LiveMember {
                    device: "Framework 16".into(),
                    availability: Some("available".into()),
                    user: Some("ada-root".into()),
                    ..member("ada-laptop", "person", "Ada")
                },
                LiveMember {
                    device: "ThinkPad".into(),
                    availability: Some("focusing".into()),
                    user: Some("ada-root".into()),
                    ..member("ada-handheld", "person", "Ada")
                },
                LiveMember {
                    device: "Mac".into(),
                    user: Some("ben-root".into()),
                    ..member("ben", "person", "Ben")
                },
            ]),
        );
        let people = here.people(&Layout::default(), &[], &|_| None);
        let ada = people
            .iter()
            .find(|person| person.id == "ada-laptop")
            .expect("Ada, listed once");
        assert_eq!(ada.label, "Ada");
        let devices: Vec<_> = ada
            .devices
            .iter()
            .map(|device| (device.label.as_str(), device.availability))
            .collect();
        assert_eq!(
            devices,
            [
                ("Framework 16", ui::Availability::Available),
                ("ThinkPad", ui::Availability::Focusing),
            ],
            "each device as it calls itself, with how it said it was"
        );

        let names = here.person_names();
        assert_eq!(
            names.get("ada-laptop").map(|(name, _)| name.as_str()),
            Some("Ada")
        );
        assert_eq!(
            names.get("ada-handheld").map(|(name, _)| name.as_str()),
            Some("Ada"),
            "the same person, on either device, by one name"
        );
    }

    phase placing_presence_marks_whom_this_shell_follows;
    // Whose view this session follows is said on their row and no other's, each frame: the bar keeps
    // that face when it has room for only one.
    {
        let mut tree = Layout::default();
        let workspace = tilo_desktop_model::WorkspaceId::nil();
        tree.workspaces
            .push(tilo_desktop_model::Workspace::new(workspace, "Design"));
        tree.active = workspace;
        let people: Vec<Person> = ["ada", "cc"]
            .into_iter()
            .map(|id| Person {
                id: id.into(),
                user: None,
                member: (id == "cc").then(|| MemberId::agent("cc", "bo-root")),
                label: id.into(),
                devices: Vec::new(),
                rgb: (1, 2, 3),
                agent: id == "cc",
                tab: None,
                focus_app: None,
                place: None,
                status: None,
                availability: ui::Availability::Available,
                call: None,
            })
            .collect();
        place_here(&mut tree, &people, Some("cc"));
        let following: Vec<String> = tree
            .active_workspace()
            .expect("a workspace")
            .present
            .iter()
            .filter(|here| here.followed)
            .map(|here| here.device.clone())
            .collect();
        assert_eq!(following, ["cc"], "the followed one, and only they");
        // Stopping says so on every row, not just the one that was followed.
        place_here(&mut tree, &people, None);
        assert!(
            tree.active_workspace()
                .expect("a workspace")
                .present
                .iter()
                .all(|here| !here.followed),
            "nobody is followed"
        );
    }

    phase placing_presence_carries_what_they_are_doing;
    // What someone says they are doing reaches their row each frame, both on a row this page
    // already holds and on one it writes now: an agent whose work the bar shows is never a name
    // with nothing under it.
    {
        let mut tree = Layout::default();
        let workspace = tilo_desktop_model::WorkspaceId::nil();
        tree.workspaces
            .push(tilo_desktop_model::Workspace::new(workspace, "Design"));
        tree.active = workspace;
        let person = |status: Option<Line>| Person {
            id: "cc".into(),
            user: None,
            member: Some(MemberId::agent("cc", "bo-root")),
            label: "Claude Code".into(),
            devices: Vec::new(),
            rgb: (1, 2, 3),
            agent: true,
            tab: None,
            focus_app: None,
            place: None,
            status,
            availability: ui::Availability::Available,
            call: None,
        };
        let said = Line {
            text: "  Writing in Launch notes ".into(),
            source: crate::desktop::people::LineSource::Observed,
        };
        place_here(&mut tree, &[person(Some(said.clone()))], None);
        let row = tree
            .active_workspace()
            .and_then(|workspace| workspace.present.first())
            .expect("overlaid");
        assert_eq!(
            row.status.as_ref().map(|status| status.text.as_str()),
            Some("Writing in Launch notes"),
            "their words, trimmed"
        );
        assert!(
            row.status.as_ref().is_some_and(|status| status.automatic),
            "read off what it did, never typed by it"
        );
        // The same row again, with the line it says a moment later.
        place_here(&mut tree, &[person(Some(said))], None);
        // A line of nothing but spaces says nothing, and neither does a row nothing said of.
        let blank = Line {
            text: "   ".into(),
            source: crate::desktop::people::LineSource::Written,
        };
        place_here(&mut tree, &[person(Some(blank))], None);
        place_here(&mut tree, &[person(None)], None);
        let row = tree
            .active_workspace()
            .and_then(|workspace| workspace.present.first())
            .expect("still overlaid");
        assert_eq!(row.status, None, "nothing said of it now");
    }

    phase an_agents_row_is_software;
    // An agent presence names is software on its row from the first frame: its member key says
    // so, never a marker a later frame has to correct.
    {
        let mut tree = Layout::default();
        let workspace_id = tilo_desktop_model::WorkspaceId::nil();
        tree.workspaces
            .push(tilo_desktop_model::Workspace::new(workspace_id, "Design"));
        tree.active = workspace_id;
        let people = vec![Person {
            id: "bot".into(),
            user: None,
            member: Some(MemberId::agent("scribe", "bo-root")),
            label: "Scribe".into(),
            devices: Vec::new(),
            rgb: (1, 2, 3),
            agent: true,
            tab: None,
            focus_app: None,
            place: None,
            status: None,
            availability: ui::Availability::Available,
            call: None,
        }];
        place_here(&mut tree, &people, None);
        let workspace = tree.active_workspace().expect("a workspace");
        let agent = workspace
            .members
            .iter()
            .find(|member| member.id == collab_types::MemberId::agent("scribe", "bo-root"))
            .expect("the agent is on the member list");
        assert!(!agent.id.is_person(), "an agent's row is software's");
    }
}

/// A file's version names the person whose device signed it and the device
/// whose session vouched for the writer: both resolve as presence shows them,
/// this page's own person too.
#[test]
fn a_persons_devices_and_name_resolve_as_presence_shows_them() {
    let mut here = HereState::default();
    let own = LiveMember {
        user: Some("aa".into()),
        ..member("me", "person", "Pat")
    };
    here.set_members(
        Some(own),
        Some(vec![LiveMember {
            device: "ada-phone".into(),
            user: Some("bb".into()),
            ..member("ada-phone", "person", "Ada")
        }]),
    );
    let mut authors = ui::AuthorDirectory::new();
    register_presence(&mut authors, &here);
    assert_eq!(authors.resolve(&hex::encode("bb")).name, "Ada");
    assert_eq!(authors.device_name("ada-phone"), Some("Ada"));
    assert_eq!(authors.device_name("me"), Some("Pat"));
    assert_eq!(authors.resolve(&hex::encode("aa")).name, "Pat");
    assert_eq!(
        authors
            .resolve(&automerge::ActorId::random().to_hex_string())
            .name,
        ui::history_source::UNKNOWN_AUTHOR_NAME,
        "someone not here is nobody presence can name"
    );
}
