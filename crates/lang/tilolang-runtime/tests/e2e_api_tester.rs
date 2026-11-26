//! The API Tester (`apps/tl/api-tester/app.tl`) end to end: a request built, saved, opened and
//! sent; its variables handed to the device; its sign-in kept on the device and never in the
//! workspace; the answer the device's engine gives, the answer a send returns to its caller, and
//! the history entry it makes; a send stopped before it answers; and the window, with its request
//! tabs, its rows, its sheets and its response pane.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::host_work::{EngineHost, JobHandlers};
    use tilolang_runtime::{
        AmStore, EditOutcome, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/api-tester/app.tl");

    /// A request as the app asked the device to send it, as the engine read it.
    #[derive(Clone, Debug, Default)]
    struct Asked {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        auth_scheme: String,
        vars: Vec<(String, String, String)>,
    }

    /// The device's engine: what it was asked, the credentials it keeps, and the answers it
    /// gives.
    #[derive(Clone, Default)]
    struct Engine {
        calls: Rc<RefCell<Vec<(String, String)>>>,
        kept: Rc<RefCell<HashMap<String, String>>>,
        asked: Rc<RefCell<Vec<Asked>>>,
        /// How the next status answers: `running` until it is `done`, `failed` or `cancelled`.
        state: Rc<RefCell<String>>,
        /// The answer the done status carries.
        answer: Rc<RefCell<String>>,
        cancelled: Rc<RefCell<bool>>,
        /// The actions each started request named as it ends.
        handlers: Rc<RefCell<Vec<JobHandlers>>>,
    }

    impl Engine {
        fn new() -> Self {
            Self {
                calls: Rc::default(),
                kept: Rc::default(),
                asked: Rc::default(),
                state: Rc::new(RefCell::new("running".to_owned())),
                answer: Rc::new(RefCell::new(String::new())),
                cancelled: Rc::default(),
                handlers: Rc::default(),
            }
        }

        fn last_ask(&self) -> Asked {
            self.asked.borrow().last().cloned().unwrap_or_default()
        }

        /// Keeps what a send asked for, as the device read it.
        fn record_ask(&self, input: &str) -> Result<(), String> {
            let ask: serde_json::Value =
                serde_json::from_str(input).map_err(|error| error.to_string())?;
            let text = |key: &str| {
                ask.get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            let pairs = |key: &str| -> Vec<(String, String)> {
                rows_of(&ask, key)
                    .into_iter()
                    .filter_map(|row| Some((row.first()?.clone(), row.get(1)?.clone())))
                    .collect()
            };
            let vars = rows_of(&ask, "vars")
                .into_iter()
                .map(|row| {
                    (
                        row.first().cloned().unwrap_or_default(),
                        row.get(1).cloned().unwrap_or_default(),
                        row.get(2).cloned().unwrap_or_default(),
                    )
                })
                .collect();
            self.asked.borrow_mut().push(Asked {
                method: text("method"),
                url: text("url"),
                headers: pairs("headers"),
                auth_scheme: text("auth_scheme"),
                vars,
            });
            Ok(())
        }

        /// The next send answers `answer` and is done.
        fn answer_with(&self, answer: &str) {
            *self.answer.borrow_mut() = answer.to_owned();
            *self.state.borrow_mut() = "done".to_owned();
        }
    }

    fn rows_of(ask: &serde_json::Value, key: &str) -> Vec<Vec<String>> {
        ask.get(key)
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        row.as_array()
                            .map(|row| {
                                row.iter()
                                    .map(|cell| cell.as_str().unwrap_or_default().to_owned())
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            match op {
                // A send waited for in the call answers its response, or why it got none.
                "send" => {
                    self.record_ask(input)?;
                    match self.state.borrow().as_str() {
                        "done" => Ok(self.answer.borrow().clone()),
                        _ => Err("the server refused the connection".to_owned()),
                    }
                }
                "keep_credential" => {
                    let value: serde_json::Value =
                        serde_json::from_str(input).map_err(|error| error.to_string())?;
                    let value = value
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned();
                    let reference =
                        format!("api-ref-{}", self.kept.borrow().len().saturating_add(1));
                    self.kept.borrow_mut().insert(reference.clone(), value);
                    Ok(format!(r#"{{"reference":"{reference}"}}"#))
                }
                "forget_credential" => {
                    let ask: serde_json::Value =
                        serde_json::from_str(input).map_err(|error| error.to_string())?;
                    let reference = ask
                        .get("reference")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned();
                    self.kept.borrow_mut().remove(&reference);
                    Ok("{}".to_owned())
                }
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn start(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            handlers: &JobHandlers,
        ) -> Result<String, String> {
            if op != "send" {
                return Err(format!("`{op}` is no op this device runs"));
            }
            self.record_ask(input)?;
            self.handlers.borrow_mut().push(handlers.clone());
            Ok("job-1".to_owned())
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            let said = || -> Result<String, String> {
                if job != "job-1" {
                    return Err(format!("no job `{job}`"));
                }
                let state = self.state.borrow().clone();
                let (answer, error) = match state.as_str() {
                    "done" => (self.answer.borrow().clone(), String::new()),
                    "failed" => (
                        String::new(),
                        "the server refused the connection".to_owned(),
                    ),
                    "cancelled" => (String::new(), "The request was cancelled.".to_owned()),
                    _ => (String::new(), String::new()),
                };
                Ok(serde_json::json!({
                    "state": state,
                    "answer": answer,
                    "error": error,
                })
                .to_string())
            };
            Ok(said()
                .ok()
                .and_then(|said| tilolang_tilo::engine::record_of(job, &said)))
        }

        fn cancel(&self, job: &str) -> bool {
            if job != "job-1" {
                return false;
            }
            *self.cancelled.borrow_mut() = true;
            *self.state.borrow_mut() = "cancelled".to_owned();
            true
        }
    }

    /// One answer as the engine writes it.
    fn answer_json(status: i32, body: &str) -> String {
        serde_json::json!({
            "status": status,
            "status_text": match status {
                200 => "OK",
                201 => "Created",
                _ => "Not Found",
            },
            "duration_ms": 84,
            "size_bytes": body.len(),
            "size_exact": true,
            "shown_bytes": body.len(),
            "binary": false,
            "content_type": "application/json; charset=utf-8",
            "body": body,
            "headers": [["content-type", "application/json; charset=utf-8"], ["server", "tilo-edge/2.4"]],
        })
        .to_string()
    }

    struct Tester {
        rt: Runtime,
        app: App,
        engine: Engine,
        selection: Clipboard,
        root: ResourceRef,
    }

    impl Tester {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let engine = Engine::new();
            rt.set_engine(engine.clone());
            let selection = Clipboard::default();
            rt.set_clipboard(selection.clone());
            authority.trust(&app.load(&rt));
            // A copy reaches outside Tilo, which the policy allows for the app it is written
            // in: the person's press of its copy button.
            for action in ["copy_body", "copy_headers", "copy_request"] {
                authority.grant_outside(&app.action(action));
            }
            let root = rt.create_root("Workspace").unwrap();
            Self {
                rt,
                app,
                engine,
                selection,
                root,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn done(&self, action: &str, args: Vec<Value>) -> Value {
            match self.call(action, args) {
                InvokeOutcome::Done(value) => value,
                other => panic!("{action}: {other:?}"),
            }
        }

        /// The one resource a field holds, or none when it holds none.
        fn of(&self, r: &ResourceRef, field: &str) -> Option<ResourceRef> {
            match self.rt.read(r, field).unwrap() {
                Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                Value::Opt(Some(tag)) => match *tag {
                    Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                    _ => None,
                },
                Value::Opt(None) => None,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// One field of a record a field of `r` holds, as it reads.
        fn nested(&self, r: &ResourceRef, field: &str, name: &str) -> serde_json::Value {
            let held = self.rt.read(r, field).unwrap();
            let whole = self.rt.project(&held);
            whole
                .get("fields")
                .and_then(|fields| fields.get(name))
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        }

        /// The request the builder holds.
        fn draft(&self) -> ResourceRef {
            self.of(&self.root, "builder").expect("a builder")
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("tester"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap_or_else(|e| panic!("tester: {e}"));
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// Types `url` into the window's address field, as a person does.
        fn type_url(window: &ViewInstance, url: &str) {
            let input = of_kind(window.tree(), "ui.TextInput")
                .into_iter()
                .find(|node| prop(node, "placeholder") == "Enter a URL")
                .expect("the address field");
            assert_eq!(
                window.edit(&input.key, &Value::text(url)).unwrap(),
                EditOutcome::Applied
            );
        }

        fn collection(&self, name: &str) -> ResourceRef {
            returned_ref(self.call(
                "new_collection",
                vec![Value::resource(&self.root), Value::text(name)],
            ))
        }

        /// Drives the request on its way the way the device does: runs the `done:` action its
        /// start named, handed the record the device keeps for the job.
        fn settle(&self) {
            let record = self
                .engine
                .job("job-1")
                .unwrap()
                .expect("the device keeps the job it runs");
            let kept = self
                .engine
                .handlers
                .borrow()
                .last()
                .cloned()
                .expect("a request was sent");
            let handler = kept.done.expect("the send named its end");
            let call = self.rt.job_call(&handler, &record).unwrap();
            self.rt
                .invoke_action(&me(), call)
                .unwrap_or_else(|error| panic!("the send's handler: {error:?}"));
        }
    }

    impl Reads for Tester {
        fn rt(&self) -> &Runtime {
            &self.rt
        }
    }

    /// Every node of a component, by its kind.
    fn of_kind<'a>(tree: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
        fn walk<'a>(node: &'a UiNode, kind: &str, found: &mut Vec<&'a UiNode>) {
            if node.kind == kind {
                found.push(node);
            }
            for child in &node.children {
                walk(child, kind, found);
            }
        }
        let mut found = Vec::new();
        walk(tree, kind, &mut found);
        found
    }

    fn prop(node: &UiNode, name: &str) -> String {
        node.props
            .get(name)
            .map(|value| match value {
                Value::Text(text) => text.clone(),
                other => format!("{other:?}"),
            })
            .unwrap_or_default()
    }

    fn buttons(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Button")
            .iter()
            .map(|b| prop(b, "label"))
            .collect()
    }

    tilo_test_env::scenario! {
        fn e2e_api_tester_1;

        phase a_request_is_built_saved_opened_and_deleted;
        {
            let s = Tester::new();
            let draft = s.draft();
            assert_eq!(s.text(&draft, "name"), "", "a new request starts unnamed");

            s.done("add_param", vec![Value::resource(&draft)]);
            let params = s.records(&draft, "params");
            assert_eq!(params.len(), 1, "the parameter stands");
            assert_eq!(s.text(&params[0], "key"), "");

            // A collection holds the request, and the builder's copy opens it again.
            let users = s.collection("Users");
            assert_eq!(s.text(&users, "name"), "Users");
            let added = returned_ref(s.call(
                "add_request",
                vec![Value::resource(&users), Value::text("List users")],
            ));
            assert_eq!(s.text(&added, "name"), "List users");
            s.done(
                "open_request",
                vec![Value::resource(&s.root), Value::resource(&added)],
            );
            assert_eq!(
                s.text(&s.draft(), "name"),
                "List users",
                "the builder holds what was opened"
            );

            // Saving the builder into another collection keeps the request there.
            let archive = s.collection("Archive");
            let saved = returned_ref(s.call(
                "save_request",
                vec![Value::resource(&s.root), Value::resource(&archive)],
            ));
            assert_eq!(s.text(&saved, "name"), "List users");
            assert_eq!(s.records(&archive, "requests").len(), 1);

            // Deleting the saved request takes it out of its collection.
            s.done(
                "delete_request",
                vec![Value::resource(&s.root), Value::resource(&saved)],
            );
            assert!(s.records(&archive, "requests").is_empty());
            assert!(s.of(&s.root, "opened").is_none());

            // A collection goes with the requests saved in it.
            s.done(
                "delete_collection",
                vec![Value::resource(&s.root), Value::resource(&users)],
            );
            let left = s.records(&s.root, "collections");
            assert_eq!(left.len(), 1, "only the other collection is left");
            assert_eq!(s.text(&left[0], "name"), "Archive");
        }

        phase environments_fill_variables_and_keep_their_secrets_on_the_device;
        {
            let s = Tester::new();
            let env = returned_ref(s.call(
                "new_environment",
                vec![Value::resource(&s.root), Value::text("Staging")],
            ));
            assert_eq!(
                s.text(&env, "name"),
                "Staging",
                "a new environment is in use"
            );
            let base = returned_ref(s.call(
                "add_variable",
                vec![
                    Value::resource(&env),
                    Value::text("base"),
                    Value::Bool(false),
                ],
            ));
            assert_eq!(s.text(&base, "key"), "base");
            assert!(!s.flag(&base, "secret"));

            // A secret's value is typed once and kept by the device; the workspace holds a
            // reference and no part of the value.
            let token = returned_ref(s.call(
                "add_variable",
                vec![
                    Value::resource(&env),
                    Value::text("token"),
                    Value::Bool(true),
                ],
            ));
            assert!(s.flag(&token, "secret"));
            s.done(
                "keep_variable",
                vec![Value::resource(&token), Value::text("s3cr3t-token")],
            );
            let reference = s.text(&token, "reference");
            assert!(reference.starts_with("api-ref-"), "reference: {reference}");
            assert_eq!(
                s.text(&token, "value"),
                "",
                "the value stays off the workspace"
            );
            assert_eq!(
                s.engine.kept.borrow().get(&reference).map(String::as_str),
                Some("s3cr3t-token")
            );

            // Deleting the variable takes its value off the device too.
            s.done(
                "delete_variable",
                vec![Value::resource(&env), Value::resource(&token)],
            );
            assert!(
                s.engine.kept.borrow().get(&reference).is_none(),
                "a deleted variable takes its value off the device"
            );

            // With no environment, nothing fills a variable.
            s.done("no_environment", vec![Value::resource(&s.root)]);
            assert!(s.of(&s.root, "active_environment").is_none());
            s.done(
                "use_environment",
                vec![Value::resource(&s.root), Value::resource(&env)],
            );
            assert_eq!(
                s.of(&s.root, "active_environment")
                    .map(|e| s.text(&e, "name")),
                Some("Staging".to_owned())
            );
        }

        phase a_send_hands_the_device_its_variables_and_its_sign_in;
        {
            let s = Tester::new();
            let env = returned_ref(s.call(
                "new_environment",
                vec![Value::resource(&s.root), Value::text("Staging")],
            ));
            s.done(
                "add_variable",
                vec![
                    Value::resource(&env),
                    Value::text("base"),
                    Value::Bool(false),
                ],
            );
            // The address is typed into the window.
            let window = s.window();
            Tester::type_url(&window, "{{base}}/users");
            assert_eq!(s.text(&s.draft(), "url"), "{{base}}/users");

            // The sign-in is kept on the device, and only its reference stands in the workspace.
            s.done(
                "keep_credential",
                vec![Value::resource(&s.root), Value::text("a-private-token")],
            );
            let reference = s
                .nested(&s.draft(), "auth", "reference")
                .as_str()
                .unwrap_or_default()
                .to_owned();
            assert!(reference.starts_with("api-ref-"), "reference: {reference}");
            assert_eq!(
                s.engine.kept.borrow().get(&reference).map(String::as_str),
                Some("a-private-token")
            );

            // Sending hands the device the request as typed, with its variables.
            s.engine.answer_with(&answer_json(200, "{}"));
            s.done("start_send", vec![Value::resource(&s.root)]);
            let ask = s.engine.last_ask();
            assert_eq!(ask.method, "GET");
            assert_eq!(
                ask.url, "{{base}}/users",
                "the device resolves the variables"
            );
            assert_eq!(
                ask.vars,
                vec![("base".to_owned(), String::new(), String::new())],
                "a shared variable's value is the workspace's"
            );
            assert_eq!(ask.auth_scheme, "none", "no scheme was chosen yet");
            assert!(ask.headers.is_empty(), "a row with no name is not sent");
        }

        phase the_answer_reaches_the_window_and_the_history;
        {
            let s = Tester::new();
            let window = s.window();
            Tester::type_url(&window, "https://api.example.com/users");
            let body = "{\"id\":1}";
            s.engine.answer_with(&answer_json(200, body));

            s.done("start_send", vec![Value::resource(&s.root)]);
            assert!(s.flag(&s.root, "sending"), "the request is on its way");
            s.settle();

            assert!(!s.flag(&s.root, "sending"));
            assert!(s.flag(&s.root, "answered"));
            assert_eq!(s.text(&s.root, "failure"), "");
            assert_eq!(
                s.nested(&s.root, "answer", "status"),
                serde_json::json!(200)
            );
            assert_eq!(
                s.nested(&s.root, "answer", "size_bytes"),
                serde_json::json!(body.len())
            );
            assert_eq!(s.nested(&s.root, "answer", "body"), serde_json::json!(body));
            assert_eq!(
                s.nested(&s.root, "answer", "pretty"),
                serde_json::json!("{\n  \"id\": 1\n}"),
                "the pretty view reads JSON as it is laid out"
            );
            assert_eq!(
                s.nested(&s.root, "answer", "headers"),
                serde_json::json!({"count": 2}),
                "both headers stand"
            );

            // The history keeps what was sent, named by its route.
            let history = s.records(&s.root, "history");
            assert_eq!(history.len(), 1);
            assert_eq!(
                s.text(&history[0], "route"),
                "GET https://api.example.com/users returned status 200"
            );
            assert_eq!(s.number(&history[0], "status"), 200);
            assert!(
                s.text(&history[0], "replay")
                    .contains("https://api.example.com/users"),
                "the entry keeps the request it sent"
            );
        }

        phase a_send_can_be_stopped_and_a_failed_one_says_why;
        {
            let s = Tester::new();
            let window = s.window();
            Tester::type_url(&window, "https://api.example.com/slow");
            s.done("start_send", vec![Value::resource(&s.root)]);
            s.done("cancel_send", vec![Value::resource(&s.root)]);
            assert!(*s.engine.cancelled.borrow());
            assert!(!s.flag(&s.root, "sending"));
            assert_eq!(s.text(&s.root, "failure"), "The request was cancelled.");

            *s.engine.state.borrow_mut() = "failed".to_owned();
            s.done("start_send", vec![Value::resource(&s.root)]);
            s.settle();
            assert_eq!(
                s.text(&s.root, "failure"),
                "the server refused the connection"
            );
            assert!(
                s.records(&s.root, "history").is_empty(),
                "nothing came back to keep"
            );
        }

        phase a_send_answers_its_caller_with_the_response;
        {
            let s = Tester::new();
            let window = s.window();
            Tester::type_url(&window, "https://api.example.com/users");
            let body = "{\"id\":1}";
            s.engine.answer_with(
                &serde_json::json!({
                    "status": 201,
                    "status_text": "Created",
                    "duration_ms": 84,
                    "size_bytes": body.len(),
                    "size_exact": true,
                    "shown_bytes": body.len(),
                    "binary": false,
                    "content_type": "application/json; charset=utf-8",
                    "body": body,
                    "headers": [
                        ["content-type", "application/json; charset=utf-8"],
                        ["x-request-id", "req-42"],
                    ],
                })
                .to_string(),
            );

            // The invocation answers the response itself, so an agent or a script reads the status,
            // the text, the body, the size and the headers in the one call.
            let answered = s.done("send", vec![Value::resource(&s.root)]);
            let answer = match answered {
                Value::Text(text) => text,
                other => panic!("send answers its caller the response: {other:?}"),
            };
            let read: serde_json::Value = serde_json::from_str(&answer).expect("the answer is JSON");
            assert_eq!(read["status"], serde_json::json!(201));
            assert_eq!(read["status_text"], serde_json::json!("Created"));
            assert_eq!(read["body"], serde_json::json!(body));
            assert_eq!(read["size_bytes"], serde_json::json!(body.len()));
            assert!(
                read["headers"]
                    .as_array()
                    .is_some_and(|headers| headers.iter().any(|pair| pair
                        .get(0)
                        .and_then(|v| v.as_str())
                        == Some("x-request-id")
                        && pair.get(1).and_then(|v| v.as_str()) == Some("req-42"))),
                "the headers come back with it: {}",
                read["headers"]
            );

            // The same answer reaches the window: the pane, the flag and the history entry.
            assert!(!s.flag(&s.root, "sending"));
            assert!(s.flag(&s.root, "answered"));
            assert_eq!(s.text(&s.root, "failure"), "");
            assert_eq!(
                s.nested(&s.root, "answer", "status"),
                serde_json::json!(201)
            );
            assert_eq!(s.nested(&s.root, "answer", "body"), serde_json::json!(body));
            let history = s.records(&s.root, "history");
            assert_eq!(history.len(), 1, "the send is kept as one entry");
            assert_eq!(s.number(&history[0], "status"), 201);
            assert!(
                s.text(&history[0], "replay")
                    .contains("https://api.example.com/users"),
                "the entry keeps the request it sent"
            );

            // A send the engine fails answers nothing and leaves why it failed in the window.
            *s.engine.state.borrow_mut() = "failed".to_owned();
            let answered = s.done("send", vec![Value::resource(&s.root)]);
            assert_eq!(answered, Value::text(""), "a failed send answers nothing");
            assert_eq!(
                s.text(&s.root, "failure"),
                "the server refused the connection"
            );
            assert!(s.flag(&s.root, "answered"));
            assert!(!s.flag(&s.root, "sending"));
            assert_eq!(
                s.records(&s.root, "history").len(),
                1,
                "nothing came back to keep"
            );
        }

        phase a_sent_request_opens_again_and_can_be_saved;
        {
            let s = Tester::new();
            let window = s.window();
            Tester::type_url(&window, "https://api.example.com/users");
            s.done("add_param", vec![Value::resource(&s.draft())]);
            s.engine.answer_with(&answer_json(201, "{}"));
            s.done("start_send", vec![Value::resource(&s.root)]);
            s.settle();
            assert_eq!(s.text(&s.root, "failure"), "", "the send went through");
            assert!(s.flag(&s.root, "answered"));
            let sent = s
                .records(&s.root, "history")
                .into_iter()
                .next()
                .unwrap_or_else(|| {
                    panic!(
                        "history is empty; answer reads {:?}",
                        s.nested(&s.root, "answer", "status")
                    )
                });

            // A fresh request replaces the builder, and the sent one brings it all back.
            s.done("new_request", vec![Value::resource(&s.root)]);
            assert_eq!(s.text(&s.draft(), "url"), "");
            s.done(
                "open_sent",
                vec![Value::resource(&s.root), Value::resource(&sent)],
            );
            let opened = s.draft();
            assert_eq!(s.text(&opened, "url"), "https://api.example.com/users");
            assert_eq!(
                s.records(&opened, "params").len(),
                1,
                "the parameters come back with it"
            );

            // It can stand in a collection as a request of its own.
            let users = s.collection("Users");
            let saved = returned_ref(s.call(
                "save_sent",
                vec![
                    Value::resource(&s.root),
                    Value::resource(&sent),
                    Value::resource(&users),
                ],
            ));
            assert_eq!(
                s.text(&saved, "url"),
                "https://api.example.com/users",
                "a sent request saved is a request"
            );
            s.done(
                "delete_sent",
                vec![Value::resource(&s.root), Value::resource(&sent)],
            );
            assert!(s.records(&s.root, "history").is_empty());
        }

        phase the_answer_and_the_request_leave_the_app_as_a_copy;
        {
            let s = Tester::new();
            let window = s.window();
            Tester::type_url(&window, "https://api.example.com/users");
            let body = "{\"id\":1}";
            s.engine.answer_with(&answer_json(200, body));
            s.done("start_send", vec![Value::resource(&s.root)]);
            s.settle();

            // The body, the headers and the request each leave as the person's copy.
            s.done("copy_body", vec![Value::resource(&s.root)]);
            assert_eq!(s.selection.text(), body);
            assert_eq!(s.text(&s.root, "notice"), "The body is copied");

            s.done("copy_headers", vec![Value::resource(&s.root)]);
            assert_eq!(
                s.selection.text(),
                "content-type: application/json; charset=utf-8\nserver: tilo-edge/2.4"
            );

            s.done("copy_request", vec![Value::resource(&s.root)]);
            assert_eq!(
                s.selection.text(),
                "GET https://api.example.com/users\n",
                "the request goes as authored, without a credential"
            );
        }

        phase the_window_shows_the_request_tabs_the_answer_and_the_sheets;
        {
            let s = Tester::new();
            let tree = s.window().tree().clone();
            let labels = buttons(&tree);
            for wanted in ["Send", "Add parameter", "Copy body", "Copy headers"] {
                assert!(
                    labels.iter().any(|label| label == wanted),
                    "{wanted} stands in the window: {labels:?}"
                );
            }
            let empty = of_kind(&tree, "ui.EmptyState");
            assert!(
                empty.iter().any(|e| prop(e, "title") == "No response yet"),
                "an empty answer says so: {empty:?}"
            );
            // Every sheet is drawn, and closed until a command opens one.
            let sheets = of_kind(&tree, "ui.ModalDialog");
            assert!(sheets.len() >= 8, "the window's sheets: {}", sheets.len());
            for sheet in &sheets {
                let open = sheet.props.get("open");
                assert!(
                    !matches!(open, Some(Value::Bool(true))),
                    "a sheet opens only when a command asks: {} reads {open:?}",
                    prop(sheet, "title")
                );
            }

            // With an answer, the pane shows the status and the body.
            let window = s.window();
            Tester::type_url(&window, "https://api.example.com/users");
            s.engine.answer_with(&answer_json(200, "{\"id\":1}"));
            s.done("start_send", vec![Value::resource(&s.root)]);
            s.settle();
            let tree = s.window().tree().clone();
            let badges = of_kind(&tree, "ui.Badge");
            assert!(
                badges.iter().any(|b| prop(b, "text") == "200 OK"),
                "the answer's status stands in the window"
            );
            let editors = of_kind(&tree, "ui.CodeEditor");
            assert!(!editors.is_empty(), "the response has its editor");
        }
    }
}
