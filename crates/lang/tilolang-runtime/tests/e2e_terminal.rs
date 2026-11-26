//! The Terminal app (`apps/tl/terminal/app.tl`) end to end, against a device engine that answers
//! as the terminal engine does: a window opening the shell where its session runs, the runs its
//! shell marks recorded and recalled on Up, the shell's asks (its history, a command it holds),
//! the machine's secrets, and the windows that draw it all.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::common::*;
    use serde_json::{Value as Json, json};
    use tilolang_runtime::files::{Entry, FilesHost};
    use tilolang_runtime::host_work::{EngineHost, JobHandlers};
    use tilolang_runtime::{
        Actor, AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewEvent, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/terminal/app.tl");

    /// The relay session a carried terminal re-attaches to: a relay session's id is 32 hex
    /// characters.
    const RELAY: &str = "0123456789abcdef0123456789abcdef";

    /// What a run the grid reports prints, and the failing test its tool found in it.
    const TEST_OUTPUT: &str = "running 2 tests\ntest parse::reads_a_line ... ok\ntest parse::reads_a_long_line ... FAILED";

    /// What the run's parsers found: one test that passed, one that failed.
    fn test_items() -> Json {
        json!([
            {"kind": "test-passed", "text": "parse::reads_a_line", "path": "src/parse.rs",
             "line": 71, "column": 5, "code": "parse::reads_a_line"},
            {"kind": "test-failed", "text": "parse::reads_a_long_line",
             "path": "src/parse.rs", "line": 88, "column": 5, "code": "parse::reads_a_long_line"},
        ])
    }

    /// A run as the grid reports it, as the shell marked it.
    fn reported(
        command: &str,
        cwd: &str,
        exit: i64,
        ended_ms: i64,
        output: &str,
        items: Json,
    ) -> String {
        json!({
            "command": command,
            "cwd": cwd,
            "started_ms": ended_ms - 5,
            "ended_ms": ended_ms,
            "exit": exit,
            "output": output,
            "truncated": false,
            "tool": "cargo",
            "undoable": false,
            "items": items,
            "job": 0,
        })
        .to_string()
    }

    /// A `cargo test` run with the failing test a session's rows show.
    fn failing_test_run(ended_ms: i64) -> String {
        reported(
            "cargo test",
            "/home/ada/src/tilo",
            101,
            ended_ms,
            TEST_OUTPUT,
            test_items(),
        )
    }

    /// The device's terminal engine: what it was asked, and what it answers.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        /// What each started job named as the action that runs when it ends.
        jobs: Rc<RefCell<Vec<JobHandlers>>>,
        /// The words the next open is refused with, once.
        refuse_open: Rc<RefCell<Option<String>>>,
        /// Whether the shell a session names has ended, as `inspect` answers.
        ended: Rc<RefCell<bool>>,
        /// How many shells this device opened, so each has a name of its own.
        opened: Rc<RefCell<u64>>,
        /// The relay a carried session's shell opens on, as the device answers.
        carries: Rc<RefCell<Option<String>>>,
    }

    impl Engine {
        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(op, _)| op.clone())
                .collect()
        }

        /// What the last call of `op` was asked, as JSON.
        fn asked_of(&self, op: &str) -> Json {
            self.asks(op)
                .pop()
                .unwrap_or_else(|| panic!("the engine was never asked `{op}`: {:?}", self.ops()))
        }

        /// What every call of `op` was asked, in order.
        fn asks(&self, op: &str) -> Vec<Json> {
            self.asked
                .borrow()
                .iter()
                .filter(|(asked, _)| asked == op)
                .map(|(_, input)| serde_json::from_str(input).unwrap_or(Json::Null))
                .collect()
        }

        fn count(&self, op: &str) -> usize {
            self.asked
                .borrow()
                .iter()
                .filter(|(asked, _)| asked == op)
                .count()
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            let asked: Json = serde_json::from_str(input).unwrap_or(Json::Null);
            match op {
                "open" => {
                    if let Some(why) = self.refuse_open.borrow_mut().take() {
                        return Err(why);
                    }
                    *self.opened.borrow_mut() += 1;
                    // Where a session runs when it names nowhere of its own: the workspace's
                    // default environment.
                    let target = asked["target"].as_str().unwrap_or_default();
                    let target = if target.is_empty() {
                        "environment"
                    } else {
                        target
                    };
                    Ok(json!({
                        "shell": format!("shell-{}", self.opened.borrow()),
                        "shell_name": "ush",
                        "target": target,
                        "relay_session": self
                            .carries
                            .borrow()
                            .clone()
                            .unwrap_or_else(|| asked["attach"].as_str().unwrap_or_default().to_owned()),
                    })
                    .to_string())
                }
                "inspect" => Ok(json!({
                    "shell": asked["shell"],
                    "shell_name": "ush",
                    "relay_session": "",
                    "ended": *self.ended.borrow(),
                })
                .to_string()),
                "environments" => Ok(r#"{"available":true,"self_hosted":true}"#.to_owned()),
                // The way a tool runs one of its tests: its own command line for that test.
                "rerun_test" => Ok(json!({
                    "command": format!(
                        "{} -- --exact {}",
                        asked["command"].as_str().unwrap_or_default(),
                        asked["test"].as_str().unwrap_or_default(),
                    ),
                    "test": asked["test"],
                })
                .to_string()),
                "read_screen" => Ok(r#"{"text":"","truncated":false,"links":[]}"#.to_owned()),
                _ => Ok("{}".to_owned()),
            }
        }

        fn start(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            handlers: &JobHandlers,
        ) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            match op {
                "run" => {
                    self.jobs.borrow_mut().push(handlers.clone());
                    Ok(format!("run-{}", self.jobs.borrow().len()))
                }
                other => Err(format!("`{other}` is no job this device starts")),
            }
        }

        fn job(&self, _job: &str) -> Result<Option<Json>, String> {
            Ok(None)
        }

        fn cancel(&self, _job: &str) -> bool {
            false
        }
    }

    /// The device's files, as the tools this app reaches them: the links it opened, in order.
    #[derive(Clone, Default)]
    struct Disk {
        opened: Rc<RefCell<Vec<String>>>,
    }

    impl FilesHost for Disk {
        fn list(&self, path: &str) -> Result<Vec<Entry>, String> {
            Err(format!("`{path}` is not a folder on this device"))
        }

        fn stat(&self, _path: &str) -> Result<Option<Entry>, String> {
            Ok(None)
        }

        fn read(&self, path: &str) -> Result<Vec<u8>, String> {
            Err(format!("`{path}` is not read here"))
        }

        fn watch(&self, _path: &str, _run: &str) -> Result<String, String> {
            Err("no watch".to_owned())
        }

        fn open_link(&self, url: &str) -> Result<(), String> {
            self.opened.borrow_mut().push(url.to_owned());
            Ok(())
        }
    }

    struct Term {
        rt: Runtime,
        app: App,
        terminal: ResourceRef,
        session: ResourceRef,
        machine: ResourceRef,
        /// What this device knows of where its terminals run.
        here: ResourceRef,
        engine: Engine,
        disk: Disk,
    }

    impl Term {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            rt.set_clock(|| "2026-11-26T09:00:00Z".to_owned());
            let engine = Engine::default();
            rt.set_engine(engine.clone());
            let disk = Disk::default();
            rt.set_device_files(disk.clone());
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            // The workspace's policy for this app: what it asks of the device is its own work,
            // so the person has already said yes to it.
            for action in &app.meta().actions {
                authority.grant_outside(&action.id);
            }
            let terminal = rt.create_root(&type_id(&app, "Terminal")).unwrap();
            let session = rt.create_root(&type_id(&app, "Session")).unwrap();
            let machine = rt.create_root(&type_id(&app, "MachineSecrets")).unwrap();
            let here_ty = type_id(&app, "Here");
            let here = rt
                .create_root_at(&rt.device_root_doc(&app.meta().app, &here_ty), &here_ty)
                .unwrap();
            Self {
                rt,
                app,
                terminal,
                session,
                machine,
                here,
                engine,
                disk,
            }
        }

        /// A window whose session runs in `target`, its shell open there.
        fn at(target: &str) -> Self {
            let t = Self::new();
            t.call(
                "record_target",
                &[("s", t.s()), ("target", Value::text(target))],
            );
            t.call("attach", &[("s", t.s())]);
            t
        }

        /// A window whose session runs in the workspace's environment, its shell open there.
        fn opened() -> Self {
            Self::at("environment")
        }

        fn s(&self) -> Value {
            Value::resource(&self.session)
        }

        fn call(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            self.call_as(&me(), action, given)
        }

        fn call_as(&self, who: &Actor, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            match self.tried(who, action, given) {
                Ok(outcome) => outcome,
                Err(why) => panic!("{action} did not run: {why}"),
            }
        }

        fn tried(
            &self,
            who: &Actor,
            action: &str,
            given: &[(&str, Value)],
        ) -> Result<InvokeOutcome, String> {
            let args = self.app.args(&self.rt, action, given);
            let id = self.app.action(action);
            let outcome = self
                .rt
                .invoke_action(who, (id.as_str(), args))
                .map_err(|why| why.to_string())?;
            match outcome {
                InvokeOutcome::Done(_) => Ok(outcome),
                other => Err(format!("{other:?}")),
            }
        }

        fn refused(&self, action: &str, given: &[(&str, Value)]) {
            self.refused_as(&me(), action, given);
        }

        /// `action` as `who` is refused: by its own words, or by the host's.
        fn refused_as(&self, who: &Actor, action: &str, given: &[(&str, Value)]) {
            match self.tried(who, action, given) {
                Err(_) => {}
                Ok(other) => panic!("{action} was taken: {other:?}"),
            }
        }

        /// How the window's session stands, as the app says it.
        fn state(&self) -> String {
            match self.call("inspect_session", &[("s", self.s())]) {
                InvokeOutcome::Done(Value::Text(said)) => said,
                other => panic!("inspect_session answered {other:?}"),
            }
        }

        fn read(&self, r: &ResourceRef, field: &str) -> Value {
            self.rt
                .read(r, field)
                .unwrap_or_else(|why| panic!("{field} does not read: {why}"))
        }

        fn text(&self, r: &ResourceRef, field: &str) -> String {
            match self.read(r, field) {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// What a run printed, as its record keeps it: bytes, read as the text they are.
        fn output(&self, r: &ResourceRef) -> String {
            match self.read(r, "output") {
                Value::Bytes(bytes) => String::from_utf8(bytes.to_vec())
                    .unwrap_or_else(|why| panic!("the output is text: {why}")),
                other => panic!("output reads {other:?}"),
            }
        }

        fn int(&self, r: &ResourceRef, field: &str) -> i64 {
            match self.read(r, field) {
                Value::Int(number) => number,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// What the list `field` of `r` holds, in order: resources, or records of `ty` held by
        /// value.
        fn list(&self, r: &ResourceRef, field: &str) -> Vec<Value> {
            match self.read(r, field) {
                Value::List(items) => items,
                other => panic!("{field} is {other:?}"),
            }
        }

        /// The field `name` of `item`, a record of the type `ty` held by value in a list.
        fn rec(&self, item: &Value, ty: &str, name: &str) -> Value {
            let field = field_id(&self.app, ty, name);
            match item {
                Value::Rec(record) => record
                    .fields
                    .get(&field)
                    .cloned()
                    .unwrap_or_else(|| panic!("{ty}.{name} is not set: {record:?}")),
                other => panic!("{ty} reads {other:?}"),
            }
        }

        /// The resources the collection `field` of `r` holds, in order.
        fn items(&self, r: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.read(r, field) {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Ref(r) => Rc::unwrap_or_clone(r.target),
                        other => panic!("{field} holds {other:?}"),
                    })
                    .collect(),
                other => panic!("{field} is {other:?}"),
            }
        }

        /// The app's window over the window's session.
        fn window(&self) -> ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("session_window"),
                    vec![Value::resource(&self.session)],
                )
                .expect("the terminal's window opens")
        }

        /// The run the job `job` started ended, saying `run` as the engine reports it: the action
        /// the start named runs, as `who` started it.
        fn run_ended(&self, who: &Actor, job: &str, run: &str) {
            let kept = self
                .engine
                .jobs
                .borrow()
                .last()
                .expect("a run was started")
                .done
                .clone()
                .expect("the run names what runs when it ends");
            let said = json!({
                "state": "done",
                "run": serde_json::from_str::<Json>(run).unwrap(),
            })
            .to_string();
            let record = tilolang_engine::record_of(job, &said).unwrap();
            let call = self.rt.job_call(&kept, &record).unwrap();
            self.rt
                .invoke_action(who, call)
                .unwrap_or_else(|why| panic!("the run's end was refused: {why}"));
        }

        /// The key of the grid the window draws.
        fn grid(&self, window: &ViewInstance) -> String {
            of_kind(window.tree(), "ui.Terminal")
                .first()
                .unwrap_or_else(|| panic!("the window draws no grid: {}", dump(window.tree())))
                .key
                .clone()
        }
    }

    /// Every text a window's nodes carry, in document order.
    fn words(node: &UiNode) -> Vec<String> {
        let mut out = Vec::new();
        fn go(node: &UiNode, out: &mut Vec<String>) {
            for value in node.props.values() {
                if let Value::Text(text) = value {
                    out.push(text.clone());
                }
            }
            for child in &node.children {
                go(child, out);
            }
        }
        go(node, &mut out);
        out
    }

    fn shows(node: &UiNode, word: &str) -> bool {
        words(node).iter().any(|shown| shown == word)
    }

    /// What a node's property reads, whether the widget takes it plainly or as a choice.
    fn prop(node: &UiNode, name: &str) -> Option<String> {
        match node.props.get(name) {
            Some(Value::Text(text)) => Some(text.clone()),
            Some(Value::Opt(Some(inner))) => inner.as_text().map(str::to_owned),
            _ => None,
        }
    }

    /// The first node of `kind` in the tree whose `prop` is `value`.
    fn node_with<'a>(node: &'a UiNode, kind: &str, name: &str, value: &str) -> &'a UiNode {
        of_kind(node, kind)
            .into_iter()
            .find(|found| prop(found, name).as_deref() == Some(value))
            .unwrap_or_else(|| panic!("no {kind} whose {name} is {value}: {}", dump(node)))
    }

    tilo_test_env::scenario! {
        fn opening_a_window_opens_the_shell_where_its_session_runs;

        phase the_window_asks_the_device_for_a_shell_where_the_session_runs;
        {
            let t = Term::new();
            t.call(
                "record_target",
                &[("s", t.s()), ("target", Value::text("environment"))],
            );
            t.call("attach", &[("s", t.s())]);
            // The device is asked for a shell where the session runs, at the grid's first size,
            // for the person at the window, naming the session it opens it for.
            let asked = t.engine.asked_of("open");
            assert_eq!(asked["target"], "environment", "{asked}");
            assert_eq!(asked["record"], t.session.id_string(), "{asked}");
            assert_eq!(asked["columns"], 100, "{asked}");
            assert_eq!(asked["rows"], 30, "{asked}");
            assert_eq!(asked["agent"], false, "{asked}");
            assert_eq!(asked["attach"], "", "{asked}");
            assert_eq!(t.engine.count("environments"), 1, "{:?}", t.engine.ops());
            // The session keeps the shell it was given, on this device, held by the person.
            assert_eq!(t.text(&t.session, "shell"), "shell-1");
            assert_eq!(t.text(&t.session, "shell_name"), "ush");
            let device = t.text(&t.here, "device");
            assert!(!device.is_empty(), "the device named itself when it opened a shell");
            assert_eq!(t.text(&t.session, "device"), device);
            assert_eq!(
                t.read(&t.session, "holder"),
                Value::some(Value::Actor(me())),
                "the person at the window holds it"
            );
            assert_eq!(t.state(), "open: Default environment");
            // The window shows that shell's grid, and where it runs.
            let window = t.window();
            let grid = node_with(window.tree(), "ui.Terminal", "session", "shell-1");
            assert_eq!(
                prop(grid, "label").as_deref(),
                Some("Terminal, Default environment"),
                "the grid says where it runs"
            );
            assert!(shows(window.tree(), "Default environment"));
        }

        phase a_shell_that_does_not_start_says_why_and_tries_again;
        {
            let t = Term::new();
            *t.engine.refuse_open.borrow_mut() = Some("The workspace's machine is not running".to_owned());
            t.call("attach", &[("s", t.s())]);
            assert_eq!(
                t.state(),
                "failed: Default environment, The workspace's machine is not running"
            );
            let mut window = t.window();
            for word in [
                "The shell couldn't start",
                "The workspace's machine is not running",
                "Try again",
            ] {
                assert!(shows(window.tree(), word), "{word} is shown: {}", dump(window.tree()));
            }
            // Trying again opens a shell, and the window shows its grid.
            let again = node_with(window.tree(), "ui.EmptyState", "action", "Try again").key.clone();
            assert!(matches!(window.fire(&again, "act").unwrap(), InvokeOutcome::Done(_)));
            window.poll_patches().unwrap();
            assert_eq!(t.state(), "open: Default environment");
            assert_eq!(t.engine.count("open"), 2);
            assert_eq!(t.text(&t.session, "shell"), "shell-1");
            node_with(window.tree(), "ui.Terminal", "session", "shell-1");
        }
    }

    tilo_test_env::scenario! {
        fn a_session_this_device_still_runs_is_rejoined_and_one_elsewhere_is_only_shown;

        phase reopening_a_window_rejoins_the_shell_this_device_runs;
        {
            let t = Term::opened();
            t.call("attach", &[("s", t.s())]);
            // The device is asked whether that shell still runs, and no second one opens.
            assert_eq!(t.engine.asked_of("inspect")["shell"], "shell-1");
            assert_eq!(t.engine.count("open"), 1, "{:?}", t.engine.ops());
            assert_eq!(t.state(), "open: Default environment");
            // A shell that ended is opened again.
            *t.engine.ended.borrow_mut() = true;
            t.call("attach", &[("s", t.s())]);
            assert_eq!(t.engine.count("open"), 2, "{:?}", t.engine.ops());
            assert_eq!(t.text(&t.session, "shell"), "shell-2");
            assert_eq!(t.state(), "open: Default environment");
        }

        phase a_session_another_device_holds_shows_its_commands_and_opens_nothing;
        {
            let t = Term::opened();
            t.call(
                "finished",
                &[
                    ("s", t.s()),
                    ("said", Value::text(failing_test_run(900))),
                ],
            );
            t.call(
                "record_target",
                &[
                    ("s", t.s()),
                    ("target", Value::text("environment")),
                    ("device", Value::some(Value::text("another-device"))),
                ],
            );
            t.call("attach", &[("s", t.s())]);
            assert_eq!(t.engine.count("open"), 1, "{:?}", t.engine.ops());
            assert_eq!(t.state(), "elsewhere: Default environment");
            let window = t.window();
            assert!(
                shows(
                    window.tree(),
                    "This session runs on another device"
                ),
                "the window says where it runs: {}",
                dump(window.tree())
            );
            assert!(
                of_kind(window.tree(), "ui.Terminal").is_empty(),
                "it opens no shell of its own: {}",
                dump(window.tree())
            );
            assert!(
                shows(window.tree(), "$ cargo test"),
                "it shows the commands the session ran: {}",
                dump(window.tree())
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_finished_run_is_kept_and_its_failing_test_runs_again;

        phase the_grid_reports_a_run_and_both_the_session_and_the_workspace_keep_it;
        {
            let t = Term::opened();
            let window = t.window();
            let grid = t.grid(&window);
            window
                .fire_with(
                    &grid,
                    "finished",
                    Some(&Value::text(failing_test_run(900))),
                )
                .expect("the grid reports the run it finished");
            // The session keeps the run as its shell marked it.
            let commands = t.items(&t.session, "commands");
            assert_eq!(commands.len(), 1);
            let run = &commands[0];
            assert_eq!(t.text(run, "command"), "cargo test");
            assert_eq!(t.text(run, "cwd"), "/home/ada/src/tilo");
            assert_eq!(t.int(run, "exit"), 101);
            assert_eq!(t.output(run), TEST_OUTPUT);
            assert_eq!(t.text(run, "tool"), "cargo");
            let items = t.list(run, "items");
            assert_eq!(items.len(), 2, "both tests its parser found");
            assert_eq!(
                t.rec(&items[1], "CommandItem", "code"),
                Value::text("parse::reads_a_long_line")
            );
            assert_eq!(
                t.rec(&items[1], "CommandItem", "path"),
                Value::text("src/parse.rs")
            );
            assert_eq!(t.rec(&items[1], "CommandItem", "line"), Value::Int(88));
            assert_eq!(t.rec(&items[1], "CommandItem", "column"), Value::Int(5));
            // The workspace's history keeps the command line, for Up to recall.
            let history = t.items(&t.terminal, "history");
            assert_eq!(history.len(), 1);
            assert_eq!(t.text(&history[0], "command"), "cargo test");
            assert_eq!(t.text(&history[0], "cwd"), "/home/ada/src/tilo");
            assert_eq!(t.text(&history[0], "environment"), "environment");
            assert_eq!(t.int(&history[0], "exit"), 101);
            assert_eq!(t.text(&history[0], "session"), t.session.id_string());
        }

        phase a_command_row_shows_how_it_ended_and_runs_its_failing_test_again;
        {
            let t = Term::opened();
            t.call(
                "finished",
                &[("s", t.s()), ("said", Value::text(failing_test_run(900)))],
            );
            // The rows of commands a window shows are those of a session it does not run.
            t.call(
                "record_target",
                &[
                    ("s", t.s()),
                    ("target", Value::text("environment")),
                    ("device", Value::some(Value::text("another-device"))),
                ],
            );
            t.call("attach", &[("s", t.s())]);
            let window = t.window();
            let row = node_with(window.tree(), "ui.DisclosureRow", "title", "$ cargo test");
            assert_eq!(
                prop(row, "secondary").as_deref(),
                Some("Exit 101, in /home/ada/src/tilo"),
                "the row says how the command ended: {}",
                dump(window.tree())
            );
            let again = node_with(window.tree(), "ui.Button", "label", "Run this test again").key.clone();
            assert!(matches!(
                window.fire(&again, "press").unwrap(),
                InvokeOutcome::Done(_)
            ));
            // The device is asked how its tool runs that one test, and runs what it answers.
            let asked = t.engine.asked_of("rerun_test");
            assert_eq!(asked["command"], "cargo test", "{asked}");
            assert_eq!(asked["test"], "parse::reads_a_long_line", "{asked}");
            let run = t.engine.asked_of("run");
            assert_eq!(run["shell"], "shell-1", "{run}");
            assert_eq!(run["text"], "cargo test -- --exact parse::reads_a_long_line", "{run}");
        }
    }

    tilo_test_env::scenario! {
        fn an_agents_run_is_its_own_job_and_the_devices_own_shell_is_never_its;

        phase an_agents_run_is_its_own_job_and_is_recorded_when_it_ends;
        {
            let t = Term::opened();
            let one = agent("claude");
            t.call_as(
                &one,
                "run",
                &[("s", t.s()), ("command", Value::text("cargo build"))],
            );
            // The device runs it as the agent's, at the prompt of the session's shell.
            let asked = t.engine.asked_of("run");
            assert_eq!(asked["shell"], "shell-1", "{asked}");
            assert_eq!(asked["text"], "cargo build", "{asked}");
            assert_eq!(asked["agent"], true, "{asked}");
            // What its job says when it ends is kept as the agent's run.
            t.run_ended(
                &one,
                "run-1",
                &reported("cargo build", "/work", 0, 2000, "Compiling tilo", json!([])),
            );
            let commands = t.items(&t.session, "commands");
            assert_eq!(commands.len(), 1);
            assert_eq!(t.text(&commands[0], "command"), "cargo build");
            assert_eq!(t.output(&commands[0]), "Compiling tilo");
            assert_eq!(
                t.read(&commands[0], "actor"),
                Value::some(Value::Actor(one.clone())),
                "the run is the agent's"
            );
            let history = t.items(&t.terminal, "history");
            assert_eq!(history.len(), 1, "the workspace's history kept it once");
            assert_eq!(t.text(&history[0], "command"), "cargo build");
            assert_eq!(t.read(&history[0], "actor"), Value::some(Value::Actor(one.clone())));
        }

        phase a_command_line_the_device_cannot_take_is_refused_before_it_starts;
        {
            let t = Term::opened();
            let one = agent("claude");
            for refused in [
                String::new(),
                "   ".to_owned(),
                "echo one\necho two".to_owned(),
                "x".repeat(4097),
            ] {
                t.refused_as(&one, "run", &[("s", t.s()), ("command", Value::text(refused.as_str()))]);
                t.refused("run", &[("s", t.s()), ("command", Value::text(refused.as_str()))]);
            }
            assert_eq!(t.engine.count("run"), 0, "{:?}", t.engine.ops());
            // An agent runs where the workspace runs, never in the device's own shell; the person
            // at the window runs there.
            let host = Term::at("host");
            host.refused_as(&one, "run", &[("s", host.s()), ("command", Value::text("whoami"))]);
            assert_eq!(host.engine.count("run"), 0);
            host.call("run", &[("s", host.s()), ("command", Value::text("whoami"))]);
            assert_eq!(host.engine.asked_of("run")["agent"], false);
        }
    }

    tilo_test_env::scenario! {
        fn up_recalls_the_persons_own_command_lines_newest_first;

        phase the_shell_is_answered_with_the_persons_lines_that_hold_what_was_typed;
        {
            let t = Term::opened();
            for (command, ended) in [("cargo build", 100), ("ls", 200), ("cargo test", 300)] {
                t.call(
                    "finished",
                    &[
                        ("s", t.s()),
                        ("said", Value::text(reported(command, "/work", 0, ended, "", json!([])))),
                    ],
                );
            }
            // Another member's lines are their own.
            let bob = person("bob");
            t.call_as(
                &bob,
                "remember",
                &[
                    ("t", Value::resource(&t.terminal)),
                    ("command", Value::text("cargo build --release")),
                    ("cwd", Value::text("/work")),
                    ("exit", Value::Int(0)),
                    ("ended_ms", Value::Int(400)),
                    ("session", Value::text("their-session")),
                    ("run", Value::text("their-run")),
                ],
            );
            t.call(
                "recall",
                &[
                    ("s", t.s()),
                    (
                        "asked",
                        Value::text(r#"{"key":"k1","skip":0,"limit":10,"matching":"cargo"}"#),
                    ),
                ],
            );
            let answer = t.engine.asked_of("answer_history");
            assert_eq!(answer["shell"], "shell-1", "{answer}");
            assert_eq!(answer["key"], "k1", "{answer}");
            assert_eq!(answer["matching"], "cargo", "{answer}");
            assert_eq!(
                answer["lines"],
                json!(["cargo test", "cargo build"]),
                "the person's matches, newest first"
            );
            // Nothing typed recalls everything of theirs.
            t.call(
                "recall",
                &[
                    ("s", t.s()),
                    ("asked", Value::text(r#"{"key":"k2","skip":0,"limit":10,"matching":""}"#)),
                ],
            );
            assert_eq!(
                t.engine.asked_of("answer_history")["lines"],
                json!(["cargo test", "ls", "cargo build"])
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_held_command_offers_its_card_and_cancels_at_the_prompt;

        phase the_banner_shows_what_the_host_holds_and_cancel_lets_it_go;
        {
            let t = Term::opened();
            let mut window = t.window();
            let grid = t.grid(&window);
            window
                .fire_with(
                    &grid,
                    "held",
                    Some(&Value::text(
                        r#"{"title":"This needs a Linux machine.","message":"No VM runs yet.","url":"https://tilo.sh/billing"}"#,
                    )),
                )
                .expect("the host says what it holds");
            window.poll_patches().unwrap();
            let banner = of_kind(window.tree(), "ui.Banner")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the window shows the held command: {}", dump(window.tree())));
            assert_eq!(prop(banner, "title").as_deref(), Some("This needs a Linux machine."));
            assert_eq!(prop(banner, "message").as_deref(), Some("Add a card to continue."));
            assert_eq!(prop(banner, "action").as_deref(), Some("Add a card"));
            assert_eq!(prop(banner, "secondary_action").as_deref(), Some("Cancel"));
            // Cancel lets the command go: the shell is interrupted, and the banner is gone.
            assert!(matches!(
                window.fire(&banner.key, "secondary_act").unwrap(),
                InvokeOutcome::Done(_)
            ));
            assert_eq!(t.text(&t.session, "held"), "");
            assert_eq!(t.engine.asked_of("interrupt")["shell"], "shell-1");
            window.poll_patches().unwrap();
            assert!(of_kind(window.tree(), "ui.Banner").is_empty());
        }
    }

    tilo_test_env::scenario! {
        fn an_exit_ends_the_window_and_a_lost_connection_reopens_with_its_relay;

        phase an_exit_ends_the_session_and_closes_the_window;
        {
            let t = Term::opened();
            let mut window = t.window();
            let grid = t.grid(&window);
            window
                .fire_with(&grid, "exit", Some(&Value::text("exited")))
                .expect("the shell says it exited");
            assert_eq!(t.state(), "ended: Default environment");
            assert_eq!(t.text(&t.session, "shell"), "", "there is no shell to show");
            assert!(
                window.poll_events().contains(&ViewEvent::CloseWindow),
                "the window closes with the shell"
            );
            window.poll_patches().unwrap();
            assert!(
                shows(window.tree(), "The shell exited"),
                "a window still shown says so: {}",
                dump(window.tree())
            );
            assert!(shows(window.tree(), "Restart shell"));
        }

        phase a_relay_session_is_recorded_for_the_session_it_carries;
        {
            let t = Term::opened();
            t.call(
                "record_relay_session",
                &[("s", t.s()), ("session", Value::some(Value::text(RELAY)))],
            );
            assert_eq!(t.text(&t.session, "relay"), RELAY);
        }

        phase a_lost_connection_says_so_and_reopens_attached_to_its_relay;
        {
            // The device opened the session's shell on a relay, so the session re-attaches to it
            // when the connection is lost.
            let t = Term::new();
            *t.engine.carries.borrow_mut() = Some(RELAY.to_owned());
            t.call(
                "record_target",
                &[("s", t.s()), ("target", Value::text("environment"))],
            );
            t.call("attach", &[("s", t.s())]);
            assert_eq!(t.text(&t.session, "relay"), RELAY);
            let window = t.window();
            let grid = t.grid(&window);
            window
                .fire_with(&grid, "lost", Some(&Value::text("the connection dropped")))
                .expect("the shell says its connection was lost");
            // The device is asked for the shell again, re-attached to the session that still runs
            // on its relay.
            let opens = t.engine.asks("open");
            assert_eq!(opens.len(), 2, "{opens:?}");
            assert_eq!(opens[1]["attach"], RELAY, "{opens:?}");
            assert_eq!(t.state(), "open: Default environment");
            assert_eq!(t.text(&t.session, "shell"), "shell-2");
        }

        phase a_lost_connection_no_relay_carries_says_it_is_reconnecting;
        {
            let t = Term::opened();
            let mut window = t.window();
            let grid = t.grid(&window);
            window
                .fire_with(&grid, "lost", Some(&Value::text("the connection dropped")))
                .expect("the shell says its connection was lost");
            assert_eq!(
                t.state(),
                "lost: Default environment, the connection dropped"
            );
            window.poll_patches().unwrap();
            assert!(
                shows(window.tree(), "Reconnecting…"),
                "the window says it reconnects: {}",
                dump(window.tree())
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_run_is_recorded_under_its_id_once;

        phase a_command_the_host_records_takes_its_id_once;
        {
            let t = Term::opened();
            let given = [
                ("s", t.s()),
                ("command", Value::text("cargo test")),
                ("cwd", Value::text("/work")),
                ("started_ms", Value::Int(10)),
                ("ended_ms", Value::Int(20)),
                ("exit", Value::Int(101)),
                ("output", Value::text("error[E0425]: cannot find value `x`")),
                ("tool", Value::text("cargo")),
                ("id", Value::text("run-7")),
            ];
            t.call("record_command", &given);
            let commands = t.items(&t.session, "commands");
            assert_eq!(commands.len(), 1);
            assert_eq!(t.text(&commands[0], "command"), "cargo test");
            assert_eq!(t.text(&commands[0], "cwd"), "/work");
            assert_eq!(t.int(&commands[0], "exit"), 101);
            assert_eq!(
                t.output(&commands[0]),
                "error[E0425]: cannot find value `x`"
            );
            assert_eq!(t.items(&t.terminal, "history").len(), 1, "the workspace kept it");
            // The same command under the same id is not taken twice: neither as a second command
            // nor as a second line of the workspace's history.
            let second = t.tried(&me(), "record_command", &given);
            assert_eq!(
                t.items(&t.terminal, "history").len(),
                1,
                "a run the host records once is no second line of the history"
            );
            assert!(second.is_err(), "a command takes an id of its own: {second:?}");
            assert_eq!(t.items(&t.session, "commands").len(), 1);
            // An id of its own records another.
            let mut other = given.to_vec();
            other[8] = ("id", Value::text("run-8"));
            other[1] = ("command", Value::text("cargo build"));
            t.call("record_command", &other);
            assert_eq!(t.items(&t.session, "commands").len(), 2);
        }

        phase an_imported_run_is_kept_once;
        {
            let t = Term::new();
            let given = [
                ("t", Value::resource(&t.terminal)),
                ("command", Value::text("cargo test")),
                ("cwd", Value::text("/work")),
                ("exit", Value::Int(0)),
                ("ended_ms", Value::Int(500)),
                ("session", Value::text("their-session")),
                ("run", Value::text("their-run")),
                ("environment", Value::some(Value::text("environment"))),
            ];
            t.call("import_run", &given);
            t.call("import_run", &given);
            let history = t.items(&t.terminal, "history");
            assert_eq!(history.len(), 1, "a run another device recorded is kept once");
            assert_eq!(t.text(&history[0], "command"), "cargo test");
            assert_eq!(t.text(&history[0], "run"), "their-run");
            assert_eq!(t.text(&history[0], "session"), "their-session");
            assert_eq!(t.text(&history[0], "environment"), "environment");
            // A run of the same session under another id is another run.
            let mut other = given.to_vec();
            other[6] = ("run", Value::text("their-other-run"));
            t.call("import_run", &other);
            assert_eq!(t.items(&t.terminal, "history").len(), 2);
        }
    }

    tilo_test_env::scenario! {
        fn the_machines_secrets_are_added_by_variable_and_removed;

        phase a_variable_takes_one_vault_item_and_naming_it_again_replaces_it;
        {
            let t = Term::new();
            let m = Value::resource(&t.machine);
            t.call(
                "add_secret",
                &[
                    ("m", m.clone()),
                    ("variable", Value::text("GITHUB_TOKEN")),
                    ("item", Value::text("vault.item:one")),
                ],
            );
            t.call(
                "add_secret",
                &[
                    ("m", m.clone()),
                    ("variable", Value::text("NPM_TOKEN")),
                    ("item", Value::text("vault.item:two")),
                ],
            );
            t.call(
                "add_secret",
                &[
                    ("m", m.clone()),
                    ("variable", Value::text("GITHUB_TOKEN")),
                    ("item", Value::text("vault.item:three")),
                ],
            );
            // The machine hands each variable its one item; which was named first says nothing.
            let mut held: Vec<(String, String)> = t
                .items(&t.machine, "secrets")
                .iter()
                .map(|x| (t.text(x, "variable"), t.text(x, "item")))
                .collect();
            held.sort();
            assert_eq!(
                held,
                [
                    ("GITHUB_TOKEN".to_owned(), "vault.item:three".to_owned()),
                    ("NPM_TOKEN".to_owned(), "vault.item:two".to_owned())
                ]
            );
        }

        phase the_machines_own_variables_and_what_a_secret_is_not_are_refused;
        {
            let t = Term::new();
            let m = Value::resource(&t.machine);
            for (variable, item) in [
                ("PATH", "vault.item:x"),
                ("HOME", "vault.item:x"),
                ("LD_PRELOAD", "vault.item:x"),
                ("XDG_RUNTIME_DIR", "vault.item:x"),
                ("1TOKEN", "vault.item:x"),
                ("MY TOKEN", "vault.item:x"),
                ("TOKEN", "hunter2"),
                ("TOKEN", "vault.item:"),
            ] {
                t.refused(
                    "add_secret",
                    &[
                        ("m", m.clone()),
                        ("variable", Value::text(variable)),
                        ("item", Value::text(item)),
                    ],
                );
            }
            assert!(t.items(&t.machine, "secrets").is_empty());
        }

        phase a_variable_removed_is_no_longer_handed_out;
        {
            let t = Term::new();
            let m = Value::resource(&t.machine);
            for variable in ["GITHUB_TOKEN", "NPM_TOKEN"] {
                t.call(
                    "add_secret",
                    &[
                        ("m", m.clone()),
                        ("variable", Value::text(variable)),
                        ("item", Value::text("vault.item:one")),
                    ],
                );
            }
            t.call(
                "remove_secret",
                &[("m", m.clone()), ("variable", Value::text("GITHUB_TOKEN"))],
            );
            let left: Vec<String> = t
                .items(&t.machine, "secrets")
                .iter()
                .map(|x| t.text(x, "variable"))
                .collect();
            assert_eq!(left, ["NPM_TOKEN"]);
            t.refused(
                "remove_secret",
                &[("m", m), ("variable", Value::text("GITHUB_TOKEN"))],
            );
        }
    }

    tilo_test_env::scenario! {
        fn only_a_plain_public_web_address_opens_from_the_output;

        phase a_printed_address_opens_only_when_it_can_carry_nothing_else;
        {
            let t = Term::opened();
            for opened in ["https://example.com", "http://example.com/a/b"] {
                t.call("open_link", &[("s", t.s()), ("url", Value::text(opened))]);
            }
            assert_eq!(
                *t.disk.opened.borrow(),
                ["https://example.com".to_owned(), "http://example.com/a/b".to_owned()]
            );
            for refused in [
                "file:///etc/passwd",
                "javascript:alert(1)",
                "https://user@evil.example",
                "https://a.example/?q=1",
                "https://a.example/#x",
                "mailto:someone@example.com",
            ] {
                t.refused("open_link", &[("s", t.s()), ("url", Value::text(refused))]);
            }
            assert_eq!(t.disk.opened.borrow().len(), 2, "nothing else opened");
        }
    }

    #[test]
    fn every_scenario_of_the_terminal_opens_its_default_view() {
        // What each scenario stages, as the window of a session shows it.
        let expected = [
            (
                "a build that failed",
                ["$ cargo test", "Run this test again"],
            ),
            (
                "a shell that could not start",
                ["The shell couldn't start", "Try again"],
            ),
            ("a command waits for a card", ["Add a card", "Cancel"]),
        ];
        let compiled = App::compile(SOURCE, &[]);
        let staged: Vec<String> = compiled
            .meta()
            .scenarios
            .iter()
            .map(|scenario| scenario.name.clone())
            .collect();
        assert_eq!(
            staged,
            expected
                .iter()
                .map(|(name, _)| (*name).to_owned())
                .collect::<Vec<_>>(),
            "every scenario this app stages has a window this test opens"
        );
        for (name, shown) in expected {
            let t = Term::new();
            let action = t
                .app
                .meta()
                .scenarios
                .iter()
                .find(|scenario| scenario.name == name)
                .unwrap_or_else(|| panic!("no scenario {name}"))
                .action
                .clone();
            let ran = t.rt.invoke_action(&me(), (action.as_str(), Vec::new()));
            assert!(
                matches!(ran, Ok(InvokeOutcome::Done(_))),
                "the scenario {name} stages: {ran:?}"
            );
            let window = t.window();
            for word in shown {
                assert!(
                    shows(window.tree(), word),
                    "{name} shows {word}: {}",
                    dump(window.tree())
                );
            }
        }
    }
}
