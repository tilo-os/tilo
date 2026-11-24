//! The Console app (`apps/tl/console/app.tl`) end to end: every agent session across the
//! workspaces with the MCP server that says which agents work now, the session chosen as its
//! timeline with what can be undone and opened, a command's whole output, a file's change, the
//! agents the server let in with Revoke, the workspaces it reaches, and what is not counted yet.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::{Value as Json, json};
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, UiNode, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/console/app.tl");

    /// The platform's packages, as a host registers them for an app this runtime does not run.
    fn platform() -> Vec<tilolang::PackageMeta> {
        tilolang_tilo::platform_apps::platform_packages().expect("the platform packages read")
    }

    /// Console over the platform's packages, its window open, the sessions and the MCP server as
    /// `sessions` and `server` say.
    struct Console {
        rt: Runtime,
        app: App,
        root: ResourceRef,
    }

    impl Console {
        fn new(sessions_json: &Json, server_json: &Json) -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            rt.set_clock(|| "2026-11-24T09:30:00Z".to_owned());
            let platform = platform();
            for package in &platform {
                // The action registry is the runtime's own: its types are here already, and
                // loading its package again declares them twice.
                if package.app == tilolang::ACTIONS_APP {
                    continue;
                }
                rt.load_foreign(package.clone())
                    .expect("a platform package");
            }
            let app = App::compile(SOURCE, &platform);
            authority.trust(&app.load(&rt));
            let sessions = ResourceRef::parse_id(
                "workspace.activity/sessions/all",
                "so.tilo.history#Sessions",
            )
            .expect("the sessions' address");
            rt.set_reading(&sessions, &sessions_json.to_string())
                .expect("the sessions read");
            let server = ResourceRef::parse_id("mcp.server", "tilo.mcp-server#Server")
                .expect("the server's address");
            rt.set_reading(&server, &server_json.to_string())
                .expect("the server reads");
            // By its app: more than one of the platform's packages declares a `Window`.
            let root = rt.create_root("so.tilo.console.Window").unwrap();
            Self { rt, app, root }
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("console"),
                    vec![Value::resource(&self.root)],
                )
                .expect("the window opens");
            window.set_lazy_rows(false).unwrap();
            window
        }
    }

    /// The sessions the history lists: one working now and one earlier.
    fn sessions_of() -> Json {
        json!({
            "sessions": [
                {
                    "id": "run-2",
                    "agent": {
                        "name": "Claude Code #2",
                        "rgb": [90, 120, 200],
                        "identity": "claude",
                        "relation": "software",
                        "on_behalf_of": "Ada",
                        "for_reader": true,
                        "task": null,
                    },
                    "workspace": "Studio",
                    "instruction": "Tidy the notes",
                    "decision": null,
                    "started_ms": 1_795_500_600_000_i64,
                    "last_ms": 1_795_502_400_000_i64,
                    "range": {"from": "aa", "to": "bb"},
                    "undo_blocked": null,
                    "now": "cargo test",
                    "now_kind": "running",
                    "actions": [
                        {
                            "range": {"from": "aa", "to": "bb"},
                            "changes": 4,
                            "at_ms": 1_795_502_400_000_i64,
                            "summary": "Edited src/notes.rs",
                            "kind": "file",
                            "place": "src/notes.rs",
                            "target": {
                                "app_id": "so.tilo.notes",
                                "app_label": "Notes",
                                "id": "note-1",
                                "kind": "tl.so.tilo.notes.Note",
                                "label": "Plan",
                            },
                            "title": "",
                            "cwd": "",
                            "output": [],
                            "more": false,
                            "exit": null,
                            "ended_ms": null,
                            "diff": [
                                {"side": "removed", "text": "old line"},
                                {"side": "added", "text": "new line"},
                            ],
                            "app_id": "so.tilo.notes",
                            "record": "note-1",
                            "execution": null,
                            "undo_label": "Undo this change",
                            "undo_verb": "undo-change",
                        },
                        {
                            "range": {"from": "bb", "to": "cc"},
                            "changes": 1,
                            "at_ms": 1_795_502_700_000_i64,
                            "summary": "Ran cargo test",
                            "kind": "command",
                            "place": "cargo test",
                            "target": {
                                "app_id": "so.tilo.terminal",
                                "app_label": "Terminal",
                                "id": "shell-1",
                                "kind": "terminal.session",
                                "label": "Shell",
                            },
                            "title": "",
                            "cwd": "~/notes",
                            "output": ["running 3 tests", "test ok"],
                            "more": true,
                            "exit": 0,
                            "ended_ms": 1_795_502_820_000_i64,
                            "diff": [],
                            "app_id": "so.tilo.terminal",
                            "record": "shell-1",
                            "execution": {
                                "workspace": null,
                                "app_id": "so.tilo.terminal",
                                "record": "shell-1",
                                "execution": "exec-1",
                            },
                            "undo_label": "Undo this run",
                            "undo_verb": "undo-entry",
                        },
                    ],
                },
                {
                    "id": "run-1",
                    "agent": {
                        "name": "Cursor #7",
                        "rgb": [200, 120, 90],
                        "identity": "cursor",
                        "relation": "software",
                        "on_behalf_of": "Bo",
                        "for_reader": false,
                        "task": null,
                    },
                    "workspace": "Elsewhere",
                    "instruction": "Draft the letter",
                    "decision": null,
                    "started_ms": 1_795_490_000_000_i64,
                    "last_ms": 1_795_491_000_000_i64,
                    "range": {"from": "01", "to": "02"},
                    "undo_blocked": null,
                    "now": "",
                    "now_kind": "",
                    "actions": [
                        {
                            "range": {"from": "01", "to": "02"},
                            "changes": 2,
                            "at_ms": 1_795_491_000_000_i64,
                            "summary": "Wrote a paragraph",
                            "kind": "record",
                            "place": "doc-1",
                            "target": {
                                "app_id": "so.tilo.word-processor",
                                "app_label": "Word Processor",
                                "id": "doc-1",
                                "kind": "tl.so.tilo.word-processor.Document",
                                "label": "Letter",
                            },
                            "title": "",
                            "cwd": "",
                            "output": [],
                            "more": false,
                            "exit": null,
                            "ended_ms": null,
                            "diff": [],
                            "app_id": "so.tilo.word-processor",
                            "record": "doc-1",
                            "execution": null,
                            "undo_label": null,
                            "undo_verb": null,
                        }
                    ],
                },
            ],
            "total": 2,
            "pending": false,
            "mutations": true,
        })
    }

    /// The MCP server: one agent let in, working now, reaching one workspace it made, and the
    /// workspace it reaches.
    fn server_of(endpoint: &str) -> Json {
        json!({
            "endpoint": endpoint,
            "grants": [{
                "id": "0b1c",
                "agent": "Claude Code",
                "reach": {"kind": "workspace", "id": "w1", "name": "Studio"},
                "granted_ms": 1_795_400_000_000_i64,
                "last_ms": 1_795_502_400_000_i64,
                "sessions": [{"name": "Claude Code #2", "started_ms": 1_795_500_600_000_i64}],
                "browser_pages": true,
                "stopped": false,
            }],
            "workspaces": [{
                "id": "w1",
                "name": "Studio",
                "made_by": {"kind": "agent", "grant": "0b1c", "agent": "Claude Code"},
                "made_ms": 1_795_400_000_000_i64,
                "open": "tilo://open#/@ada.tilo.cloud/w/studio",
            }],
        })
    }

    /// The text of a node's property, whatever option carries it.
    fn text(node: &UiNode, name: &str) -> String {
        match node.props.get(name) {
            Some(Value::Text(text)) => text.clone(),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Text(text) => text.clone(),
                other => panic!("{name} of {} is {other:?}", node.kind),
            },
            other => panic!("{name} of {} is {other:?}", node.kind),
        }
    }

    /// Every text the tree shows, in document order.
    fn drawn(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.Text")
            .into_iter()
            .filter_map(|node| match node.props.get("value") {
                Some(Value::Text(text)) => Some(text.clone()),
                Some(Value::Opt(Some(held))) => match &**held {
                    Value::Text(text) => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn shows(view: &ViewInstance, said: &str) -> bool {
        let mut shown = drawn(view);
        for kind in [
            "ui.ListRow",
            "ui.Section",
            "ui.Button",
            "ui.NavItem",
            "ui.Badge",
            "ui.EmptyState",
            "ui.Property",
        ] {
            for node in of_kind(view.tree(), kind) {
                for prop in [
                    "title",
                    "label",
                    "text",
                    "subtitle",
                    "secondary",
                    "meta",
                    "note",
                    "value",
                ] {
                    match node.props.get(prop) {
                        Some(Value::Text(text)) => shown.push(text.clone()),
                        Some(Value::Opt(Some(held))) => {
                            if let Value::Text(text) = &**held {
                                shown.push(text.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        shown.iter().any(|text| text.contains(said))
    }

    fn button(view: &ViewInstance, label: &str) -> UiNode {
        of_kind(view.tree(), "ui.Button")
            .into_iter()
            .find(|node| {
                node.props
                    .get("label")
                    .is_some_and(|held| matches!(held, Value::Text(text) if text == label))
            })
            .unwrap_or_else(|| panic!("no `{label}` button in:\n{}", dump(view.tree())))
            .clone()
    }

    fn press(view: &mut ViewInstance, label: &str) {
        let key = button(view, label).key.clone();
        view.fire(&key, "press").unwrap();
        view.poll_patches().unwrap();
    }

    fn row(view: &ViewInstance, title: &str) -> UiNode {
        of_kind(view.tree(), "ui.ListRow")
            .into_iter()
            .find(|node| text(node, "title") == title)
            .unwrap_or_else(|| panic!("no `{title}` row in:\n{}", dump(view.tree())))
            .clone()
    }

    fn activate(view: &mut ViewInstance, title: &str) {
        let key = row(view, title).key.clone();
        view.fire(&key, "activate").unwrap();
        view.poll_patches().unwrap();
    }

    /// Choose the place `label` in the sidebar, as its press does.
    fn choose(view: &mut ViewInstance, label: &str) {
        let key = of_kind(view.tree(), "ui.NavItem")
            .into_iter()
            .find(|node| text(node, "label") == label)
            .unwrap_or_else(|| panic!("no `{label}` place in:\n{}", dump(view.tree())))
            .key
            .clone();
        view.fire(&key, "select").unwrap();
        view.poll_patches().unwrap();
    }

    /// Every session is listed with what it is doing now, who asked for it and where, the working
    /// one under "Working now" and the rest under "Earlier".
    #[test]
    fn every_session_is_listed_with_its_place_and_its_client() {
        let console = Console::new(&sessions_of(), &server_of(""));
        let window = console.window();
        assert!(shows(&window, "Tidy the notes"), "{}", dump(window.tree()));
        assert!(shows(&window, "Draft the letter"));
        assert!(shows(&window, "Working now"));
        assert!(shows(&window, "Earlier"));
        assert!(shows(&window, "Running cargo test…"));
        assert!(shows(&window, "Started by Bo"));
        assert!(shows(&window, "Studio"));
        assert!(shows(&window, "Elsewhere"));
        // The clients that acted here are listed once each.
        assert!(shows(&window, "Clients"));
        assert!(shows(&window, "Claude Code"));
        assert!(shows(&window, "Cursor"));
    }

    /// A session opens as its timeline: what it was asked, what can be done with it, why, and each
    /// action with the undo it offers on its own.
    #[test]
    fn a_session_opens_as_its_timeline_and_its_commands() {
        let mut console = Console::new(&sessions_of(), &server_of(""));
        let mut window = console.window();
        activate(&mut window, "Tidy the notes");
        assert!(
            shows(&window, "Edited src/notes.rs"),
            "{}",
            dump(window.tree())
        );
        assert!(shows(&window, "Ran cargo test"));
        assert!(shows(&window, "Stop"));
        assert!(shows(&window, "Take over"));
        assert!(shows(&window, "Undo session"));
        assert!(shows(&window, "Undo this run"));
        assert!(shows(&window, "2 actions"));
        // Back to the list.
        press(&mut window, "Back");
        assert!(shows(&window, "Draft the letter"), "the list is back");
    }

    /// The sessions narrow to one client and one workspace, and "Show everything" brings the rest
    /// back.
    #[test]
    fn the_sessions_narrow_to_a_client_and_a_workspace() {
        let mut console = Console::new(&sessions_of(), &server_of(""));
        let mut window = console.window();
        press(&mut window, "Cursor");
        assert!(shows(&window, "Draft the letter"));
        assert!(!shows(&window, "Tidy the notes"), "{}", dump(window.tree()));
        press(&mut window, "Show everything");
        assert!(shows(&window, "Tidy the notes"));
        press(&mut window, "Studio");
        assert!(shows(&window, "Tidy the notes"));
        assert!(!shows(&window, "Draft the letter"));
    }

    /// A file's change opens to its lines, what it removed and what it added.
    #[test]
    fn a_file_change_shows_what_it_removed_and_added() {
        let mut console = Console::new(&sessions_of(), &server_of(""));
        let mut window = console.window();
        activate(&mut window, "Tidy the notes");
        activate(&mut window, "Edited src/notes.rs");
        assert!(shows(&window, "src/notes.rs"));
        assert!(shows(&window, "− old line"), "{}", dump(window.tree()));
        assert!(shows(&window, "+ new line"));
        assert!(shows(&window, "Open"));
    }

    /// A command opens to its last lines and its exit, and "Show all output" reads everything its
    /// shell kept.
    #[test]
    fn a_command_shows_its_output_and_reads_every_line() {
        let console = Console::new(&sessions_of(), &server_of(""));
        let output = ResourceRef::parse_id(
            "workspace.activity/output/-/so.tilo.terminal/shell-1/exec-1",
            "so.tilo.history#OutputReading",
        )
        .expect("the output's address");
        console
            .rt
            .set_reading(
                &output,
                r#"{"output":{"lines":["running 3 tests","ok"],"truncated":false,"unavailable":false,"running":false}}"#,
            )
            .expect("the output reads");
        let mut window = console.window();
        activate(&mut window, "Tidy the notes");
        activate(&mut window, "Ran cargo test");
        assert!(shows(&window, "$ cargo test"), "{}", dump(window.tree()));
        assert!(shows(&window, "running 3 tests"));
        assert!(shows(&window, "Exited with 0 after 2m"));
        press(&mut window, "Show all output");
        assert!(shows(&window, "Show less"), "{}", dump(window.tree()));
        assert!(shows(&window, "ok"), "{}", dump(window.tree()));
    }

    /// Connect shows where an agent reaches the server and every agent it let in, with the reach it
    /// was let in for and Revoke. A server with no endpoint offers setting the agent up instead.
    #[test]
    fn connect_shows_the_endpoint_and_every_grant() {
        let mut console = Console::new(&sessions_of(), &server_of("https://mcp.tilo.sh"));
        let mut window = console.window();
        choose(&mut window, "Connect");
        assert!(
            shows(&window, "https://mcp.tilo.sh"),
            "{}",
            dump(window.tree())
        );
        assert!(shows(
            &window,
            r#"{"mcpServers": {"tilo": {"type": "http", "url": "https://mcp.tilo.sh"}}}"#
        ));
        // Both values are the person's to copy into their agent's configuration.
        for label in ["Server address", "Any MCP client"] {
            let property = of_kind(window.tree(), "ui.Property")
                .into_iter()
                .find(|node| text(node, "label") == label)
                .unwrap_or_else(|| panic!("no `{label}` property in:\n{}", dump(window.tree())));
            assert!(
                matches!(property.props.get("copyable"), Some(Value::Int(1))),
                "`{label}` copies: {:?}",
                property.props.get("copyable")
            );
        }
        assert!(shows(&window, "Only Studio"));
        assert!(shows(&window, "operates web pages"));
        let _ = button(&window, "Revoke");

        let mut console = Console::new(&sessions_of(), &server_of(""));
        let mut window = console.window();
        choose(&mut window, "Connect");
        assert!(
            shows(&window, "Set up your agent"),
            "{}",
            dump(window.tree())
        );
    }

    /// Workspaces lists what the server reaches, who made it, and opens it in the full desktop.
    #[test]
    fn workspaces_lists_what_the_server_reaches() {
        let mut console = Console::new(&sessions_of(), &server_of("https://mcp.tilo.sh"));
        let mut window = console.window();
        choose(&mut window, "Workspaces");
        assert!(shows(&window, "Studio"), "{}", dump(window.tree()));
        assert!(shows(&window, "Made by Claude Code"));
        let _ = button(&window, "Open in Tilo");
    }

    /// Usage says what is not counted yet rather than showing nothing.
    #[test]
    fn usage_says_what_is_not_counted_yet() {
        let mut console = Console::new(&sessions_of(), &server_of(""));
        let mut window = console.window();
        choose(&mut window, "Usage");
        assert!(
            shows(&window, "No Linux machine time yet"),
            "{}",
            dump(window.tree())
        );
    }
}
