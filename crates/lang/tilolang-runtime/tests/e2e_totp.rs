//! Authenticator (`apps/tl/totp/app.tl`) end to end: an account started from what a person
//! typed or a setup link said, the code it makes and the instant it changes, a change saved, the
//! list reordered and cut, and the window that shows each account's code. The device's work is
//! the real engine (`crates/engines/totp-engine`), so the test goes through the same answers a
//! session gives.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/totp/app.tl");

    /// The RFC 6238 test secret: "12345678901234567890" in base32.
    const VECTOR: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    /// The device's content store, as a window lends it, kept for the test's length.
    #[derive(Clone)]
    struct Device {
        store: tilo_content::ContentStore,
        scope: Rc<tilo_wake::WakeScope>,
    }

    impl Device {
        fn new() -> Self {
            let store = tilo_content::ContentStore::memory_in_workspace();
            let scope = Rc::new(tilo_wake::WakeScope::new());
            tilo_resource::content::register(&scope.handle(), store.clone());
            Self { store, scope }
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.store.read(hash)
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("this test chooses nothing".to_owned())
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = self.store.put(bytes).map_err(|error| error.to_string())?;
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    /// The device's engine, answered by the crate the session registers, keeping its seeds in
    /// the test's own memory store rather than the machine's.
    #[derive(Clone)]
    struct Engine {
        engine: Rc<totp_engine::TotpEngine>,
        scope: Rc<tilo_wake::WakeScope>,
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            use tilolang_tilo::engine::Provider as _;
            let _window = self.scope.enter();
            self.engine.call(op, input)
        }

        fn start(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            use tilolang_tilo::engine::Provider as _;
            let _window = self.scope.enter();
            self.engine.start(op, input)
        }

        fn status(&self, job: &str) -> Result<String, String> {
            use tilolang_tilo::engine::Provider as _;
            let _window = self.scope.enter();
            self.engine.status(job)
        }

        fn cancel(&self, job: &str) -> bool {
            use tilolang_tilo::engine::Provider as _;
            let _window = self.scope.enter();
            self.engine.cancel(job)
        }
    }

    /// The person's selection, as the device answers it.
    #[derive(Clone, Default)]
    struct Selection(Rc<RefCell<Option<(String, Vec<u8>)>>>);

    impl ClipboardHost for Selection {
        fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
            *self.0.borrow_mut() = Some((mime.to_owned(), bytes.to_vec()));
            true
        }

        fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
            self.0.borrow().clone()
        }
    }

    /// The actions that reach the device: each is granted the outside work its call makes.
    const TOUCHING: &[&str] = &[
        "add_account",
        "import_uri",
        "import_picture",
        "update_account",
        "refresh",
        "refresh_one",
        "save_form",
        "move_up",
        "move_down",
        "delete_account",
        "copy_code",
    ];

    struct Desk {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        selection: Selection,
        device: Device,
        store: tilo_secrets::SecretStore,
    }

    impl Desk {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Device::new();
            let selection = Selection::default();
            let store = tilo_secrets::SecretStore::memory(totp_engine::APP);
            rt.set_blobs(device.clone());
            rt.set_clipboard(selection.clone());
            rt.set_engine(Engine {
                engine: Rc::new(totp_engine::TotpEngine::with_store(store.clone())),
                scope: device.scope.clone(),
            });
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            for action in TOUCHING {
                authority.grant_outside(&app.action(action));
            }
            let root = rt.create_root("Vault").unwrap();
            Self {
                rt,
                app,
                root,
                selection,
                device,
                store,
            }
        }

        /// The seed this device keeps for `a`, as the secret store holds it.
        fn seed(&self, a: &ResourceRef) -> Option<String> {
            let id = a.id_string();
            self.store
                .item("seed", &id)
                .get_blocking()
                .expect("the secret store reads")
                .map(|secret| String::from_utf8(secret.expose().to_vec()).expect("a seed is text"))
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap()
        }

        fn add(&self, issuer: &str, label: &str, secret: &str) -> ResourceRef {
            returned_ref(self.call(
                "add_account",
                vec![
                    Value::resource(&self.root),
                    Value::text(issuer),
                    Value::text(label),
                    Value::text(secret),
                ],
            ))
        }

        fn text(&self, r: &ResourceRef, field: &str) -> String {
            match self.rt.read(r, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn number(&self, r: &ResourceRef, field: &str) -> i64 {
            match self.rt.read(r, field).unwrap() {
                Value::Int(n) => n,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn accounts(&self) -> Vec<ResourceRef> {
            match self.rt.read(&self.root, "accounts").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Ref(r) => Rc::unwrap_or_clone(r.target),
                        other => panic!("an account is a record, not {other:?}"),
                    })
                    .collect(),
                other => panic!("accounts reads {other:?}"),
            }
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view("vault_window"), vec![Value::resource(&self.root)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// A picture as another app hands it over.
        fn picture(&self, bytes: &[u8]) -> FileValue {
            self.device
                .keep("code.png", "image/png", bytes)
                .expect("the picture is kept")
        }
    }

    /// The first node of `kind` in `node`'s tree.
    fn find<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
        if node.kind == kind {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, kind))
    }

    fn texts(node: &UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.Text" {
            if let Some(Value::Text(text)) = node.props.get("value") {
                out.push(text.clone());
            }
        }
        for child in &node.children {
            texts(child, out);
        }
    }

    tilo_test_env::scenario! {
        fn an_account_makes_codes_and_the_window_shows_them;

        phase an_account_makes_a_code_in_threes_and_says_when_it_changes;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            assert_eq!(s.text(&a, "issuer"), "GitHub");
            assert_eq!(s.text(&a, "label"), "ada@example.com");
            assert!(
                s.rt.read(&a, "secret").is_err(),
                "the record carries no key: the device's secret store does"
            );
            assert_eq!(s.seed(&a).as_deref(), Some(VECTOR), "the key is kept on this device");
            let code = s.text(&a, "code");
            assert_eq!(code.len(), 7, "six digits in threes: {code}");
            assert_eq!(code.replace(" ", "").len(), 6);
            assert!(code.chars().all(|c| c.is_ascii_digit() || c == ' '), "{code}");
            assert!(
                !matches!(s.rt.read(&a, "next").unwrap(), Value::Unit),
                "the instant it changes is written"
            );
            // Asking again within the same step keeps the code: the change is per period.
            let before = s.text(&a, "code");
            s.call("refresh", vec![Value::resource(&s.root)]);
            assert_eq!(s.text(&a, "code"), before);
        }

        phase a_setup_link_adds_the_account_it_names;
        {
            let s = Desk::new();
            let made = returned_ref(s.call(
                "import_uri",
                vec![
                    Value::resource(&s.root),
                    Value::text(
                        "otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&issuer=Acme&algorithm=SHA256&digits=8&period=45",
                    ),
                ],
            ));
            assert_eq!(s.text(&made, "issuer"), "Acme");
            assert_eq!(s.text(&made, "label"), "bob");
            assert_eq!(s.text(&made, "algorithm"), "SHA256");
            assert_eq!(s.number(&made, "digits"), 8);
            assert_eq!(s.number(&made, "period"), 45);
            assert_eq!(s.text(&made, "code").replace(" ", "").len(), 8);
            s.call(
                "import_uri",
                vec![Value::resource(&s.root), Value::text("https://example.com")],
            );
            assert_eq!(s.text(&s.root, "notice_kind"), "warning");
            assert_eq!(s.accounts().len(), 1, "the link that was no link added nothing");
        }

        phase a_picture_is_read_for_its_code_and_one_without_a_qr_says_so;
        {
            let s = Desk::new();
            let picture = s.picture(b"not a picture at all");
            s.call(
                "import_picture",
                vec![Value::resource(&s.root), Value::file(picture)],
            );
            let notice = s.text(&s.root, "notice");
            assert!(
                notice.contains("picture") || notice.contains("QR"),
                "{notice}"
            );
            assert!(s.accounts().is_empty());
        }

        phase an_account_is_changed_and_its_code_follows;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            s.call(
                "update_account",
                vec![
                    Value::resource(&a),
                    Value::text("GitHub"),
                    Value::text("ada@work.example"),
                    Value::text("JBSWY3DPEHPK3PXP"),
                    Value::text("SHA512"),
                    Value::Int(8),
                    Value::Int(60),
                ],
            );
            assert_eq!(s.text(&a, "label"), "ada@work.example");
            assert_eq!(s.text(&a, "algorithm"), "SHA512");
            assert_eq!(s.number(&a, "period"), 60);
            assert_eq!(
                s.seed(&a).as_deref(),
                Some("JBSWY3DPEHPK3PXP"),
                "the new key replaced the one this device kept"
            );
            assert_eq!(s.text(&a, "code").replace(" ", "").len(), 8);
            let refused = s.rt.invoke_action(
                &me(),
                (
                    s.app.action("update_account").as_str(),
                    vec![
                        Value::resource(&a),
                        Value::text(""),
                        Value::text(""),
                        Value::text(VECTOR),
                        Value::text("SHA1"),
                        Value::Int(6),
                        Value::Int(30),
                    ],
                ),
            );
            assert!(refused.is_err(), "a nameless account is refused");
            assert_eq!(s.text(&a, "issuer"), "GitHub", "a nameless account was not saved");
        }

        phase accounts_are_reordered_and_deleted;
        {
            let s = Desk::new();
            let first = s.add("GitHub", "", VECTOR);
            let second = s.add("Acme", "", VECTOR);
            let third = s.add("Zed", "", VECTOR);
            s.call("move_down", vec![Value::resource(&s.root), Value::resource(&first)]);
            let order: Vec<String> = s.accounts().iter().map(|a| s.text(a, "issuer")).collect();
            assert_eq!(order, ["Acme", "GitHub", "Zed"]);
            s.call("move_up", vec![Value::resource(&s.root), Value::resource(&third)]);
            let order: Vec<String> = s.accounts().iter().map(|a| s.text(a, "issuer")).collect();
            assert_eq!(order, ["Acme", "Zed", "GitHub"]);
            s.call("delete_account", vec![Value::resource(&s.root), Value::resource(&second)]);
            let order: Vec<String> = s.accounts().iter().map(|a| s.text(a, "issuer")).collect();
            assert_eq!(order, ["Zed", "GitHub"]);
            assert_eq!(s.seed(&second), None, "the key went with the account");
        }

        phase a_code_is_copied_without_its_spaces;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "", VECTOR);
            let code = s.text(&a, "code").replace(" ", "");
            s.call("copy_code", vec![Value::resource(&a)]);
            let held = s.selection.0.borrow().clone().expect("the code was copied");
            assert_eq!(String::from_utf8(held.1).unwrap(), code);
        }

        phase the_window_shows_each_account_and_the_form_that_starts_one;
        {
            let s = Desk::new();
            // Nothing yet: the empty state asks for the first account.
            let window = s.window();
            let empty = find(&window.tree(), "ui.EmptyState").expect("the empty state stands");
            assert_eq!(empty.props.get("title"), Some(&Value::text("No accounts yet")));
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            let window = s.window();
            let card = find(&window.tree(), "ui.Card").expect("the account's card stands in the window");
            assert_eq!(card.props.get("title"), Some(&Value::text("GitHub")));
            let countdown = find(&window.tree(), "ui.Countdown").expect("its code's lifetime shows");
            assert!(countdown.props.contains_key("at"), "the instant it changes");
            let mut said = Vec::new();
            texts(&window.tree(), &mut said);
            assert!(
                said.iter().any(|w| w.contains(' ') && w.replace(" ", "").len() >= 6),
                "the code reads in threes: {said:?}"
            );
            assert_eq!(s.text(&a, "issuer"), "GitHub");
        }

        phase the_form_saves_what_was_typed_and_refuses_a_bad_key;
        {
            let s = Desk::new();
            s.call(
                "save_form",
                vec![
                    Value::resource(&s.root),
                    Value::Opt(None),
                    Value::text("GitHub"),
                    Value::text("ada@example.com"),
                    Value::text(VECTOR),
                    Value::text("SHA1"),
                    Value::Int(6),
                    Value::Int(30),
                ],
            );
            assert_eq!(s.accounts().len(), 1);
            assert_eq!(s.text(&s.root, "notice"), "");
            s.call(
                "save_form",
                vec![
                    Value::resource(&s.root),
                    Value::Opt(None),
                    Value::text("Broken"),
                    Value::text(""),
                    Value::text("not base32!"),
                    Value::text("SHA1"),
                    Value::Int(6),
                    Value::Int(30),
                ],
            );
            assert_eq!(s.accounts().len(), 1, "the bad key added nothing");
            assert_eq!(s.text(&s.root, "notice_kind"), "warning");
            assert!(s.text(&s.root, "notice").contains("base32"));
        }
    }
}
