//! The Console app (`apps/tl/console/app.tl`) end to end: every agent session across the
//! workspaces with the MCP server that says which agents work now, each session and each client's
//! card as a resource of its own that a list, a link, search or another app opens, a session's
//! timeline with what can be undone and opened, a command's whole output, a file's change, the
//! agents the server let in with Revoke, the workspaces it reaches, and what is not counted yet.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::{Value as Json, json};
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, UiNode, Value, ViewEvent, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/console/app.tl");

    /// The platform's packages, as a host registers them for an app this runtime does not run.
    fn platform() -> Vec<tilolang::PackageMeta> {
        tilolang_tilo::platform_apps::platform_packages().expect("the platform packages read")
    }

    /// The id of the field `name` of the type `ty` of the platform package `app`: what a record
    /// value holds the field under.
    fn field_id_of(app: &str, ty: &str, name: &str) -> String {
        let package = platform()
            .into_iter()
            .find(|package| package.app == app)
            .unwrap_or_else(|| panic!("{app} is a platform package"));
        let declared = package
            .types
            .into_iter()
            .find(|declared| declared.name == ty)
            .unwrap_or_else(|| panic!("{ty} is a type of {app}"));
        declared
            .fields
            .into_iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{ty} has no {name}"))
            .id
    }

    /// Console over the platform's packages, its window open, the sessions and the MCP server as
    /// `sessions` and `server` say.
    struct Console {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        /// The history's reading of the sessions, which the window reads.
        sessions: ResourceRef,
        /// What the app's calls to other packages' actions reach.
        dispatch: Dispatch,
    }

    impl Console {
        fn new(sessions_json: &Json, server_json: &Json) -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            rt.set_clock(|| "2026-11-24T09:30:00Z".to_owned());
            let dispatch = Dispatch::new();
            rt.set_foreign_invoker(dispatch.clone());
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
            Self {
                rt,
                app,
                root,
                sessions,
                dispatch,
            }
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

        /// The root's collection `field`, each item as the resource it is.
        fn items(&self, field: &str) -> Vec<ResourceRef> {
            match self.rt.read(&self.root, field).expect("the window's field") {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Ref(r) => ResourceRef::clone(&r.target),
                        other => panic!("{field} holds {other:?}"),
                    })
                    .collect(),
                other => panic!("{field} is {other:?}"),
            }
        }

        /// The sessions the window published, each a resource of its own.
        fn links(&self) -> Vec<ResourceRef> {
            self.items("sessions")
        }

        /// The clients' cards the window published, each a resource of its own.
        fn cards(&self) -> Vec<ResourceRef> {
            self.items("cards")
        }

        /// The link whose session is `run`.
        fn link(&self, run: &str) -> ResourceRef {
            self.links()
                .into_iter()
                .find(|link| self.field(link, "run") == run)
                .unwrap_or_else(|| panic!("a resource of {run}"))
        }

        /// The link the session `run` opens, made by the action every path opens one by.
        fn session_link(&self, run: &str) -> ResourceRef {
            let action = self.app.action("session_link");
            let out = self
                .rt
                .invoke_action(
                    &me(),
                    (
                        action.as_str(),
                        vec![Value::resource(&self.root), Value::text(run)],
                    ),
                )
                .expect("the session's resource is made");
            returned_ref(out)
        }

        /// The card of `client`, made by the action every path opens one by.
        fn agent_card(&self, client: &str) -> ResourceRef {
            let action = self.app.action("agent_card");
            let out = self
                .rt
                .invoke_action(
                    &me(),
                    (
                        action.as_str(),
                        vec![Value::resource(&self.root), Value::text(client)],
                    ),
                )
                .expect("the client's card is made");
            returned_ref(out)
        }

        /// The card whose client is `client`.
        fn card(&self, client: &str) -> ResourceRef {
            self.cards()
                .into_iter()
                .find(|card| self.rt.title(card).unwrap_or_default() == client)
                .unwrap_or_else(|| panic!("{client}'s card"))
        }

        /// What the window's own clock does when it reads a session or a client it has not
        /// published yet.
        fn publish(&self) {
            let action = self.app.action("publish");
            self.rt
                .invoke_action(&me(), (action.as_str(), vec![Value::resource(&self.root)]))
                .expect("the window publishes");
        }

        /// Run the card command `name` on `card`, as its button, the palette or an agent does.
        fn command(&self, card: &ResourceRef, name: &str) {
            let action = self.app.action(name);
            let _ = self
                .rt
                .invoke_action(&me(), (action.as_str(), vec![Value::resource(card)]));
        }

        /// Run the package's action `name` for `client`, as a card, a palette or an agent does.
        fn client_command(&self, client: &str, name: &str) {
            let action = self.app.action(name);
            let _ = self.rt.invoke_action(
                &me(),
                (
                    action.as_str(),
                    vec![Value::resource(&self.root), Value::text(client)],
                ),
            );
        }

        /// A field's text, as the store holds it.
        fn field(&self, at: &ResourceRef, name: &str) -> String {
            self.rt
                .read(at, name)
                .unwrap_or_else(|why| panic!("{name} of {}: {why}", at.id_string()))
                .as_text()
                .unwrap_or_default()
                .to_owned()
        }

        /// The resource opened in the window its type's own default view makes.
        fn open(&self, resource: &ResourceRef) -> ViewInstance {
            let mut view = self
                .rt
                .open_resource(&me(), resource, None)
                .expect("the resource opens in its own view");
            view.set_lazy_rows(false).unwrap();
            view
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
                    "in_effect": [],
                    "changes_total": 0,
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
                    "in_effect": [],
                    "changes_total": 0,
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

    /// The sessions read before the history has answered: nothing listed yet, and it is reading.
    fn sessions_pending() -> Json {
        json!({"sessions": [], "total": 0, "pending": true, "mutations": true})
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
            "ui.ObjectHeader",
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

    /// The window's own clock ticking, as it does each second while the window shows.
    fn tick(view: &mut ViewInstance) {
        let every = of_kind(view.tree(), "ui.Timer")
            .into_iter()
            .find(|node| match node.props.get("every") {
                Some(Value::Float(seconds)) => (*seconds - 1.0).abs() < 1e-9,
                Some(Value::Int(seconds)) => *seconds == 1,
                _ => false,
            })
            .map(|node| node.key.clone())
            .unwrap_or_else(|| panic!("the window counts each second:\n{}", dump(view.tree())));
        view.fire(&every, "tick").unwrap();
        view.poll_patches().unwrap();
    }

    /// The resource the window's last press opened.
    fn opened(view: &mut ViewInstance) -> ResourceRef {
        match view.poll_events().as_slice() {
            [ViewEvent::Open { target, .. }] => ResourceRef::clone(target),
            other => panic!("the press opens a resource, not {other:?}"),
        }
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

    /// Activating a session opens its own resource: the timeline of what it was asked, what can
    /// be done with it, why, and each action with the undo it offers on its own. The list it was
    /// chosen from stays where it was.
    #[test]
    fn a_session_opens_as_its_timeline_in_a_window_of_its_own() {
        let console = Console::new(&sessions_of(), &server_of(""));
        let mut window = console.window();
        activate(&mut window, "Tidy the notes");
        let link = opened(&mut window);
        assert_eq!(
            console.rt.title(&link).unwrap(),
            "Tidy the notes",
            "the session's own words name its resource"
        );
        let view = console.open(&link);
        assert!(shows(&view, "Edited src/notes.rs"), "{}", dump(view.tree()));
        assert!(shows(&view, "Ran cargo test"));
        assert!(shows(&view, "Stop"));
        assert!(shows(&view, "Take over"));
        assert!(shows(&view, "Undo session"));
        assert!(shows(&view, "Undo this run"));
        assert!(shows(&view, "2 actions"));
        // The window that listed it still stands on the list.
        assert!(
            shows(&window, "Draft the letter"),
            "the list is still there"
        );
    }

    /// The sessions narrow to one client and one workspace, and "Show everything" brings the rest
    /// back.
    #[test]
    fn the_sessions_narrow_to_a_client_and_a_workspace() {
        let console = Console::new(&sessions_of(), &server_of(""));
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
        let console = Console::new(&sessions_of(), &server_of(""));
        let mut window = console.window();
        activate(&mut window, "Tidy the notes");
        let link = opened(&mut window);
        let mut view = console.open(&link);
        activate(&mut view, "Edited src/notes.rs");
        assert!(shows(&view, "src/notes.rs"));
        assert!(shows(&view, "− old line"), "{}", dump(view.tree()));
        assert!(shows(&view, "+ new line"));
        assert!(shows(&view, "Open"));
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
        let link = opened(&mut window);
        let mut view = console.open(&link);
        activate(&mut view, "Ran cargo test");
        assert!(shows(&view, "$ cargo test"), "{}", dump(view.tree()));
        assert!(shows(&view, "running 3 tests"));
        assert!(shows(&view, "Exited with 0 after 2m"));
        press(&mut view, "Show all output");
        assert!(shows(&view, "Show less"), "{}", dump(view.tree()));
        assert!(shows(&view, "ok"), "{}", dump(view.tree()));
    }

    /// Connect shows where an agent reaches the server and every agent it let in, with the reach it
    /// was let in for and Revoke. A server with no endpoint offers setting the agent up instead.
    #[test]
    fn connect_shows_the_endpoint_and_every_grant() {
        let console = Console::new(&sessions_of(), &server_of("https://mcp.tilo.sh"));
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

        let console = Console::new(&sessions_of(), &server_of(""));
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
        let console = Console::new(&sessions_of(), &server_of("https://mcp.tilo.sh"));
        let mut window = console.window();
        choose(&mut window, "Workspaces");
        assert!(shows(&window, "Studio"), "{}", dump(window.tree()));
        assert!(shows(&window, "Made by Claude Code"));
        let _ = button(&window, "Open in Tilo");
    }

    /// Usage says what is not counted yet rather than showing nothing.
    #[test]
    fn usage_says_what_is_not_counted_yet() {
        let console = Console::new(&sessions_of(), &server_of(""));
        let mut window = console.window();
        choose(&mut window, "Usage");
        assert!(
            shows(&window, "No Linux machine time yet"),
            "{}",
            dump(window.tree())
        );
    }

    /// Every session the history lists has one resource of its own, named by what the session was
    /// asked; reading the list again leaves the same resources where they stand, and each opens
    /// its own session's page.
    #[test]
    fn every_session_has_a_resource_of_its_own() {
        let console = Console::new(&sessions_of(), &server_of(""));
        let mut window = console.window();
        // The window reads the history: its clock gives what it lists a resource each.
        tick(&mut window);
        window.poll_patches().unwrap();

        let links = console.links();
        assert_eq!(links.len(), 2, "one resource per session: {links:?}");
        let mut runs: Vec<String> = links
            .iter()
            .map(|link| console.field(link, "run"))
            .collect();
        runs.sort();
        assert_eq!(runs, ["run-1", "run-2"]);
        let named = console.link("run-2");
        assert_eq!(
            console.rt.title(&named).unwrap(),
            "Tidy the notes",
            "the session's own words name its resource"
        );
        let ids: Vec<String> = links.iter().map(ResourceRef::id_string).collect();

        // The history answers again: the resources already standing are those, unchanged.
        console.publish();
        let again: Vec<String> = console.links().iter().map(ResourceRef::id_string).collect();
        assert_eq!(again, ids, "no session's resource is made twice");

        // Each resource opens its own session's page, and no other's.
        let view = console.open(&named);
        assert!(shows(&view, "Edited src/notes.rs"), "{}", dump(view.tree()));
        assert!(shows(&view, "Ran cargo test"));
        let other = console.open(&console.link("run-1"));
        assert!(shows(&other, "Draft the letter"), "{}", dump(other.tree()));
        assert!(
            !shows(&other, "Tidy the notes"),
            "the other session's page is its own"
        );
        assert_eq!(
            console.link("run-2").id_string(),
            named.id_string(),
            "the link every path names is the one resource"
        );
    }

    /// A link names a session before the history has answered: its resource exists, says it is
    /// reading, and fills in when the answer lands; one the history never lists says so.
    #[test]
    fn a_link_opens_before_the_history_answers() {
        let console = Console::new(&sessions_pending(), &server_of(""));
        let link = console.session_link("run-2");
        assert_eq!(
            console.rt.title(&link).unwrap(),
            "Agent session",
            "nothing names it until the history lists it"
        );
        let mut view = console.open(&link);
        assert!(
            shows(&view, "Reading this session"),
            "{}",
            dump(view.tree())
        );

        console
            .rt
            .set_reading(&console.sessions, &sessions_of().to_string())
            .expect("the history answers");
        view.poll_patches().unwrap();
        assert!(shows(&view, "Edited src/notes.rs"), "{}", dump(view.tree()));
        assert!(!shows(&view, "Reading this session"), "the page filled in");

        // A session the history never lists keeps the state that says so.
        let missing = console.session_link("run-9");
        let view = console.open(&missing);
        assert!(
            shows(&view, "This session is not here"),
            "{}",
            dump(view.tree())
        );
    }

    /// Each client's card is a resource of its own, named by the agent, showing that client's
    /// work, commands and access and no other's.
    #[test]
    fn each_clients_card_is_a_resource_of_its_own() {
        let console = Console::new(&sessions_of(), &server_of("https://mcp.tilo.sh"));
        let mut window = console.window();
        tick(&mut window);
        window.poll_patches().unwrap();

        let cards = console.cards();
        assert_eq!(cards.len(), 2, "one card per client: {cards:?}");
        let claude = console.card("Claude Code");
        let cursor = console.card("Cursor");
        assert_eq!(
            console.rt.title(&claude).unwrap(),
            "Claude Code",
            "the agent's name names its card's window"
        );
        assert_ne!(
            claude.id_string(),
            cursor.id_string(),
            "two clients, two cards"
        );
        assert_eq!(
            console.agent_card("Cursor").id_string(),
            cursor.id_string(),
            "the action every path opens a card by makes the one card"
        );

        let view = console.open(&claude);
        assert!(shows(&view, "Only Studio"), "{}", dump(view.tree()));
        assert!(shows(&view, "Stop"));
        assert!(shows(&view, "Take over"));
        assert!(shows(&view, "Undo session"));
        assert!(shows(&view, "Tidy the notes"));
        assert!(
            !shows(&view, "Draft the letter"),
            "the other client's work is not here"
        );

        let theirs = console.open(&cursor);
        assert!(
            shows(&theirs, "Draft the letter"),
            "{}",
            dump(theirs.tree())
        );
        assert!(!shows(&theirs, "Tidy the notes"));
        assert!(
            !shows(&theirs, "Stop"),
            "it came in by no grant, so has none to stop"
        );
    }

    /// The card's commands are actions of the card, published with it: a button, the palette and
    /// an agent call the same ones, and none is bound to a key chord the card never had.
    #[test]
    fn a_cards_commands_are_its_published_actions() {
        let console = Console::new(&sessions_of(), &server_of(""));
        for (name, destructive) in [
            ("stop_agent", true),
            ("take_over_agent", false),
            ("undo_agent_session", true),
            ("revoke_agent", true),
            ("let_agent_go_on", false),
        ] {
            let action = console
                .app
                .meta()
                .action_named(name)
                .unwrap_or_else(|| panic!("{name} is an action of the package"));
            assert_eq!(
                action.params.first().map(|param| param.ty.as_str()),
                Some("AgentCard"),
                "{name} acts on the client's card"
            );
            assert_eq!(
                action.role.as_deref() == Some("destructive"),
                destructive,
                "{name}'s look"
            );
            assert!(action.key.is_none(), "{name} declares no shortcut");
        }
    }

    /// A client's command is Console's own action by the client a card, a palette or an agent
    /// names: it makes that client's card and acts on it, so the stop reaches that client only.
    #[test]
    fn a_clients_command_acts_on_that_client_only() {
        let console = Console::new(&sessions_of(), &server_of("https://mcp.tilo.sh"));
        console.publish();

        console.client_command("Claude Code", "undo_client_session");
        let call = console.dispatch.waiting();
        assert_eq!(call.app(), "so.tilo.history", "the undo the history offers");
        assert!(call.name().contains("undo"), "{}", call.name());
        let (_, args) = console.dispatch.asked();
        assert_eq!(
            args.get(1).and_then(Value::as_text),
            Some("aa"),
            "Claude Code's run"
        );
        assert_eq!(args.get(2).and_then(Value::as_text), Some("bb"));

        console.client_command("Cursor", "undo_client_session");
        let (_, args) = console.dispatch.asked();
        assert_eq!(
            args.get(1).and_then(Value::as_text),
            Some("01"),
            "Cursor's run, not the other client's"
        );
        assert_eq!(args.get(2).and_then(Value::as_text), Some("02"));

        // The stop ends the working session of the named client, and no other's.
        console.client_command("Claude Code", "stop_client");
        let call = console.dispatch.waiting();
        assert_eq!(call.app(), "tilo.mcp-server", "the stop the server answers");
        let (_, args) = console.dispatch.asked();
        let Some(Value::Rec(session)) = args.get(1) else {
            panic!("the stop names the session: {:?}", args.get(1));
        };
        let said = |name: &str| {
            session
                .fields
                .get(&field_id_of("tilo.mcp-server", "AgentSessionRef", name))
                .and_then(Value::as_text)
        };
        assert_eq!(said("session"), Some("Claude Code #2"));
        assert_eq!(said("grant"), Some("0b1c"));

        let before = console.dispatch.len();
        console.client_command("Cursor", "stop_client");
        assert_eq!(
            console.dispatch.len(),
            before,
            "Cursor came in by no grant, so has none to stop"
        );
    }

    /// The card offers the system's Copy link among its own actions: the press is the system's
    /// copy link on the card, which the window's host mints and copies, not an action the app
    /// writes. `card.share_link()` compiles against the verb every resource has.
    #[test]
    fn a_cards_copy_link_is_the_systems() {
        let console = Console::new(&sessions_of(), &server_of(""));
        console.publish();
        let card = console.card("Claude Code");
        let mut view = console.open(&card);
        let key = button(&view, "Copy link").key.clone();
        view.fire(&key, "press").unwrap();
        view.poll_patches().unwrap();
        match view.poll_events().as_slice() {
            [ViewEvent::CopyLink { target, app }] => {
                assert_eq!(
                    target.id_string(),
                    card.id_string(),
                    "the link is the card's own"
                );
                assert_eq!(app, "so.tilo.console", "minted as the app's resource");
            }
            other => panic!("the press copies the card's link, not {other:?}"),
        }
    }

    /// Each command acts on the client its card is of: the undo takes back that client's newest
    /// run, and the stop ends that client's session, through the same actions the card's buttons
    /// call.
    #[test]
    fn a_cards_command_acts_on_that_client_only() {
        let console = Console::new(&sessions_of(), &server_of("https://mcp.tilo.sh"));
        console.publish();
        let claude = console.card("Claude Code");
        let cursor = console.card("Cursor");

        console.command(&claude, "undo_agent_session");
        let call = console.dispatch.waiting();
        assert_eq!(call.app(), "so.tilo.history", "the undo the history offers");
        assert!(call.name().contains("undo"), "{}", call.name());
        let (_, args) = console.dispatch.asked();
        assert_eq!(
            args.get(1).and_then(Value::as_text),
            Some("aa"),
            "Claude Code's run"
        );
        assert_eq!(args.get(2).and_then(Value::as_text), Some("bb"));

        console.command(&cursor, "undo_agent_session");
        let (_, args) = console.dispatch.asked();
        assert_eq!(
            args.get(1).and_then(Value::as_text),
            Some("01"),
            "Cursor's run, not the other client's"
        );
        assert_eq!(args.get(2).and_then(Value::as_text), Some("02"));

        // Stop ends the working session of the card's own client, and only that.
        console.command(&claude, "stop_agent");
        let call = console.dispatch.waiting();
        assert_eq!(call.app(), "tilo.mcp-server", "the stop the server answers");
        let (_, args) = console.dispatch.asked();
        let Some(Value::Rec(session)) = args.get(1) else {
            panic!("the stop names the session: {:?}", args.get(1));
        };
        let said = |name: &str| {
            session
                .fields
                .get(&field_id_of("tilo.mcp-server", "AgentSessionRef", name))
                .and_then(Value::as_text)
        };
        assert_eq!(said("session"), Some("Claude Code #2"));
        assert_eq!(said("grant"), Some("0b1c"));

        let before = console.dispatch.len();
        console.command(&cursor, "stop_agent");
        assert_eq!(
            console.dispatch.len(),
            before,
            "Cursor's card came in by no grant, so has none to stop"
        );
    }
}
