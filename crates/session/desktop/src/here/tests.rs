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
    fn agents_here_are_met_once_and_said_finished_once;

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
                record: "0123456789abcdef".into(),
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
            ui::presence::identity_rgb(ui::tokens::PresenceHue::Orange),
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

    phase a_finished_agent_says_how_many_changes_its_run_made_and_where;
    // A finished agent's notice counts what its newest run changed: each change of its record and
    // file edits, not the commands it ran or pages it read, and names the workspace the history gives it.
    {
        use tilo_actions::remote::workspace_activity as wire;
        let target = wire::Target {
            app_id: Some("so.tilo.notes".into()),
            app_label: Some("Notes".into()),
            id: "note".into(),
            kind: String::new(),
            label: "Welcome".into(),
        };
        let action = |view: wire::ActionView| wire::SessionAction {
            range: wire::Range {
                from: "aa".into(),
                to: "ab".into(),
            },
            changes: 2,
            at_ms: 1,
            summary: String::new(),
            view,
            app_id: "so.tilo.notes".into(),
            record: "note".into(),
            undo: None,
        };
        let record = || {
            action(wire::ActionView::Record {
                target: target.clone(),
            })
        };
        let run = |name: &str, last_ms: i64, actions: Vec<wire::SessionAction>| wire::AgentSession {
            id: format!("mcp:{name}{last_ms}"),
            agent: wire::Author {
                name: name.into(),
                ..wire::Author::default()
            },
            workspace: Some("Storefront".into()),
            instruction: String::new(),
            decision: None,
            started_ms: 0,
            last_ms,
            range: wire::Range {
                from: "aa".into(),
                to: "ab".into(),
            },
            actions,
            undo_blocked: None,
            now: None,
        };
        let sessions = wire::Sessions {
            sessions: vec![
                run("Claude Code", 5, vec![record()]),
                run(
                    "Claude Code #2",
                    9,
                    vec![
                        record(),
                        record(),
                        action(wire::ActionView::Command {
                            target: target.clone(),
                            command: "cargo test".into(),
                            cwd: String::new(),
                            output: Vec::new(),
                            more: false,
                            exit: Some(0),
                            started_ms: None,
                            ended_ms: None,
                            execution: None,
                        }),
                        record(),
                        record(),
                    ],
                ),
                run("Codex", 12, vec![record()]),
            ],
            total: 3,
            pending: false,
            mutations: true,
        };
        let finished = AgentFinished {
            publisher: "claude".into(),
            label: "Claude Code".into(),
        };
        assert_eq!(
            finished.summary(&sessions),
            Some(SessionSummary {
                session: "mcp:Claude Code #29".into(),
                changes: 8,
                worked: String::new(),
                workspace: Some("Storefront".into()),
            }),
            "its newest run, counting the two changes of each of its four record edits and not the \
         command"
        );
        let unknown = AgentFinished {
            label: "Cursor".into(),
            ..finished
        };
        assert_eq!(unknown.summary(&sessions), None);
    }

    phase an_agent_gone_for_good_has_finished;
    // An agent that leaves has finished, said once and only after a reconnect's grace: one back
    // within it never left.
    {
        let mut here = HereState::default();
        let claude = || LiveMember {
            activity: Some("Editing notes/Welcome".into()),
            ..member("claude", "agent", "Claude Code")
        };
        here.set_members(None, Some(vec![claude()]));
        assert!(here.take_agents_finished(1_000).is_empty());
        here.set_members(None, Some(Vec::new()));
        assert!(
            here.take_agents_finished(1_000 + REJOIN_QUIET_MS / 2)
                .is_empty(),
            "a reconnect is not finishing"
        );
        here.set_members(None, Some(vec![claude()]));
        assert!(
            here.take_agents_finished(2_000 + REJOIN_QUIET_MS / 2)
                .is_empty()
        );
        here.set_members(None, Some(Vec::new()));
        let later = 3_000 + REJOIN_QUIET_MS * 2;
        assert_eq!(
            here.take_agents_finished(later),
            [AgentFinished {
                publisher: "claude".into(),
                label: "Claude Code".into(),
            }]
        );
        assert!(here.take_agents_finished(later + 1).is_empty(), "said once");
    }

    phase an_agent_restarting_its_session_finishes_once;
    // An agent's client restarting its session is a new publisher working on: the agent has not
    // finished while any session of it is here, and once the last leaves its sessions are said
    // finished as one, by the one here last.
    {
        let mut here = HereState::default();
        let session = |publisher: &str| member(publisher, "agent", "Claude Code");
        here.set_members(None, Some(vec![session("first")]));
        assert!(here.take_agents_finished(1_000).is_empty());
        // Its client restarts it: the first session left, a second works on.
        here.set_members(None, Some(vec![session("second")]));
        assert!(here.take_agents_finished(2_000).is_empty());
        let later = 3_000 + REJOIN_QUIET_MS * 2;
        assert!(
            here.take_agents_finished(later).is_empty(),
            "the first session left long ago, but the agent is still here"
        );
        assert_eq!(
            here.next_agent_finished_ms(),
            None,
            "nothing is due while it is here"
        );
        here.set_members(None, Some(Vec::new()));
        assert_eq!(here.next_agent_finished_ms(), Some(later + REJOIN_QUIET_MS));
        assert_eq!(
            here.take_agents_finished(later + REJOIN_QUIET_MS),
            [AgentFinished {
                publisher: "second".into(),
                label: "Claude Code".into(),
            }],
            "one notice for the agent, from the session here last"
        );
        assert!(here.take_agents_finished(later * 3).is_empty(), "said once");
        assert_eq!(here.next_agent_finished_ms(), None);
    }

    phase an_agent_gone_is_due_once_its_grace_passes;
    // An agent that left is due to be said finished once the reconnect's grace has passed since
    // it was last here, so the page can wake for it; none is due while it is here.
    {
        let mut here = HereState::default();
        here.set_members(None, Some(vec![member("claude", "agent", "Claude Code")]));
        assert!(here.take_agents_finished(1_000).is_empty());
        assert_eq!(here.next_agent_finished_ms(), None, "it is still here");
        here.set_members(None, Some(Vec::new()));
        assert_eq!(here.next_agent_finished_ms(), Some(1_000 + REJOIN_QUIET_MS));
        assert_eq!(here.take_agents_finished(1_000 + REJOIN_QUIET_MS).len(), 1);
        assert_eq!(
            here.next_agent_finished_ms(),
            None,
            "said, nothing more is due"
        );
    }
}

tilo_test_env::scenario! {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test arithmetic: the lint exempts the body of a test function, not the closure a phase is"
    )]
    fn people_arriving_are_named_and_announced_once;

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
            Some(guest_label())
        );
        assert_eq!(here.person("someone-who-left"), None);
    }

    phase only_people_who_join_later_are_announced;
    // Someone joining is worth a notice; who was already here when this page arrived, this page
    // itself, agents and people leaving are not.
    {
        let mut here = HereState::default();
        assert!(here.take_arrivals(1_000).is_empty(), "no roster yet");
        let me = member("me", "person", "Quinn");
        here.set_members(
            Some(me.clone()),
            Some(vec![me.clone(), member("ada", "person", "Ada")]),
        );
        assert!(here.take_arrivals(1_000).is_empty(), "Ada was already here");
        here.set_members(
            Some(me.clone()),
            Some(vec![
                me.clone(),
                member("ada", "person", "Ada"),
                member("ben", "person", "Ben"),
                member("bot", "agent", "Scribe"),
            ]),
        );
        assert_eq!(here.take_arrivals(1_000), ["Ben"]);
        assert!(here.take_arrivals(1_000).is_empty(), "announced once");
        here.set_members(Some(me.clone()), Some(vec![me.clone()]));
        assert!(here.take_arrivals(1_000).is_empty(), "leaving is not news");
        here.set_members(Some(me.clone()), None);
        assert!(here.take_arrivals(2_000).is_empty(), "the layer dropped");
        here.set_members(
            Some(me.clone()),
            Some(vec![me, member("ada", "person", "Ada")]),
        );
        assert_eq!(
            here.take_arrivals(1_000 + REJOIN_QUIET_MS),
            ["Ada"],
            "she came back"
        );
    }

    phase a_reconnect_or_repeat_from_someone_here_is_not_a_second_arrival;
    // Someone already here who drops out of the live layer for a moment and is back, or whose
    // presence arrives again, has not joined again: a page renews its live grant by reconnecting,
    // which drops and re-adds every member, and a carrier re-announces a member as withdrawn and
    // back. Neither is news; one arrival is one notice, when it happens.
    {
        let me = member("me", "person", "Quinn");
        let ben = LiveMember {
            user: Some("ben-root".into()),
            ..member("ben", "person", "Ben")
        };
        let mut here = HereState::default();
        here.set_members(Some(me.clone()), Some(vec![me.clone()]));
        assert!(here.take_arrivals(0).is_empty());
        here.set_members(Some(me.clone()), Some(vec![me.clone(), ben.clone()]));
        assert_eq!(here.take_arrivals(1_000), ["Ben"], "announced on arrival");
        // The same frame again, and a presence update, are not arrivals.
        here.set_members(Some(me.clone()), Some(vec![me.clone(), ben.clone()]));
        assert!(here.take_arrivals(2_000).is_empty());
        let mut moved = ben.clone();
        moved.place = Some("2".into());
        here.set_members(Some(me.clone()), Some(vec![me.clone(), moved]));
        assert!(here.take_arrivals(3_000).is_empty());
        // This page renews its grant ten minutes on: everyone drops out and is back; then Ben's
        // page does the same.
        for at in [600_000, 660_000] {
            here.set_members(Some(me.clone()), Some(vec![me.clone()]));
            assert!(here.take_arrivals(at).is_empty());
            here.set_members(Some(me.clone()), None);
            assert!(here.take_arrivals(at + 50).is_empty());
            here.set_members(Some(me.clone()), Some(vec![me.clone(), ben.clone()]));
            assert!(here.take_arrivals(at + 300).is_empty(), "Ben never left");
        }
        let ben_tab = LiveMember {
            user: Some("ben-root".into()),
            ..member("ben-tab", "person", "Ben")
        };
        here.set_members(Some(me.clone()), Some(vec![me, ben, ben_tab]));
        assert!(here.take_arrivals(700_000).is_empty(), "his other tab");
    }

    phase a_visitor_is_announced_by_the_name_the_bar_shows;
    // A visitor's presence can arrive before it carries their name: the notice waits for the name
    // rather than calling them a guest, and names them as the bar does once their presence says it.
    {
        let me = member("me", "person", "Quinn");
        let mut here = HereState::default();
        here.set_members(Some(me.clone()), Some(vec![me.clone()]));
        assert!(here.take_arrivals(1).is_empty());
        here.set_members(
            Some(me.clone()),
            Some(vec![me.clone(), member("dolphin", "person", "")]),
        );
        assert!(here.take_arrivals(1).is_empty(), "no name yet");
        here.set_members(
            Some(me.clone()),
            Some(vec![me, member("dolphin", "person", "Quiet Dolphin")]),
        );
        assert_eq!(here.take_arrivals(2), ["Quiet Dolphin"]);
        assert!(here.take_arrivals(3).is_empty(), "announced once");
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
            source: crate::people::LineSource::Observed,
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
    fn everyone_here_wears_a_colour_of_their_own;

    phase visitors_here_together_wear_distinct_colours_on_every_page;
    // Two visitors whose identities hash to the same colour still wear two once they are here
    // together, and each one's page settles the workspace alike, so a person is the same colour on
    // every screen, their own included.
    {
        assert_eq!(
            ui::presence::user_color("visitor-0"),
            ui::presence::user_color("visitor-13")
        );
        let certified = |device: &str| LiveMember {
            user: Some(device.into()),
            ..member(device, "person", device)
        };
        let page = |own: &str, other: &str| {
            let mut here = HereState::default();
            here.set_members(Some(certified(own)), Some(vec![certified(other)]));
            here
        };
        let first = page("visitor-0", "visitor-13");
        let second = page("visitor-13", "visitor-0");
        assert_ne!(
            first.colors()["visitor-0"],
            first.colors()["visitor-13"],
            "two people here together never share a colour"
        );
        assert_eq!(first.colors(), second.colors(), "every page settles alike");
        assert_eq!(first.own_rgb(), Some(second.colors()["visitor-0"]));
        let tree = Layout::default();
        let shown = first.people(&tree, &[], &|_| None);
        assert_eq!(shown[0].rgb, second.own_rgb().expect("own colour"));
    }

    phase an_agent_never_wears_the_persons_own_colour;
    // An agent that announces the colour the person themselves wears does not wear it: the
    // person keeps their own colour, on their own ring and everywhere else, and the agent reads
    // apart from them.
    {
        let mine = ui::presence::user_color("me-root");
        let hex = format!("#{:02x}{:02x}{:02x}", mine.0, mine.1, mine.2);
        let claiming = |member: &mut LiveMember| {
            member.colour.clone_from(&hex);
            member.clone()
        };
        let person = claiming(&mut LiveMember {
            user: Some("me-root".into()),
            ..member("me", "person", "Pat")
        });
        // The agent's face names no client this build knows, so it wears what it was given.
        let agent = claiming(&mut member("helper", "agent", "Helper #1"));
        let mut here = HereState::default();
        here.set_members(Some(person), Some(vec![agent]));
        assert_eq!(
            here.own_rgb(),
            Some(mine),
            "the person wears their own colour"
        );
        assert_ne!(
            here.colors().get("helper").copied(),
            Some(mine),
            "no member is drawn in the person's own colour"
        );
        // A client hue the person themselves wears is not worn by its agent either.
        let orange = ui::presence::identity_rgb(ui::tokens::PresenceHue::Orange);
        let orange_hex = format!("#{:02x}{:02x}{:02x}", orange.0, orange.1, orange.2);
        let mut here = HereState::default();
        here.set_members(
            Some(LiveMember {
                user: Some("me-root".into()),
                colour: orange_hex,
                ..member("me", "person", "Pat")
            }),
            Some(vec![member("claude", "agent", "Claude Code #1")]),
        );
        assert_ne!(
            here.colors().get("claude").copied(),
            Some(orange),
            "the agent steps aside from the colour the person wears"
        );
        assert_eq!(here.own_rgb(), Some(orange));
    }

    phase an_agent_steps_clear_of_its_persons_colour;
    // One agent wears one colour on every surface that draws it: the shell's faces and the cards in
    // its windows, the People list's row, the bar's face, and the colour its own apps paint its
    // card, its text marks and its caret in — the one rule ([`ui::presence::participant_color`]),
    // never a colour a surface picked for itself.
    // An agent that claims no colour wears the one rule's, clear of the person it is certified
    // for, as its record and every app paint it: never theirs, even where what it is would land
    // on them.
    {
        let helper = "agent:helper@host";
        let own = ui::tokens::PresenceHue::of_agent(helper);
        let person = (0..1000)
            .map(|n| format!("person-{n}"))
            .find(|id| ui::tokens::PresenceHue::of_identity(id) == own)
            .expect("someone whose colour what it is lands on");
        let agent = LiveMember {
            user: Some(person.clone()),
            ..member(helper, "agent", "Helper")
        };
        let mut here = HereState::default();
        here.set_members(None, Some(vec![agent]));
        let worn = here.colors().get(helper).copied();
        assert_eq!(
            worn,
            Some(ui::presence::member_color(&ui::permissions::MemberId::agent(helper), Some(&person)))
        );
        assert_ne!(
            worn,
            Some(ui::presence::user_color(&person)),
            "never theirs"
        );
    }

    phase an_agent_wears_one_colour_on_every_surface;
    {
        let helper = "agent:helper@host";
        let (r, g, b) = ui::presence::member_color(&ui::permissions::MemberId::agent(helper), None);
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
                presence.color_for(&MemberId::agent("helper@host")),
                ui::presence::member_color(&ui::permissions::MemberId::agent(helper), None),
            ),
            (Some((r, g, b)), (r, g, b), (r, g, b), (r, g, b)),
            "the board, its people row, the windows and every app settle one colour"
        );

        // The bar and the People list read the workspace's rows, which this page fills from its
        // board: the same colour again.
        let mut tree = Layout {
            workspaces: vec![Workspace::new(tilo_desktop_model::WorkspaceId::from_u128(1), "Shell")],
            ..Layout::default()
        };
        tree.active = tree.workspaces[0].id();
        place_here(&mut tree, &people, None);
        let rows = crate::people::roster_rows(&tree, "me-root");
        let row = rows
            .iter()
            .find(|row| row.id == helper)
            .expect("the agent has a row in the People list");
        assert_eq!(
            row.color,
            vello::peniko::Color::from_rgb8(r, g, b),
            "the People list"
        );
        let mut bar =
            crate::topbar::TopBarChrome::from_tree_with_local_user(&tree, "me-root".into());
        bar.sync_from_tree(&tree);
        assert_eq!(
            bar.face_colors(),
            [vello::peniko::Color::from_rgb8(r, g, b)],
            "the face on the bar"
        );
    }

    phase a_person_wears_the_colour_they_chose_and_the_others_step_aside;
    // The colour someone chose in their own card is the colour they wear here,
    // on their own page and on everyone else's, and whoever else's identity
    // names the same one steps to a free entry: one person, one colour, on the
    // bar, in the People list and in every app they edit in.
    {
        let violet = ui::presence::identity_rgb(ui::tokens::PresenceHue::Violet);
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
        assert_ne!(
            peer_page.colors()["bo"],
            violet,
            "whoever else lands on it steps aside"
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
            (ui::presence::UNSETTLED_RGB, None),
            "not yet known whose device it is"
        );
        assert!(!here.colors().contains_key("ben-tab"));
        assert_eq!(
            here.own_rgb(),
            Some(ui::presence::assign_peer_colors(&["me-root"], &[])["me-root"]),
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
        let settled = ui::presence::assign_peer_colors(&["me-root", "ben-root"], &[]);
        assert_eq!(shown(&mut here), (settled["ben-root"], Some(settled["ben-root"])));
        // A neutral face paints grey, not a ring colour.
        let palette = ui::tokens::colors::Colors::dark().presence;
        let painted = palette.of_rgb(ui::presence::UNSETTLED_RGB);
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
        let colors = here.colors();
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
        use crate::people::LineSource;
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
        use crate::people::LineSource;
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
            people[0].label, "Framework 16",
            "a member who has not named themselves goes by their device's name"
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
            ui::presence::identity_rgb(ui::tokens::PresenceHue::Orange),
            "in its client's colour"
        );
        assert_eq!(
            listed[0]
                .status
                .as_ref()
                .map(|line| (line.text.as_str(), line.source)),
            Some(("Looking at the apps", crate::people::LineSource::Written)),
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
                record: "welcome".into(),
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
                    record: digest.into(),
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

    phase people_sharing_a_name_take_the_rosters_numbers_on_every_page;
    // Two people going by one name are numbered, this page included, by when the workspace
    // admitted them: the numbers its roster gives them, on every page, never the order their ids
    // sort in.
    {
        // The workspace admitted z before a, though a's id sorts first.
        let mut workspace = Workspace::new(tilo_desktop_model::WorkspaceId::from_u128(1), "Studio");
        for (person, admitted) in [("person-z", 10_u64), ("person-a", 20)] {
            workspace
                .policy
                .members
                .insert(person.into(), tilo_action_policy::Role::Editor);
            workspace.policy.joined.insert(person.into(), admitted);
            workspace.member_here(Member::named(collab_types::MemberId::person(person), "Alex"));
        }
        let tree = Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Layout::default()
        };
        // When the workspace admitted each of its people, as the host hands it over.
        let joined: BTreeMap<String, u64> = tree
            .active_workspace()
            .expect("the workspace is in front")
            .policy
            .admitted()
            .map(|(person, admitted)| (person.to_owned(), admitted))
            .collect();
        let alex = |publisher: &str, person: &str| LiveMember {
            user: Some(person.into()),
            ..member(publisher, "person", "Alex")
        };
        let page = |own: (&str, &str), others: Vec<LiveMember>| {
            let mut here = HereState::default();
            here.set_joined(&joined);
            here.set_members(Some(alex(own.0, own.1)), Some(others));
            here
        };
        let roster: HashMap<String, String> = crate::people::roster_rows(&tree, "me-root")
            .into_iter()
            .map(|row| (row.id, row.label))
            .collect();
        assert_eq!(roster["person-z"], "Alex (1)", "who joined first is (1)");
        assert_eq!(roster["person-a"], "Alex (2)");

        let mut mine = page(("dev-a", "person-a"), vec![alex("dev-z", "person-z")]);
        let listed = |here: &HereState| -> HashMap<String, String> {
            here.people(&tree, &[], &|_| None)
                .into_iter()
                .map(|person| (person.user.expect("certified"), person.label))
                .collect()
        };
        let seen = listed(&mine);
        assert_eq!(
            seen["person-z"], roster["person-z"],
            "as the roster has them"
        );
        assert_eq!(mine.own_shown().as_deref(), Some("Alex (2)"));

        // Another page, hearing of them in the other order, numbers them alike.
        let theirs = page(("dev-z", "person-z"), vec![alex("dev-a", "person-a")]);
        assert_eq!(
            listed(&theirs)["person-a"],
            roster["person-a"],
            "the same numbers on another page"
        );
        assert_eq!(theirs.own_shown().as_deref(), Some("Alex (1)"));

        // Alone, nobody needs a number.
        mine.set_members(Some(alex("dev-a", "person-a")), Some(Vec::new()));
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
                member: (id == "cc").then(|| "agent:cc".to_owned()),
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
            member: Some("agent:cc".into()),
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
            source: crate::people::LineSource::Observed,
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
            source: crate::people::LineSource::Written,
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
            member: Some("agent:scribe".into()),
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
            .find(|member| member.id.key() == "agent:scribe")
            .expect("the agent is on the member list");
        assert!(!agent.id.is_person(), "an agent's row is software's");
        let rows = crate::people::roster_rows(&tree, "me-root");
        assert!(rows[0].is_agent, "the agent's face is software's");
    }
}

fn finished_agent(publisher: &str) -> AgentFinished {
    AgentFinished {
        publisher: publisher.into(),
        label: "Claude Code".into(),
    }
}

fn session_summary(session: &str, changes: usize, worked: &str) -> SessionSummary {
    SessionSummary {
        session: session.into(),
        changes,
        worked: worked.into(),
        workspace: Some("Workspace".into()),
    }
}

tilo_test_env::scenario! {
    fn a_finished_run_is_told_once_with_the_agents_face;

    phase a_finished_notice_says_what_the_run_worked_with_the_agents_face;
    // A finished notice wears the agent's own face, not a person's initials, and says what the run
    // worked under its title, in its card's words, never the live status it had while it worked.
    {
        let mut told = std::collections::BTreeSet::new();
        let notice = finished_notice(
            &finished_agent("claude"),
            Some(&session_summary("run-1", 8, "Edited Launch notes")),
            &mut told,
        )
        .expect("a run that changed records is told");
        assert_eq!(notice.title, "Claude Code finished: 8 changes in Workspace");
        assert_eq!(notice.body, "Edited Launch notes");
        let from = notice.from.expect("it is from the agent");
        assert!(from.agent, "the agent's square face, in its client's tint");
        assert_eq!(from.identity, "claude");
    }

    phase one_run_is_told_once_and_a_run_of_no_changes_not_at_all;
    // One run is one notice, however many of the agent's sessions worked it; a session whose run
    // changed nothing tells nothing.
    {
        let mut told = std::collections::BTreeSet::new();
        let run = session_summary("run-1", 8, "Edited Launch notes");
        assert!(finished_notice(&finished_agent("first"), Some(&run), &mut told).is_some());
        assert!(
            finished_notice(&finished_agent("second"), Some(&run), &mut told).is_none(),
            "the second session of the same run"
        );
        assert!(
            finished_notice(
                &finished_agent("third"),
                Some(&session_summary("run-2", 0, "")),
                &mut told
            )
            .is_none(),
            "a run that changed nothing"
        );
        let unknown = finished_notice(&finished_agent("fourth"), None, &mut told)
            .expect("without the history, it still says the agent finished");
        assert_eq!(unknown.title, "Claude Code finished");
        assert!(
            unknown.body.is_empty(),
            "and claims no status it no longer has"
        );
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
