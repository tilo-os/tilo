//! The control socket's own behaviour: what a request waits for, what it is answered, and
//! what a locked session shows of itself.

use super::state::owner_shown;
use super::*;

tilo_test_env::scenario! {
    fn a_request_waits_for_the_workspace_in_front;

    phase a_request_before_the_shell_holds_a_workspace_waits_for_the_one_in_front;
    {
        let front = Arc::new(Front::default());
        let waiting = {
            let front = Arc::clone(&front);
            std::thread::spawn(move || front.known(Duration::from_secs(5)))
        };
        // The loop's pass serves the session while the request waits: it is answered for the
        // workspace in front, not for none.
        front.served("workspace-1".to_owned());
        assert_eq!(
            waiting.join().expect("the waiting request answers"),
            Some("workspace-1".to_owned())
        );
        assert_eq!(
            front.known(Duration::from_millis(1)),
            Some("workspace-1".to_owned())
        );
    }

    phase a_shell_that_never_holds_a_workspace_names_none;
    {
        let front = Front::default();
        assert_eq!(front.known(Duration::from_millis(10)), None);
    }
}

tilo_test_env::scenario! {
    fn the_control_socket_opens_where_its_build_and_its_device_allow;

    phase only_a_development_build_opens_the_socket_it_is_named;
    {
        assert_eq!(
            path_in(true, Some("/run/user/1000/control.sock"), None, None),
            Some(PathBuf::from("/run/user/1000/control.sock"))
        );
        assert_eq!(
            path_in(false, Some("/run/user/1000/control.sock"), None, None),
            None
        );
        assert_eq!(
            OPENED_BY_THIS_BUILD,
            cfg!(any(debug_assertions, feature = "dev-overrides"))
        );
    }

    phase a_socket_nothing_names_opens_where_the_tools_look;
    {
        let runtime = Some(OsString::from("/run/user/1000"));
        assert_eq!(
            path_in(true, None, None, runtime),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/test-control.sock"
            ))
        );
        // A tool that names one keeps it, and the environment names the next.
        assert_eq!(
            path_in(
                true,
                Some("/tmp/named.sock"),
                Some(OsString::from("/tmp/env.sock")),
                None
            ),
            Some(PathBuf::from("/tmp/named.sock"))
        );
        assert_eq!(
            path_in(true, None, Some(OsString::from("/tmp/env.sock")), None),
            Some(PathBuf::from("/tmp/env.sock"))
        );
        assert_eq!(path_in(true, None, None, None), None);
        assert_eq!(path_in(true, None, Some(OsString::new()), None), None);
    }

    phase the_owners_api_is_the_devices_own_and_a_headless_session_keeps_its_own;
    {
        let runtime = Some(OsString::from("/run/user/1000"));
        assert_eq!(
            admin_path_in(true, false, None, runtime.clone()),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/admin-control.sock"
            ))
        );
        assert_eq!(
            admin_path_in(true, true, None, runtime.clone()),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/headless-owner-control.sock"
            ))
        );
        // A session in another desktop's window owns no machine, and serves no owner's shared.
        assert_eq!(admin_path_in(false, false, None, runtime.clone()), None);
        // A path a developer named is theirs, and tells where it is.
        assert_eq!(
            admin_path_in(
                false,
                false,
                Some(PathBuf::from("/run/somewhere.sock")),
                runtime
            ),
            Some(PathBuf::from("/run/somewhere.sock"))
        );
        assert_eq!(admin_path_in(true, false, None, None), None);
    }
}

tilo_test_env::scenario! {
    fn the_owner_api_shows_the_owner_what_the_shell_shows_and_no_more;

    phase the_owner_api_serves_the_action_model_and_the_state_it_shows;
    {
        // Every op of the protocol, and the shared that serves it: the owner's shared is for a person's
        // own scripts, which drive the desktop through the model it publishes and read the state it
        // shows — never through the pointer, the keyboard or a picture of the screen.
        let owner = [
            Op::ProcessMemory,
            Op::ProfileStart,
            Op::ProfileStatus,
            Op::ProfileStop,
            Op::Settings,
            Op::StateDump,
            Op::Watch,
            Op::Windows,
            Op::KeepingDevices,
        ];
        for op in Op::ALL {
            let served_there = owner.contains(&op);
            assert!(served(Audience::Dev, op), "a development socket answers {op:?}");
            assert_eq!(
                served(Audience::Owner, op),
                served_there,
                "the owner's shared and {op:?}"
            );
            assert_eq!(
                Op::of(&json!({"op": op.name()})),
                Some(op),
                "{op:?} reads back as {}",
                op.name()
            );
        }
        // Every op the protocol answers is in the list, each once, under its own name.
        let mut names: Vec<&str> = Op::ALL.iter().map(|op| op.name()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "click_at",
                "click_named",
                "daemon_link",
                "drag",
                "keeping_devices",
                "key",
                "key_down",
                "key_up",
                "list_targets",
                "modifiers",
                "pinch",
                "pointer_button",
                "pointer_move",
                "process_memory",
                "profile_start",
                "profile_status",
                "profile_stop",
                "provision_test_identity",
                "says",
                "scroll",
                "search_rows",
                "settings",
                "shell_action",
                "shell_setting",
                "snapshot",
                "state_dump",
                "swipe",
                "type",
                "unlock",
                "watch",
                "windows",
            ],
            "the protocol's ops"
        );
        assert_eq!(Op::of(&json!({"op": "nonsense"})), None);
    }

    phase a_window_row_says_the_window_and_where_its_frame_draws_it;
    {
        use tilo_desktop_model::{
            Layout, Strip, Tab, TileContent, WindowGroup, Workspace, WorkspaceId, ids::StripId,
            width::WindowWidth,
        };
        let tab = |id: u64, title: &str, tile: TileContent| Tab {
            id,
            title: title.to_owned(),
            tile,
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let mut strip = Strip::new(StripId(1), "Windows");
        strip.window_groups = vec![WindowGroup {
            id: 10,
            tabs: vec![
                tab(
                    100,
                    "Notes",
                    TileContent::Foreign {
                        app_id: "so.tilo.notes".into(),
                        surface_id: 7,
                    },
                ),
                tab(
                    101,
                    "Files",
                    TileContent::LocalApp {
                        app_id: "so.tilo.files".into(),
                        label: "Files".into(),
                        note: String::new(),
                    },
                ),
            ],
            active_tab: 100,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }];
        let mut home = Workspace::new(WorkspaceId::from_u128(1), "Home");
        home.strips = vec![strip];
        let mut tree = Layout {
            active: home.id(),
            workspaces: vec![home],
            ..Layout::default()
        };
        tree.active_strip
            .insert(WorkspaceId::from_u128(1), StripId(1));
        tree.set_active_group(StripId(1), 10);
        // The frame draws the one window it has mapped; the other tab is an app to open again, with
        // no window of its own yet.
        let frames = |id: u64| if id == 7 { 41 } else { 0 };
        // The window's app says what it shows; the tab with no window of its own is on no
        // record, so it shows nothing.
        let resources = |tab: u64| (tab == 100).then(|| "notes.page:groceries".to_owned());
        // The window was opened on one document of its app's library.
        let items = |tab: u64| (tab == 100).then(|| "tl:so.tilo.pdf-viewer:lib/a".to_owned());
        let rows = window_rows(
            &tree,
            (
                |id| (id == 7).then_some((14, 96, 946, 984)),
                frames,
                resources,
                items,
            ),
            None,
        );
        assert_eq!(rows.len(), 2, "one row per tab: {rows:?}");
        assert_eq!(rows[0]["window"], json!(7));
        assert_eq!(rows[0]["rect"], json!([14, 96, 946, 984]));
        assert_eq!(rows[0]["active"], json!(true));
        assert_eq!(rows[0]["frames"], json!(41));
        assert_eq!(rows[0]["resource"], json!("notes.page:groceries"));
        assert_eq!(rows[0]["item"], json!("tl:so.tilo.pdf-viewer:lib/a"));
        assert_eq!(rows[1]["window"], Json::Null);
        assert_eq!(rows[1]["rect"], Json::Null);
        assert_eq!(rows[1]["frames"], json!(0));
        assert_eq!(rows[1]["resource"], Json::Null);
        assert_eq!(rows[1]["item"], Json::Null);
        // One application's windows alone, where a caller names one: the other application's tab
        // is not a row, and its place is not read of.
        let notes = window_rows(
            &tree,
            (
                |id| (id == 7).then_some((14, 96, 946, 984)),
                frames,
                resources,
                items,
            ),
            Some("so.tilo.notes"),
        );
        assert_eq!(notes.len(), 1, "one row for one app: {notes:?}");
        assert_eq!(notes[0]["app_id"], json!("so.tilo.notes"));
        // A frame that no longer maps the window leaves the row without a rectangle rather than one
        // that is no longer where it is drawn.
        let closed = window_rows(&tree, (|_| None, frames, resources, items), None);
        assert_eq!(closed[0]["window"], json!(7));
        assert_eq!(closed[0]["rect"], Json::Null);
    }

    phase a_locked_shell_shows_the_owner_api_nothing_the_lock_hides;
    {
        let dump = |locked: bool| {
            json!({"state": {
                "workspace_tree": {"workspaces": [{"name": "Work"}]},
                "held_workspaces": [{"workspace": "00000000-0000-0000-0000-000000000001"}],
                "chrome": {
                    "session_locked": locked,
                    "in_transition": false,
                    "overlays": {"quick_settings": true, "overview": false},
                },
                "peers": Json::Null,
                "sharing": {"sharing": [{"tab": "7", "workspace": "workspace-1", "viewers": []}]},
                "notifications": [{"id": 4, "title": "Ada wrote to you", "body": "hello"}],
                "clipboard": "the person's copy",
            }})
        };
        // Unlocked, the owner's shared sees the session as the development socket does.
        assert_eq!(
            owner_shown(Audience::Owner, dump(false)),
            dump(false),
            "an unlocked session is shown whole"
        );
        // Locked, it learns that the screen is locked, and nothing of what the lock covers.
        let locked = owner_shown(Audience::Owner, dump(true));
        assert_eq!(locked["state"]["chrome"]["session_locked"], true);
        assert_eq!(locked["state"]["workspace_tree"], Json::Null);
        assert_eq!(locked["state"]["held_workspaces"], json!([]));
        assert_eq!(
            locked["state"]["sharing"],
            Json::Null,
            "which windows the session shares is behind the lock too"
        );
        assert_eq!(
            locked["state"]["notifications"],
            Json::Null,
            "what the person was told is behind the lock too"
        );
        assert_eq!(
            locked["state"]["clipboard"],
            Json::Null,
            "what the person copied is behind the lock too"
        );
        assert_eq!(
            locked["state"]["chrome"]["overlays"],
            json!({"quick_settings": false, "overview": false})
        );
        // A development socket sees the session it drives, locked or not.
        assert_eq!(owner_shown(Audience::Dev, dump(true)), dump(true));
        // An app reads its own workspace of the session, and nothing of what the session shares.
        let app = Context {
            who: Audience::Dev,
            scope: Some(Arc::new(Scope {
                app: "so.tilo.notes".to_owned(),
                workspace: "workspace-1".to_owned(),
            })),
        };
        assert_eq!(
            shown(&app, dump(false))["state"]["sharing"],
            Json::Null,
            "the window shares are the session's, and no app's to read"
        );
        assert_eq!(
            shown(&app, dump(false))["state"]["notifications"],
            Json::Null,
            "what the person was told is theirs, and no app's to read"
        );
        assert_eq!(
            shown(&app, dump(false))["state"]["clipboard"],
            Json::Null,
            "what the person copied is read through the clipboard's grant, never here"
        );
    }

    phase the_owner_api_is_the_account_that_owns_the_device;
    {
        assert_eq!(owner_decision(true, Ok(true)), Ok(()));
        assert_eq!(
            owner_decision(true, Ok(false)),
            Err(NotServed::NotDeviceOwner)
        );
        // A member of `wheel` whose device ownership cannot be read is refused, not guessed at.
        assert!(matches!(
            owner_decision(true, Err(io::Error::other("no key"))),
            Err(NotServed::OwnershipUnreadable(_))
        ));
        assert_eq!(
            owner_decision(false, Ok(true)),
            Err(NotServed::NotAdministrator)
        );
    }

}

tilo_test_env::scenario! {
    fn what_a_tool_sends_is_read_as_keys_addresses_cursors_and_input;

    phase a_named_key_and_a_character_are_told_apart;
    {
        assert_eq!(named_key("Enter"), Some(28));
        assert_eq!(named_key("Super"), Some(125));
        assert_eq!(named_key("enter"), None);
        assert_eq!(named_key("a"), None);
    }

    phase the_input_a_tool_injects_is_answered_where_a_device_hands_its_own;
    {
        let injected: Vec<&str> = Op::ALL
            .iter()
            .filter(|op| op.hands_input())
            .map(|op| op.name())
            .collect();
        assert_eq!(
            injected,
            [
                "click_at",
                "drag",
                "key",
                "key_down",
                "key_up",
                "modifiers",
                "pinch",
                "pointer_button",
                "pointer_move",
                "scroll",
                "swipe",
                "type",
            ],
            "the input a tool injects is served before the pass lays its frame \
         out, so that frame carries it"
        );
        // Everything else reads or changes what a frame already shows, and is answered once the
        // pass has drawn it.
        for op in [
            Op::ClickNamed,
            Op::StateDump,
            Op::Snapshot,
            Op::Watch,
            Op::Windows,
        ] {
            assert!(!op.hands_input(), "{} waits for the frame", op.name());
        }
    }
}

tilo_test_env::scenario! {
    fn a_watcher_and_a_wait_are_answered_when_the_state_changes;

    phase a_watcher_is_sent_the_state_and_nothing_is_built_while_none_is;
    {
        let changes = Changes::new(Arc::new(|| ()));
        changes.follow(true, || json!({"state": {"unwatched": true}}));
        assert_eq!(changes.generation(), 1, "a change wakes a held request");
        assert!(
            changes.state.lock_recover().dump.is_none(),
            "nobody watched; no state was built"
        );

        changes.attach();
        assert!(
            changes.state.lock_recover().refresh,
            "the state as it is now is asked for"
        );
        changes.follow(false, || json!({"state": {"watched": true}}));
        let (generation, dump) = changes.wait(1, None, None).expect("the published state");
        assert_eq!(generation, 2);
        assert_eq!(dump, Some(json!({"state": {"watched": true}})));

        changes.detach();
        changes.follow(true, || json!({"state": {"unwatched": true}}));
        assert_eq!(changes.generation(), 3);
        assert!(
            changes
                .state
                .lock_recover()
                .dump
                .as_ref()
                .is_some_and(|(dumped, _)| *dumped == 2),
            "the dump a watcher read stays the last one built"
        );
    }

    phase a_wait_answers_when_the_state_changes_or_the_session_ends;
    {
        let changes = Changes::new(Arc::new(|| ()));
        changes.follow(true, Json::default);
        assert_eq!(changes.generation(), 1);
        assert_eq!(
            changes.wait(1, None, Some(Instant::now())),
            None,
            "a wait past its deadline answers with what it has"
        );

        changes.end();
        assert_eq!(changes.wait(0, None, None), None, "the session ended");
    }
}
