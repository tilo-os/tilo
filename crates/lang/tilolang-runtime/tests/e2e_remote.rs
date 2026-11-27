//! Remote Desktop (`apps/tl/remote/app.tl`) end to end: the saved connections a workspace shares,
//! the form's refusals, the passwords kept in the workspace's vault, and the sessions it opens. An SSH
//! session's terminal, and a VNC desktop's or a Tilo device's screen, as each stream says it moves,
//! from connecting to closed. A machine's key is trusted only as it was shown.
//!
//! The device's engine is the test's. It answers the ops the Remote engine answers
//! (`crates/engines/loadable/remote` over `crates/engines/tilo-remote`, whose own tests reach real
//! SSH and VNC servers on loopback) and says what each stream says of itself, so the test drives
//! every turn a session can take. Its vault is a map, and a stream's ask is read into the session it
//! opens the way the engine reads it (`tilo_remote::asked`), so the test sees the password a session
//! signs in with. `e2e_remote_servers` drives the same app against a real `sshd` and `Xvnc`.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::sync::Arc;

    use crate::common::*;
    use serde_json::{Value as Json, json};
    use tilo_test_env::SpawnUnderEnvLock as _;
    use tilolang_engine::Push;
    use tilolang_runtime::host_work::{EngineHost, JobHandlers};
    use tilolang_runtime::{
        AmStore, EnumValue, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/remote/app.tl");

    /// A Tilo device's endpoint id, as the person's devices name it.
    const DEVICE: &str = "6f1c2d3e4a5b69788796a5b4c3d2e1f00112233445566778899aabbccddeeff0";

    /// What the device's engine was asked, and the streams it runs.
    #[derive(Clone, Default)]
    struct Device {
        /// Every call, `op` and its input.
        calls: Rc<RefCell<Vec<(String, Json)>>>,
        /// Every stream started, `op` and its input, by its job.
        streams: Rc<RefCell<Vec<(String, String, Json)>>>,
        cancelled: Rc<RefCell<Vec<String>>>,
        push: Rc<RefCell<Option<Arc<dyn Push>>>>,
        /// The workspace's vault: what `keep-secret` kept, by its key.
        vault: Rc<RefCell<BTreeMap<String, String>>>,
    }

    impl Device {
        /// The password the newest stream's session signs in with, read from the vault as the
        /// engine reads it.
        fn signs_in_with(&self) -> Option<String> {
            let (_, op, input) = self.stream();
            let vault = self.vault.borrow();
            let kept = |key: &str| Ok(vault.get(key).cloned());
            if op == "vnc" {
                let ask = serde_json::from_value(input).expect("a VNC ask");
                tilo_remote::asked::vnc_config(ask, &kept)
                    .expect("the ask reads")
                    .password
            } else {
                let ask: tilo_remote::asked::SshAsk =
                    serde_json::from_value(input).expect("an SSH ask");
                ask.secret_key
                    .as_deref()
                    .and_then(|key| vault.get(key).cloned())
            }
        }

        fn calls(&self, op: &str) -> Vec<Json> {
            self.calls
                .borrow()
                .iter()
                .filter(|(asked, _)| asked == op)
                .map(|(_, input)| input.clone())
                .collect()
        }

        /// The newest stream's job, op and input.
        fn stream(&self) -> (String, String, Json) {
            self.streams
                .borrow()
                .last()
                .cloned()
                .expect("a stream was started")
        }

        /// The stream `job` says `state` of itself.
        fn say(&self, job: &str, state: &Json) {
            self.push
                .borrow()
                .clone()
                .expect("a stream was started")
                .state(job, &state.to_string());
        }
    }

    impl EngineHost for Device {
        fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            if app != "so.tilo.remote" {
                return Err(format!("`{app}` is not the Remote app"));
            }
            let input: Json = serde_json::from_str(input).expect("the app asks in JSON");
            self.calls.borrow_mut().push((op.to_owned(), input.clone()));
            match op {
                "ssh-send" => Ok("{}".to_owned()),
                "keep-secret" => {
                    let ask: tilo_remote::asked::KeepAsk =
                        serde_json::from_value(input).expect("a password to keep");
                    self.vault.borrow_mut().insert(ask.key, ask.secret);
                    Ok("{}".to_owned())
                }
                other => Err(format!("`{other}` is no op of the Remote engine")),
            }
        }

        fn start(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            _handlers: &JobHandlers,
        ) -> Result<String, String> {
            let input: Json = serde_json::from_str(input).expect("the app asks in JSON");
            self.calls.borrow_mut().push((op.to_owned(), input));
            Ok(format!("devices/local/jobs/{op}-1"))
        }

        fn stream(
            &self,
            app: &str,
            op: &str,
            input: &str,
            push: Arc<dyn Push>,
        ) -> Result<String, String> {
            if app != "so.tilo.remote" {
                return Err(format!("`{app}` is not the Remote app"));
            }
            let job = format!("remote-{}", self.streams.borrow().len().saturating_add(1));
            let input: Json = serde_json::from_str(input).expect("the app asks in JSON");
            self.streams
                .borrow_mut()
                .push((job.clone(), op.to_owned(), input));
            push.state(&job, r#"{"phase":"connecting"}"#);
            *self.push.borrow_mut() = Some(push);
            if op == "ssh" {
                // An SSH stream answers the terminal its shell is drawn in, as the engine does.
                Ok(json!({ "job": job, "session": format!("term-{job}") }).to_string())
            } else {
                Ok(json!({ "job": job }).to_string())
            }
        }

        fn job(&self, _path: &str) -> Result<Option<Json>, String> {
            Ok(None)
        }

        fn cancel(&self, job: &str) -> bool {
            self.cancelled.borrow_mut().push(job.to_owned());
            true
        }
    }

    struct Desk {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        here: ResourceRef,
        device: Device,
    }

    impl Desk {
        fn new() -> Self {
            let device = Device::default();
            Self::over(device.clone(), device)
        }

        /// The app on a device whose engine is `engine`; `device` records what a test's own
        /// engine was asked.
        fn over(engine: impl EngineHost + 'static, device: Device) -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            rt.set_engine(engine);
            rt.set_clock(|| "2026-11-26T09:30:00Z".to_owned());
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            let root = rt.create_root("Remote").unwrap();
            let here_ty = app
                .meta()
                .types
                .iter()
                .find(|declared| declared.name == "Here")
                .expect("the device's own type")
                .id
                .clone();
            let here = rt
                .create_root_at(
                    &format!("devices/local/{}/root-{here_ty}", app.meta().app),
                    &here_ty,
                )
                .expect("the device's sessions");
            // The person's devices, as the session publishes them: a Tilo connection picks one.
            let devices = ResourceRef::parse_id("session:devices", "so.tilo.device#Devices")
                .expect("the devices' address");
            rt.set_reading(
                &devices,
                &json!({"devices": [
                    {"id": "aa".repeat(32), "label": "This laptop", "online": true, "nearby": true,
                     "local": true, "display": true, "wakeable": true, "audio_output": true,
                     "seen_ms": 0, "storage": []},
                    {"id": DEVICE, "label": "The studio", "online": true, "nearby": false,
                     "local": false, "display": true, "wakeable": false, "audio_output": false,
                     "seen_ms": 0, "storage": []}
                ]})
                .to_string(),
            )
            .expect("the person's devices read");
            for action in app.meta().actions.iter().map(|a| a.name.clone()) {
                authority.grant_outside(&app.action(&action));
            }
            Self {
                rt,
                app,
                root,
                here,
                device,
            }
        }

        fn call(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            let id = self.app.action(action);
            let args = self.app.args(&self.rt, action, given);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        /// `action` refused, with the words it said.
        fn refused(&self, action: &str, given: &[(&str, Value)]) -> String {
            let args = self.app.args(&self.rt, action, given);
            let id = self.app.action(action);
            match self.rt.invoke_action(&me(), (id.as_str(), args)) {
                Ok(other) => panic!("{action} was expected to be refused, and answered {other:?}"),
                Err(error) => format!("{error:?}"),
            }
        }

        /// The case `variant` of the app's enum `ty`, as a value an action takes.
        fn case(&self, ty: &str, variant: &str) -> Value {
            let ids = &self.app.compiled.ids;
            Value::Enum(EnumValue {
                ty: ids.get(ty).unwrap().to_owned(),
                variant: ids
                    .get(&format!("{ty}.{variant}"))
                    .or_else(|| ids.get(variant))
                    .unwrap_or_else(|| panic!("no case {ty}.{variant}"))
                    .to_owned(),
                fields: BTreeMap::new(),
            })
        }

        /// The records of the collection `field` of `at`.
        fn items(&self, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.rt.read(at, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Ref(held) => ResourceRef::clone(&held.target),
                        other => panic!("an item of {field} is {other:?}"),
                    })
                    .collect(),
                other => panic!("{field} is {other:?}"),
            }
        }

        fn add(&self, given: &[(&str, Value)]) -> ResourceRef {
            let mut all = vec![("r", Value::resource(&self.root))];
            all.extend(given.iter().cloned());
            match self.call("add_connection", &all) {
                InvokeOutcome::Done(Value::Ref(made)) => ResourceRef::clone(&made.target),
                other => panic!("add_connection: {other:?}"),
            }
        }

        fn field(&self, at: &ResourceRef, field: &str) -> Value {
            self.rt.read(at, field).unwrap()
        }

        fn sessions(&self) -> Vec<ResourceRef> {
            self.items(&self.here, "sessions")
        }

        fn connect(&self, c: &ResourceRef) -> ResourceRef {
            match self.call("connect", &[("c", Value::resource(c))]) {
                InvokeOutcome::Done(Value::Ref(made)) => ResourceRef::clone(&made.target),
                other => panic!("connect: {other:?}"),
            }
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("window"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }
    }

    /// The page the window shows for the connection `c`: its session, or why there is none.
    fn page(desk: &Desk, c: &ResourceRef) -> ViewInstance {
        let mut window = desk.window();
        assert!(
            window.show(c).unwrap(),
            "the window answers for a connection"
        );
        window.poll_patches().unwrap();
        window
    }

    /// The first node of `kind` whose `prop` reads `value`.
    fn node<'a>(tree: &'a UiNode, kind: &str, prop: &str, value: &str) -> &'a UiNode {
        of_kind(tree, kind)
            .into_iter()
            .find(|n| match n.props.get(prop) {
                Some(Value::Text(text)) => text == value,
                Some(Value::Opt(Some(held))) => {
                    matches!(&**held, Value::Text(text) if text == value)
                }
                _ => false,
            })
            .unwrap_or_else(|| panic!("no {kind} with {prop} {value} in {}", dump(tree)))
    }

    /// Every title, message and label a tree shows, in document order.
    fn words(node: &UiNode) -> Vec<String> {
        fn go(node: &UiNode, out: &mut Vec<String>) {
            for key in [
                "title",
                "message",
                "label",
                "value",
                "secondary",
                "meta",
                "action",
            ] {
                match node.props.get(key) {
                    Some(Value::Text(text)) if !text.is_empty() => out.push(text.clone()),
                    Some(Value::Opt(Some(held))) => {
                        if let Value::Text(text) = &**held {
                            out.push(text.clone());
                        }
                    }
                    _ => {}
                }
            }
            for child in &node.children {
                go(child, out);
            }
        }
        let mut out = Vec::new();
        go(node, &mut out);
        out
    }

    /// The titles of the list's sections, in order.
    fn headings(window: &ViewInstance) -> Vec<String> {
        of_kind(window.tree(), "ui.Section")
            .into_iter()
            .filter_map(|section| match section.props.get("title") {
                Some(Value::Text(title)) if !title.is_empty() => Some(title.clone()),
                Some(Value::Opt(Some(held))) => match &**held {
                    Value::Text(title) if !title.is_empty() => Some(title.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn shows(window: &ViewInstance, said: &str) -> bool {
        words(window.tree()).iter().any(|word| word.contains(said))
    }

    /// The records of the device's own collection `field`.
    fn kept(desk: &Desk, field: &str) -> Vec<ResourceRef> {
        desk.items(&desk.here, field)
    }

    tilo_test_env::scenario! {
        fn remote_keeps_the_workspaces_connections_and_opens_sessions_as_the_device_says_they_move;

        phase an_empty_workspace_asks_for_the_first_connection;
        {
            let desk = Desk::new();
            let window = desk.window();
            assert!(shows(&window, "No saved connections"), "{}", dump(window.tree()));
            assert!(shows(&window, "New connection"));
        }

        phase a_connection_is_refused_until_it_says_where_and_how;
        {
            let desk = Desk::new();
            let r = Value::resource(&desk.root);
            let why = desk.refused("add_connection", &[("r", r.clone()), ("name", Value::text("  ")), ("host", Value::text("a"))]);
            assert!(why.contains("needs a name"), "{why}");
            let why = desk.refused("add_connection", &[("r", r.clone()), ("name", Value::text("Box")), ("host", Value::text(""))]);
            assert!(why.contains("needs an address"), "{why}");
            let why = desk.refused("add_connection", &[
                ("r", r.clone()), ("name", Value::text("Box")), ("host", Value::text("a")),
                ("port", Value::some(Value::Int(70000))),
            ]);
            assert!(why.contains("1–65535"), "{why}");
            let why = desk.refused("add_connection", &[
                ("r", r.clone()), ("name", Value::text("Desk")), ("host", Value::text("a")),
                ("kind", desk.case("ConnectionKind", "vnc")),
                ("auth", Value::some(desk.case("AuthMethod", "public_key"))),
            ]);
            assert!(why.contains("VNC signs in with a password"), "{why}");
            let why = desk.refused("add_connection", &[
                ("r", r), ("name", Value::text("Studio")), ("host", Value::text("studio.local")),
                ("kind", desk.case("ConnectionKind", "tilo")),
            ]);
            assert!(why.contains("exact endpoint id"), "{why}");
            assert!(desk.items(&desk.root, "connections").is_empty());
        }

        phase the_list_says_where_each_machine_is_and_keeps_the_pinned_ones_first;
        {
            let desk = Desk::new();
            let ssh = desk.add(&[("name", Value::text("Build box")), ("host", Value::text("build.lan")),
                ("user", Value::some(Value::text("ada")))]);
            desk.add(&[("name", Value::text("Odd port")), ("host", Value::text("10.0.0.2")),
                ("port", Value::some(Value::Int(2222))), ("user", Value::some(Value::text("root")))]);
            desk.add(&[("name", Value::text("Kiosk")), ("host", Value::text("kiosk.lan")),
                ("kind", desk.case("ConnectionKind", "vnc"))]);
            desk.add(&[("name", Value::text("Studio")), ("host", Value::text(DEVICE)),
                ("kind", desk.case("ConnectionKind", "tilo"))]);
            desk.add(&[("name", Value::text("Office PC")), ("host", Value::text("office.lan")),
                ("kind", desk.case("ConnectionKind", "rdp"))]);
            assert_eq!(desk.field(&ssh, "port"), Value::Int(22), "SSH's own port is kept");
            let window = desk.window();
            let said = words(window.tree());
            for address in ["ada@build.lan", "root@10.0.0.2:2222", "kiosk.lan", "6f1c2d3e4a5b…", "office.lan"] {
                assert!(said.iter().any(|w| w == address), "{address} in {said:?}");
            }
            assert!(said.iter().any(|w| w == "RDP isn't supported"), "an RDP row says so: {said:?}");
            assert!(headings(&window).is_empty(), "no heading before anything is pinned");
            desk.call("pin", &[("c", Value::resource(&ssh))]);
            assert_eq!(desk.field(&ssh, "favorite"), Value::Bool(true));
            let window = desk.window();
            assert_eq!(headings(&window), ["Pinned", "Other connections"]);
            let rows: Vec<String> = of_kind(window.tree(), "ui.ListRow")
                .into_iter()
                .filter_map(|row| match row.props.get("title") {
                    Some(Value::Text(title)) => Some(title.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(rows.first().map(String::as_str), Some("Build box"), "{rows:?}");
        }

        phase editing_keeps_the_pin_and_the_last_use_and_writes_only_what_changed;
        {
            let desk = Desk::new();
            let c = desk.add(&[("name", Value::text("Build box")), ("host", Value::text("build.lan")),
                ("user", Value::some(Value::text("ada")))]);
            desk.call("pin", &[("c", Value::resource(&c))]);
            desk.call("mark_used", &[("c", Value::resource(&c))]);
            let used = desk.field(&c, "last_used");
            assert!(matches!(used, Value::Opt(Some(_))), "{used:?}");
            desk.call("change_connection", &[
                ("c", Value::resource(&c)), ("name", Value::text(" Build server ")),
                ("host", Value::text("build.lan")), ("kind", desk.case("ConnectionKind", "ssh")),
                ("port", Value::Int(2200)), ("user", Value::text("ada")),
                ("auth", desk.case("AuthMethod", "public_key")), ("group", Value::text("Lab")),
            ]);
            assert_eq!(desk.field(&c, "name"), Value::text("Build server"));
            assert_eq!(desk.field(&c, "port"), Value::Int(2200));
            assert_eq!(desk.field(&c, "group"), Value::text("Lab"));
            assert_eq!(desk.field(&c, "favorite"), Value::Bool(true), "the pin stays");
            assert_eq!(desk.field(&c, "last_used"), used, "the last use stays");
        }

        phase a_password_is_kept_in_the_vault_by_what_it_signs_in_with_and_never_by_the_connection;
        {
            let desk = Desk::new();
            let r = Value::resource(&desk.root);
            let saved = desk.call("save_form", &[
                ("r", r.clone()), ("editing", Value::none()), ("name", Value::text("NAS")),
                ("host", Value::text("nas.lan")), ("port", Value::text("22")),
                ("user", Value::text("ada")), ("kind", desk.case("ConnectionKind", "ssh")),
                ("auth", desk.case("AuthMethod", "password")), ("group", Value::text("")),
                ("secret", Value::text("hunter2")),
            ]);
            assert!(matches!(saved, InvokeOutcome::Done(Value::Bool(true))), "{saved:?}");
            let nas = desk.items(&desk.root, "connections").remove(0);
            let vault: Vec<(String, String)> = desk.device.vault.borrow().clone().into_iter().collect();
            assert_eq!(vault.len(), 1, "{vault:?}");
            assert!(vault[0].0.ends_with(":password"), "kept by what it signs in with: {vault:?}");
            assert_eq!(vault[0].1, "hunter2");
            let record = desk.rt.project(&Value::resource(&nas)).to_string();
            assert!(!record.contains("hunter2"), "the connection holds no password: {record}");
            desk.connect(&nas);
            let (_, _, input) = desk.device.stream();
            assert!(!input.to_string().contains("hunter2"), "no ask carries the password: {input}");
            assert_eq!(desk.device.signs_in_with().as_deref(), Some("hunter2"), "the password signs in");
            // Signing in with a key now: the password is never handed over as its passphrase.
            desk.call("change_connection", &[
                ("c", Value::resource(&nas)), ("name", Value::text("NAS")),
                ("host", Value::text("nas.lan")), ("kind", desk.case("ConnectionKind", "ssh")),
                ("port", Value::Int(22)), ("user", Value::text("ada")),
                ("auth", desk.case("AuthMethod", "public_key")), ("group", Value::text("")),
            ]);
            desk.connect(&nas);
            let (_, _, input) = desk.device.stream();
            assert_eq!(input["method"], "key");
            assert_eq!(desk.device.signs_in_with(), None, "no passphrase was kept: {input}");
            // A form with a bad port saves nothing and keeps nothing.
            let saved = desk.call("save_form", &[
                ("r", r), ("editing", Value::none()), ("name", Value::text("Other")),
                ("host", Value::text("o.lan")), ("port", Value::text("http")),
                ("user", Value::text("")), ("kind", desk.case("ConnectionKind", "ssh")),
                ("auth", desk.case("AuthMethod", "password")), ("group", Value::text("")),
                ("secret", Value::text("x")),
            ]);
            assert!(matches!(saved, InvokeOutcome::Done(Value::Bool(false))), "{saved:?}");
            assert_eq!(desk.device.vault.borrow().len(), 1);
            assert_eq!(desk.items(&desk.root, "connections").len(), 1);
        }

        phase an_ssh_session_shows_its_terminal_once_the_shell_is_up_and_ends_on_disconnect;
        {
            let desk = Desk::new();
            let c = desk.add(&[("name", Value::text("Build box")), ("host", Value::text("build.lan")),
                ("user", Value::some(Value::text("ada")))]);
            // Activating the row asks first, naming where it goes; Connect opens the session page.
            let mut window = desk.window();
            let row = node(window.tree(), "ui.ListRow", "title", "Build box").key.clone();
            window.fire(&row, "activate").unwrap();
            window.poll_patches().unwrap();
            assert!(shows(&window, "Connect to Build box?"), "{}", dump(window.tree()));
            assert!(shows(&window, "ada@build.lan"));
            assert!(desk.device.streams.borrow().is_empty(), "nothing is sent before the yes");
            let dialog = node(window.tree(), "ui.ModalDialog", "title", "Connect to Build box?").key.clone();
            window.fire(&dialog, "act_primary").unwrap();
            window.poll_patches().unwrap();
            let s = desk.sessions().pop().expect("the yes opened a session");
            let (job, op, input) = desk.device.stream();
            assert_eq!(op, "ssh");
            assert_eq!(input["host"], "build.lan");
            assert_eq!(input["port"], 22);
            assert_eq!(input["user"], "ada");
            assert_eq!(input["method"], "password");
            assert_eq!(input["expected"], Json::Null, "no key is trusted yet");
            assert_eq!(desk.field(&s, "job"), Value::text(&job));
            assert!(matches!(desk.field(&c, "last_used"), Value::Opt(Some(_))), "it was used now");
            assert!(shows(&page(&desk, &c), "Connecting to Build box"));
            desk.device.say(&job, &json!({"phase": "connected"}));
            let terminal = page(&desk, &c);
            assert_eq!(
                of_kind(terminal.tree(), "ui.Terminal")[0].props.get("session"),
                Some(&Value::text(format!("term-{job}"))),
                "the shell is the Terminal's own session"
            );
            desk.call("send_text", &[("c", Value::resource(&c)), ("text", Value::text("ls\n"))]);
            assert_eq!(desk.device.calls("ssh-send"), vec![json!({"job": job, "text": "ls\n"})]);
            desk.call("put_file", &[("c", Value::resource(&c)), ("local_path", Value::text("/home/ada/a.txt")),
                ("remote_path", Value::text("/tmp/a.txt"))]);
            desk.call("fetch_file", &[("c", Value::resource(&c)), ("remote_path", Value::text("/etc/hostname")),
                ("local_path", Value::text("/home/ada/hostname"))]);
            assert_eq!(desk.device.calls("put"), vec![json!({"job": job, "local_path": "/home/ada/a.txt", "remote_path": "/tmp/a.txt"})]);
            assert_eq!(desk.device.calls("fetch"), vec![json!({"job": job, "local_path": "/home/ada/hostname", "remote_path": "/etc/hostname"})]);
            desk.call("disconnect", &[("c", Value::resource(&c))]);
            assert_eq!(*desk.device.cancelled.borrow(), vec![job]);
            assert!(desk.sessions().is_empty(), "the session is gone, the connection stays");
            assert_eq!(desk.items(&desk.root, "connections").len(), 1);
            let why = desk.refused("send_text", &[("c", Value::resource(&c)), ("text", Value::text("ls\n"))]);
            assert!(why.contains("No session to this machine is open here"), "{why}");
        }

        phase a_machines_first_key_is_trusted_only_as_it_was_shown_and_a_changed_one_is_never_overwritten;
        {
            let desk = Desk::new();
            let c = desk.add(&[("name", Value::text("New box")), ("host", Value::text("new.lan"))]);
            desk.connect(&c);
            let (job, _, _) = desk.device.stream();
            desk.device.say(&job, &json!({
                "phase": "need-trust",
                "why": "This is the first connection to new.lan. Check its key before you trust it.",
                "algorithm": "ssh-ed25519", "fingerprint": "SHA256:abc",
            }));
            let refused = page(&desk, &c);
            assert!(shows(&refused, "Couldn't connect to New box"));
            assert!(shows(&refused, "Check its key"));
            assert!(shows(&refused, "Trust key"), "{}", dump(refused.tree()));
            let why = desk.refused("trust_host_key", &[("c", Value::resource(&c)), ("fingerprint", Value::text("SHA256:other"))]);
            assert!(why.contains("changed before it was trusted"), "{why}");
            assert!(kept(&desk, "trusted").is_empty());
            desk.call("trust_host_key", &[("c", Value::resource(&c)), ("fingerprint", Value::text("SHA256:abc"))]);
            let trusted = kept(&desk, "trusted");
            assert_eq!(trusted.len(), 1);
            assert_eq!(desk.field(&trusted[0], "fingerprint"), Value::text("SHA256:abc"));
            // Connecting again expects exactly that key: the old stream is stopped.
            desk.connect(&c);
            assert_eq!(desk.device.streams.borrow().len(), 2);
            assert_eq!(*desk.device.cancelled.borrow(), vec![job]);
            assert_eq!(desk.sessions().len(), 1);
            let (job, _, input) = desk.device.stream();
            assert_eq!(input["expected"], json!({"algorithm": "ssh-ed25519", "fingerprint": "SHA256:abc"}));
            // The machine shows another key: the window offers only to forget the old one.
            desk.device.say(&job, &json!({
                "phase": "key-changed",
                "why": "new.lan shows a different key than the one this device trusted.",
                "expected": {"algorithm": "ssh-ed25519", "fingerprint": "SHA256:abc"},
                "observed": {"algorithm": "ssh-ed25519", "fingerprint": "SHA256:evil"},
            }));
            let changed = page(&desk, &c);
            assert!(shows(&changed, "different key"), "{}", dump(changed.tree()));
            assert!(shows(&changed, "Forget key"));
            assert!(!shows(&changed, "Trust key"));
            let why = desk.refused("trust_host_key", &[("c", Value::resource(&c)), ("fingerprint", Value::text("SHA256:evil"))]);
            assert!(why.contains("no new key"), "{why}");
            assert_eq!(desk.field(&kept(&desk, "trusted")[0], "fingerprint"), Value::text("SHA256:abc"));
            desk.call("forget_host_key", &[("c", Value::resource(&c))]);
            assert!(kept(&desk, "trusted").is_empty(), "the old key is forgotten on the person's word");
            let why = desk.refused("forget_host_key", &[("c", Value::resource(&c))]);
            assert!(why.contains("trusts no key"), "{why}");
        }

        phase a_screen_session_shows_the_far_screen_takes_its_input_and_says_when_it_ended;
        {
            let desk = Desk::new();
            let c = desk.add(&[("name", Value::text("Kiosk")), ("host", Value::text("kiosk.lan")),
                ("kind", desk.case("ConnectionKind", "vnc"))]);
            desk.call("keep_secret", &[("c", Value::resource(&c)), ("secret", Value::text("pw"))]);
            desk.connect(&c);
            let (job, op, input) = desk.device.stream();
            assert_eq!(op, "vnc");
            assert_eq!(input["host"], "kiosk.lan");
            assert_eq!(input["port"], 5900);
            assert!(
                input["password_key"].as_str().is_some_and(|key| key.ends_with(":vnc")),
                "the ask names the password, never carries it: {input}"
            );
            assert_eq!(desk.device.signs_in_with().as_deref(), Some("pw"));
            desk.device.say(&job, &json!({"phase": "connected", "width": 1024, "height": 768}));
            let mut screen = page(&desk, &c);
            let shown = of_kind(screen.tree(), "ui.Image");
            assert_eq!(shown.len(), 1, "{}", dump(screen.tree()));
            assert_eq!(shown[0].props.get("src"), Some(&Value::text(format!("tilo-live://{job}"))));
            assert!(
                matches!(shown[0].props.get("input"), Some(Value::Text(t)) if *t == job)
                    || shown[0].props.get("input") == Some(&Value::some(Value::text(&job))),
                "the screen takes the pointer and keys for its stream: {:?}",
                shown[0].props.get("input")
            );
            assert!(shows(&screen, "Full screen"));
            let actual = node(screen.tree(), "ui.Button", "label", "Actual size").key.clone();
            screen.fire(&actual, "press").unwrap();
            screen.poll_patches().unwrap();
            assert!(shows(&screen, "Fit to window"), "{}", dump(screen.tree()));
            desk.call("observe", &[("s", Value::resource(&desk.sessions()[0]))]);
            let seen = desk.items(&desk.here, "seen");
            assert_eq!(desk.field(&seen[0], "online"), Value::Bool(true), "the machine answered");
            desk.device.say(&job, &json!({"phase": "closed"}));
            let ended = page(&desk, &c);
            assert!(shows(&ended, "Kiosk closed the session."), "{}", dump(ended.tree()));
            assert!(shows(&ended, "Reconnect"));
            desk.device.say(&job, &json!({"phase": "failed", "why": "kiosk.lan refused the connection"}));
            let failed = page(&desk, &c);
            assert!(shows(&failed, "kiosk.lan refused the connection"));
            assert!(shows(&failed, "Try again"));
        }

        phase rdp_and_tilo_devices_are_kept_and_say_why_no_session_opens;
        {
            let desk = Desk::new();
            let rdp = desk.add(&[("name", Value::text("Office PC")), ("host", Value::text("office.lan")),
                ("kind", desk.case("ConnectionKind", "rdp"))]);
            assert_eq!(desk.field(&rdp, "port"), Value::Int(3389));
            desk.connect(&rdp);
            assert!(desk.device.streams.borrow().is_empty(), "nothing is opened for RDP");
            let shown = page(&desk, &rdp);
            assert!(shows(&shown, "Remote can't open RDP connections"), "{}", dump(shown.tree()));
            assert!(shows(&shown, "Choose SSH or VNC"));
            let c = desk.add(&[("name", Value::text("Studio")), ("host", Value::text(DEVICE)),
                ("kind", desk.case("ConnectionKind", "tilo"))]);
            assert_eq!(desk.field(&c, "port"), Value::Int(0));
            assert_eq!(desk.field(&c, "auth"), desk.case("AuthMethod", "device_identity"));
            desk.connect(&c);
            assert!(desk.device.streams.borrow().is_empty());
            let shown = page(&desk, &c);
            assert!(shows(&shown, "another Tilo device's screen"), "{}", dump(shown.tree()));
        }

        phase deleting_a_connection_ends_its_session;
        {
            let desk = Desk::new();
            let c = desk.add(&[("name", Value::text("Kiosk")), ("host", Value::text("kiosk.lan")),
                ("kind", desk.case("ConnectionKind", "vnc"))]);
            desk.call("keep_secret", &[("c", Value::resource(&c)), ("secret", Value::text("pw"))]);
            desk.connect(&c);
            let (job, _, _) = desk.device.stream();
            desk.call("delete_connection", &[("r", Value::resource(&desk.root)), ("c", Value::resource(&c))]);
            assert_eq!(*desk.device.cancelled.borrow(), vec![job]);
            assert!(desk.sessions().is_empty());
            assert!(desk.items(&desk.root, "connections").is_empty());
        }

        phase three_hundred_long_named_connections_all_show;
        {
            let desk = Desk::new();
            for i in 0..300 {
                desk.add(&[("name", Value::text(format!("Machine {i:04} with a long name that keeps going"))),
                    ("host", Value::text(format!("host-{i}.lan")))]);
            }
            let window = desk.window();
            assert_eq!(of_kind(window.tree(), "ui.ListRow").len(), 300);
        }
    }

    // ------------------------------------------------------------ real servers

    /// A device whose engine opens real sessions, as the Remote engine does: an ask is read into
    /// its session by `tilo_remote::asked`, with the password from the device's vault.
    #[derive(Clone, Default)]
    struct Real {
        vault: Rc<RefCell<BTreeMap<String, String>>>,
        ssh: Rc<RefCell<BTreeMap<String, tilo_remote::SshSession>>>,
        vnc: Rc<RefCell<BTreeMap<String, tilo_remote::VncSession>>>,
        /// What every stream last said of itself, by its job.
        said: Arc<std::sync::Mutex<BTreeMap<String, Json>>>,
        /// What an SSH shell printed, by its job.
        printed: Arc<std::sync::Mutex<BTreeMap<String, String>>>,
        /// The frames a VNC desktop drew, by its job.
        frames: Arc<std::sync::Mutex<BTreeMap<String, usize>>>,
        /// The identity a key sign-in uses: the engine's are in `~/.ssh`, this test's in its own
        /// directory.
        identity: Rc<RefCell<Option<std::path::PathBuf>>>,
        streams: Rc<RefCell<usize>>,
    }

    impl Real {
        fn said(&self, job: &str) -> Json {
            self.said
                .lock()
                .unwrap()
                .get(job)
                .cloned()
                .unwrap_or(Json::Null)
        }
    }

    impl EngineHost for Real {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            match op {
                "keep-secret" => {
                    let ask: tilo_remote::asked::KeepAsk =
                        serde_json::from_str(input).map_err(|e| e.to_string())?;
                    self.vault.borrow_mut().insert(ask.key, ask.secret);
                    Ok("{}".to_owned())
                }
                "ssh-send" => {
                    let ask: Json = serde_json::from_str(input).map_err(|e| e.to_string())?;
                    let job = ask["job"].as_str().unwrap_or_default();
                    let sessions = self.ssh.borrow();
                    let session = sessions.get(job).ok_or("no such session")?;
                    session.send_input_ack(
                        ask["text"].as_str().unwrap_or_default().as_bytes().to_vec(),
                    )?;
                    Ok("{}".to_owned())
                }
                other => Err(format!("`{other}` is no op of the Remote engine")),
            }
        }

        fn start(&self, _: &str, op: &str, _: &str, _: &JobHandlers) -> Result<String, String> {
            Err(format!("`{op}` is no job of this test"))
        }

        fn stream(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            push: Arc<dyn Push>,
        ) -> Result<String, String> {
            let started = self.streams.borrow().saturating_add(1);
            *self.streams.borrow_mut() = started;
            let job = format!("{op}-{started}");
            let vault = self.vault.borrow().clone();
            let kept = move |key: &str| Ok(vault.get(key).cloned());
            let said = Arc::clone(&self.said);
            let tell = move |job: &str, word: String| {
                said.lock()
                    .unwrap()
                    .insert(job.to_owned(), serde_json::from_str(&word).unwrap());
                push.state(job, &word);
            };
            if op == "ssh" {
                let mut ask: tilo_remote::asked::SshAsk =
                    serde_json::from_str(input).map_err(|e| e.to_string())?;
                ask.key_path = self
                    .identity
                    .borrow()
                    .as_ref()
                    .map(|path| path.display().to_string());
                let config = tilo_remote::asked::ssh_config(ask, &kept)?;
                let named = job.clone();
                let session = tilo_remote::SshSession::start(
                    config,
                    Box::new(move |status| tell(&named, tilo_remote::asked::ssh_word(&status))),
                )?;
                let output = session.take_output().expect("the shell's output");
                let printed = Arc::clone(&self.printed);
                let named = job.clone();
                std::thread::spawn(move || {
                    while let Ok(bytes) = output.recv() {
                        printed
                            .lock()
                            .unwrap()
                            .entry(named.clone())
                            .or_default()
                            .push_str(&String::from_utf8_lossy(&bytes));
                    }
                });
                self.ssh.borrow_mut().insert(job.clone(), session);
                Ok(json!({ "job": job, "session": format!("term-{job}") }).to_string())
            } else {
                let ask: tilo_remote::asked::VncAsk =
                    serde_json::from_str(input).map_err(|e| e.to_string())?;
                let config = tilo_remote::asked::vnc_config(ask, &kept)?;
                let (named, drawn) = (job.clone(), job.clone());
                let frames = Arc::clone(&self.frames);
                let session = tilo_remote::VncSession::start(
                    config,
                    Box::new(move |status| tell(&named, tilo_remote::asked::vnc_word(&status))),
                    Box::new(move |_frame| {
                        frames
                            .lock()
                            .unwrap()
                            .entry(drawn.clone())
                            .and_modify(|drawn| *drawn = drawn.saturating_add(1))
                            .or_insert(1);
                    }),
                )?;
                self.vnc.borrow_mut().insert(job.clone(), session);
                Ok(json!({ "job": job }).to_string())
            }
        }

        fn job(&self, _path: &str) -> Result<Option<Json>, String> {
            Ok(None)
        }

        fn cancel(&self, job: &str) -> bool {
            if let Some(session) = self.ssh.borrow_mut().remove(job) {
                session.close();
            }
            self.vnc.borrow_mut().remove(job);
            true
        }
    }

    /// Waits for `done`, up to the time a server on loopback takes at worst.
    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !done() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn run(program: &str, args: &[&str]) {
        let status = std::process::Command::new(program)
            .args(args)
            .status_locked()
            .unwrap_or_else(|e| panic!("{program}: {e}"));
        assert!(status.success(), "{program} {args:?}: {status}");
    }

    /// A server process this test started, stopped when the test ends.
    struct Server(std::process::Child);

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn listening(port: u16) -> bool {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    }

    /// The form a person fills in for a new connection, saved.
    fn save(
        desk: &Desk,
        name: &str,
        kind: &str,
        auth: &str,
        port: u16,
        user: &str,
        secret: &str,
    ) -> ResourceRef {
        let saved = desk.call(
            "save_form",
            &[
                ("r", Value::resource(&desk.root)),
                ("editing", Value::none()),
                ("name", Value::text(name)),
                ("host", Value::text("127.0.0.1")),
                ("port", Value::text(port.to_string())),
                ("user", Value::text(user)),
                ("kind", desk.case("ConnectionKind", kind)),
                ("auth", desk.case("AuthMethod", auth)),
                ("group", Value::text("")),
                ("secret", Value::text(secret)),
            ],
        );
        assert!(
            matches!(saved, InvokeOutcome::Done(Value::Bool(true))),
            "{saved:?}"
        );
        desk.items(&desk.root, "connections")
            .into_iter()
            .find(|c| desk.field(c, "name") == Value::text(name))
            .expect("the saved connection")
    }

    /// A real `OpenSSH` `sshd` on loopback in `dir`, with a host key of its own, that takes only
    /// the key `dir/id`, locked by the passphrase `correct horse`.
    fn sshd(dir: &std::path::Path) -> (Server, u16) {
        let at = |name: &str| dir.join(name).display().to_string();
        // SSH: a host key, a passphrase-locked identity, and a server that takes only that key.
        run(
            "ssh-keygen",
            &["-q", "-t", "ed25519", "-N", "", "-f", &at("host")],
        );
        run(
            "ssh-keygen",
            &[
                "-q",
                "-t",
                "ed25519",
                "-N",
                "correct horse",
                "-f",
                &at("id"),
            ],
        );
        std::fs::copy(at("id.pub"), at("authorized_keys")).unwrap();
        let ssh_port = free_port();
        std::fs::write(
            at("sshd_config"),
            format!(
                "Port {ssh_port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\n\
             PubkeyAuthentication yes\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
             UsePAM no\nStrictModes no\nPermitRootLogin prohibit-password\nPidFile {}\n",
                at("host"),
                at("authorized_keys"),
                at("sshd.pid"),
            ),
        )
        .unwrap();
        let sshd = Server(
            std::process::Command::new("/usr/sbin/sshd")
                .args(["-D", "-e", "-f", &at("sshd_config")])
                .spawn_locked()
                .expect("OpenSSH's sshd"),
        );
        until("sshd to listen", || listening(ssh_port));
        (sshd, ssh_port)
    }

    /// A person saves an SSH machine that signs in with a key locked by a passphrase kept in the
    /// vault, trusts its key, and the real `OpenSSH` `sshd` runs the command they type.
    #[test]
    #[ignore = "starts a real sshd, which the remote-servers check installs"]
    fn remote_reaches_a_real_sshd_with_the_passphrase_the_vault_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let real = Real::default();
        let desk = Desk::over(real.clone(), Device::default());
        let me = std::process::Command::new("id")
            .arg("-un")
            .output_locked()
            .unwrap();
        let me = String::from_utf8(me.stdout).unwrap().trim().to_owned();

        *real.identity.borrow_mut() = Some(dir.path().join("id"));
        let (_sshd, ssh_port) = sshd(dir.path());

        let c = save(
            &desk,
            "Lab",
            "ssh",
            "public_key",
            ssh_port,
            &me,
            "correct horse",
        );
        assert!(
            !desk
                .rt
                .project(&Value::resource(&c))
                .to_string()
                .contains("correct horse"),
            "the shared connection never holds the passphrase"
        );
        desk.connect(&c);
        until("the first contact", || {
            real.said("ssh-1")["phase"] == "need-trust"
        });
        let shown = page(&desk, &c);
        assert!(shows(&shown, "Trust key"), "{}", dump(shown.tree()));
        let fingerprint = real.said("ssh-1")["fingerprint"]
            .as_str()
            .unwrap()
            .to_owned();
        desk.call(
            "trust_host_key",
            &[
                ("c", Value::resource(&c)),
                ("fingerprint", Value::text(&fingerprint)),
            ],
        );
        desk.connect(&c);
        until("the shell", || {
            let said = real.said("ssh-2");
            assert_ne!(said["phase"], "failed", "the shell did not open: {said}");
            said["phase"] == "connected"
        });
        let terminal = page(&desk, &c);
        assert_eq!(
            of_kind(terminal.tree(), "ui.Terminal").len(),
            1,
            "{}",
            dump(terminal.tree())
        );
        desk.call(
            "send_text",
            &[
                ("c", Value::resource(&c)),
                ("text", Value::text("echo tilo-$((6*7))\n")),
            ],
        );
        until("the shell to run the command", || {
            real.printed
                .lock()
                .unwrap()
                .get("ssh-2")
                .is_some_and(|out| out.contains("tilo-42"))
        });
        desk.call("disconnect", &[("c", Value::resource(&c))]);
    }

    /// A person saves a VNC desktop with its password kept in the vault: a wrong one is refused,
    /// and with the right one the real `TigerVNC` `Xvnc` shows its screen.
    #[test]
    #[ignore = "starts a real Xvnc, which the remote-servers check installs"]
    fn remote_reaches_a_real_xvnc_with_the_password_the_vault_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name).display().to_string();
        let real = Real::default();
        let desk = Desk::over(real.clone(), Device::default());
        // VNC: a desktop that asks for a password.
        let display = (20..99)
            .find(|n| !std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists())
            .expect("a free X display");
        let vnc_port = free_port();
        let made = std::process::Command::new("vncpasswd")
            .arg("-f")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn_locked()
            .and_then(|mut child| {
                use std::io::Write as _;
                child.stdin.take().unwrap().write_all(b"desk-pw\n")?;
                child.wait_with_output()
            })
            .expect("TigerVNC's vncpasswd");
        std::fs::write(at("vnc-passwd"), made.stdout).unwrap();
        let _xvnc = Server(
            std::process::Command::new("Xvnc")
                .args([
                    &format!(":{display}"),
                    "-rfbport",
                    &vnc_port.to_string(),
                    "-localhost",
                    "-SecurityTypes",
                    "VncAuth",
                    "-PasswordFile",
                    &at("vnc-passwd"),
                    "-geometry",
                    "640x480",
                    "-depth",
                    "24",
                    "-nolisten",
                    "tcp",
                ])
                .spawn_locked()
                .expect("TigerVNC's Xvnc"),
        );
        until("Xvnc to listen", || listening(vnc_port));

        let wrong = save(
            &desk,
            "Desk, wrong password",
            "vnc",
            "password",
            vnc_port,
            "",
            "not-it",
        );
        desk.connect(&wrong);
        until("the refusal", || real.said("vnc-1")["phase"] == "failed");
        let refused = page(&desk, &wrong);
        assert!(shows(&refused, "Try again"), "{}", dump(refused.tree()));

        let desktop = save(&desk, "Desk", "vnc", "password", vnc_port, "", "desk-pw");
        desk.connect(&desktop);
        until("the desktop", || {
            let said = real.said("vnc-2");
            assert_ne!(said["phase"], "failed", "the desktop did not open: {said}");
            said["phase"] == "connected"
        });
        assert_eq!(real.said("vnc-2")["width"], 640);
        assert_eq!(real.said("vnc-2")["height"], 480);
        until("its first frame", || {
            real.frames
                .lock()
                .unwrap()
                .get("vnc-2")
                .is_some_and(|drawn| *drawn > 0)
        });
        let screen = page(&desk, &desktop);
        let shown = of_kind(screen.tree(), "ui.Image");
        assert_eq!(shown.len(), 1, "{}", dump(screen.tree()));
        assert_eq!(
            shown[0].props.get("src"),
            Some(&Value::text("tilo-live://vnc-2"))
        );
    }
}
