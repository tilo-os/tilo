//! The App Publisher (`apps/tl/publisher/app.tl`) end to end: this computer's key read, made and
//! copied out, the proof line and where it goes, the application the registry is asked for and
//! the answer kept, and the window that shows all of it. The device's engine is a fake, so no
//! test reaches a real secret store or a real registry.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/publisher/app.tl");

    /// This computer's key, as the engine answers it.
    const NO_KEY: &str = r#"{"state":"missing","public":""}"#;
    const A_KEY: &str = r#"{"state":"present","public":"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"}"#;
    const UNREADABLE: &str = r#"{"state":"unreadable","public":""}"#;
    const A_KEY_FILE: &str = r#"{"text":"{\"secret_key_hex\":\"9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f\"}"}"#;

    /// The public key `A_KEY` names, and the proof line it makes.
    const PUBLIC: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    const PROOF: &str =
        "tilo-publisher=a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

    /// The device's engine, as the app asks it: the key this machine holds, the jobs started and
    /// how the next status answers.
    #[derive(Clone, Default)]
    struct Engine {
        calls: Rc<RefCell<Vec<(String, String)>>>,
        started: Rc<RefCell<Vec<(String, String)>>>,
        cancelled: Rc<RefCell<Vec<String>>>,
        /// What `key-state` and `create` answer.
        key: Rc<RefCell<String>>,
        /// What `key-file` answers.
        key_file: Rc<RefCell<String>>,
        /// What the next `status` answers; nothing while a job still runs.
        status: Rc<RefCell<String>>,
        /// Every key-file ask, and the signature asks.
        next: Rc<RefCell<u32>>,
    }

    impl Engine {
        fn new() -> Self {
            Self {
                key: Rc::new(RefCell::new(NO_KEY.to_owned())),
                key_file: Rc::new(RefCell::new(A_KEY_FILE.to_owned())),
                ..Self::default()
            }
        }

        fn asks(&self) -> Vec<(String, String)> {
            self.calls.borrow().clone()
        }

        fn jobs(&self) -> Vec<(String, String)> {
            self.started.borrow().clone()
        }

        fn set_key(&self, answer: &str) {
            *self.key.borrow_mut() = answer.to_owned();
        }

        fn set_status(&self, answer: &str) {
            *self.status.borrow_mut() = answer.to_owned();
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            match op {
                "key-state" | "create" => Ok(self.key.borrow().clone()),
                "key-file" => Ok(self.key_file.borrow().clone()),
                other => Err(format!("`{other}` is no op this device answers now")),
            }
        }

        fn start(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            _handlers: &tilolang_runtime::host_work::JobHandlers,
        ) -> Result<String, String> {
            self.started
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            let mut next = self.next.borrow_mut();
            *next = next.saturating_add(1);
            Ok(format!(r#"{{"job":"{op}-{next}"}}"#))
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            let said = || -> Result<String, String> {
                let answer = self.status.borrow().clone();
                if answer.is_empty() {
                    return Ok(r#"{"state":"running","summary":"","error":""}"#.to_owned());
                }
                if !job.starts_with("apply-") && !job.starts_with("status-") {
                    return Err(format!("no job `{job}` runs on this device"));
                }
                Ok(answer)
            };
            Ok(said()
                .ok()
                .and_then(|said| tilolang_tilo::engine::record_of(job, &said)))
        }

        fn cancel(&self, job: &str) -> bool {
            self.cancelled.borrow_mut().push(job.to_owned());
            true
        }
    }

    /// The person's clipboard: what a copy leaves there.
    #[derive(Clone, Default)]
    struct Selection(Rc<RefCell<String>>);

    impl ClipboardHost for Selection {
        fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
            if mime == "text/plain" {
                *self.0.borrow_mut() = String::from_utf8_lossy(bytes).into_owned();
                true
            } else {
                false
            }
        }

        fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
            None
        }
    }

    /// The device's content store: the files an action made, by their digest.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _: &FileRequest) -> Result<Vec<FileValue>, String> {
            Ok(Vec::new())
        }

        fn save(&self, _: &SaveRequest, _: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = digest_of(bytes);
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    fn digest_of(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let mut hex = String::with_capacity(17);
        let _ = write!(hex, "h{digest:016x}");
        hex
    }

    /// The app on a machine: its window, its engine, its clipboard and its store.
    struct Publisher {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        engine: Engine,
        selection: Selection,
        device: Device,
        /// This machine's clock, which a test moves between checks.
        clock: Rc<RefCell<String>>,
    }

    impl Publisher {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let clock = Rc::new(RefCell::new("2026-06-30T08:05:00Z".to_owned()));
            let ticking = Rc::clone(&clock);
            rt.set_clock(move || ticking.borrow().clone());
            let engine = Engine::new();
            rt.set_engine(engine.clone());
            let selection = Selection::default();
            rt.set_clipboard(selection.clone());
            let device = Device::default();
            rt.set_blobs(device.clone());
            authority.trust(&app.load(&rt));
            // A copy reaches outside Tilo, which the policy allows for the app it is written in.
            for action in ["copy_proof", "copy_secret", "export_key"] {
                authority.grant_outside(&app.action(action));
            }
            let root = rt.create_root("Publisher").unwrap();
            Self {
                rt,
                app,
                root,
                engine,
                selection,
                device,
                clock,
            }
        }

        /// Moves this machine's clock a minute on, as a second check later does.
        fn a_minute_later(&self) {
            *self.clock.borrow_mut() = "2026-06-30T08:06:00Z".to_owned();
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

        /// Invokes `action` and answers why it refused, as `require` does.
        fn refused(&self, action: &str) -> String {
            let id = self.app.action(action);
            match self
                .rt
                .invoke_action(&me(), (id.as_str(), vec![Value::resource(&self.root)]))
            {
                Err(error) => format!("{error:?}"),
                Ok(other) => panic!("{action} was expected to refuse: {other:?}"),
            }
        }

        fn act(&self, action: &str) {
            self.call(action, vec![Value::resource(&self.root)]);
        }

        fn text(&self, field: &str) -> String {
            match self.rt.read(&self.root, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} is text, not {other:?}"),
            }
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("publisher_window"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap_or_else(|e| panic!("publisher_window: {e}"));
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// Types `text` into the window's field that says `placeholder`, as a person does.
        fn type_into(&self, window: &ViewInstance, placeholder: &str, text: &str) {
            let input = of_kind(window.tree(), "ui.TextInput")
                .into_iter()
                .find(|node| prop(node, "placeholder") == placeholder)
                .unwrap_or_else(|| panic!("the field {placeholder:?}"));
            window.edit(&input.key, &Value::text(text)).unwrap();
        }

        /// The proof lines the window shows under "Where to publish the proof".
        fn places(&self, window: &ViewInstance) -> Vec<String> {
            of_kind(window.tree(), "ui.Property")
                .into_iter()
                .filter(|node| prop(node, "label") == "Publish at")
                .map(|node| prop(node, "value"))
                .collect()
        }

        /// The readings the window lists, newest first, as their summaries.
        fn readings(&self, window: &ViewInstance) -> Vec<String> {
            of_kind(window.tree(), "ui.ListRow")
                .into_iter()
                .map(|node| prop(node, "title"))
                .collect()
        }
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

    #[test]
    fn the_profile_holds_the_registry_and_the_namespaces_to_claim() {
        let p = Publisher::new();
        assert_eq!(p.text("registry"), "https://apps.tilo.so");
        assert_eq!(p.text("domain"), "");
        assert_eq!(p.text("forge_account"), "");
        assert_eq!(p.text("contact"), "");

        let window = p.window();
        p.type_into(&window, "example.com", "lovelace.dev");
        p.type_into(&window, "github:ada", "codeberg:ada");
        p.type_into(&window, "you@example.com", "ada@lovelace.dev");
        p.type_into(&window, "https://apps.tilo.so", "https://apps.example.com");
        assert_eq!(p.text("domain"), "lovelace.dev");
        assert_eq!(p.text("forge_account"), "codeberg:ada");
        assert_eq!(p.text("contact"), "ada@lovelace.dev");
        assert_eq!(p.text("registry"), "https://apps.example.com");
    }

    #[test]
    fn opening_the_app_reads_this_computers_key() {
        let p = Publisher::new();
        p.engine.set_key(A_KEY);
        p.act("open_key");
        assert_eq!(p.text("key"), "present");
        assert_eq!(p.text("public_key"), PUBLIC);
        assert_eq!(
            p.engine.asks(),
            [("key-state".to_owned(), String::new())],
            "the device is asked for this computer's key, and nothing else"
        );

        p.engine.set_key(NO_KEY);
        p.act("open_key");
        assert_eq!(p.text("key"), "missing");
        assert_eq!(p.text("public_key"), "");

        p.engine.set_key(UNREADABLE);
        p.act("open_key");
        assert_eq!(p.text("key"), "unreadable");
        assert_eq!(p.text("public_key"), "");
    }

    #[test]
    fn the_key_is_made_once_on_this_computer() {
        let p = Publisher::new();
        p.engine.set_key(A_KEY);
        p.act("create_key");
        assert_eq!(p.text("key"), "present");
        assert_eq!(p.text("public_key"), PUBLIC);
        assert_eq!(p.text("notice_kind"), "success");
        assert_eq!(p.text("notice_title"), "Created your publisher key");
        assert!(
            p.text("notice_detail").contains(PUBLIC),
            "the detail names the public key: {}",
            p.text("notice_detail")
        );
        assert_eq!(
            p.engine.asks(),
            [("create".to_owned(), String::new())],
            "the device makes the key; the app never sees the secret"
        );

        // A machine whose key cannot be made says why.
        let p = Publisher::new();
        p.engine.set_key(UNREADABLE);
        p.act("create_key");
        assert_eq!(p.text("key"), "unreadable");
    }

    #[test]
    fn the_proof_line_is_the_public_keys_and_it_copies() {
        let p = Publisher::new();
        p.engine.set_key(A_KEY);
        p.act("open_key");
        p.act("copy_proof");
        assert_eq!(p.selection.0.borrow().as_str(), PROOF);
        assert_eq!(p.text("notice_title"), "Copied the proof line.");
        assert_eq!(p.text("notice_detail"), PROOF);

        let p = Publisher::new();
        p.act("copy_proof");
        assert_eq!(p.text("notice_kind"), "error");
        assert!(
            p.text("notice_detail").contains("no publisher key"),
            "nothing to copy before there is a key: {}",
            p.text("notice_detail")
        );
    }

    #[test]
    fn the_key_file_copies_for_ci_and_exports() {
        let p = Publisher::new();
        p.engine.set_key(A_KEY);
        p.act("copy_secret");
        assert_eq!(
            p.engine.asks(),
            [("key-file".to_owned(), String::new())],
            "the key file comes from the device"
        );
        assert_eq!(
            p.selection.0.borrow().as_str(),
            r#"{"secret_key_hex":"9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f"}"#
        );
        assert_eq!(p.text("notice_kind"), "success");
        assert_eq!(p.text("notice_title"), "Copied the key");

        let Value::File(file) = p.done("export_key", vec![Value::resource(&p.root)]) else {
            panic!("an export answers a file");
        };
        assert_eq!(file.name, "publisher-key.json");
        let bytes = p.device.0.borrow().get(&file.hash).cloned();
        assert_eq!(
            bytes.as_deref().map(String::from_utf8_lossy),
            Some("{\"secret_key_hex\":\"9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f\"}".into()),
            "the file the person keeps is the key file"
        );
        assert_eq!(p.text("notice_title"), "Exported your publisher key");
    }

    #[test]
    fn an_application_starts_as_a_job_and_its_answer_is_kept() {
        let p = Publisher::new();
        p.engine.set_key(A_KEY);
        let window = p.window();
        p.type_into(&window, "example.com", "lovelace.dev");
        p.type_into(&window, "github:ada", "codeberg:ada");
        p.type_into(&window, "you@example.com", "ada@lovelace.dev");
        p.act("apply_for_namespaces");
        assert_eq!(p.text("job"), "apply-1", "the registry call runs as a job");
        let asked = p.engine.jobs();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, "apply");
        let ask: serde_json::Value = serde_json::from_str(&asked[0].1).unwrap();
        assert_eq!(ask["registry"], "https://apps.tilo.so");
        assert_eq!(ask["domain"], "lovelace.dev");
        assert_eq!(ask["forge_account"], "codeberg:ada");
        assert_eq!(ask["contact"], "ada@lovelace.dev");

        p.engine
            .set_status(r#"{"state":"done","summary":"pending (dev.lovelace)","error":""}"#);
        p.act("poll");
        assert_eq!(p.text("job"), "", "the call ended");
        assert_eq!(p.text("notice_kind"), "info");
        assert_eq!(p.text("notice_title"), "Application status");
        assert_eq!(p.text("notice_detail"), "pending (dev.lovelace)");
        assert_eq!(
            p.readings(&p.window()),
            ["pending (dev.lovelace)"],
            "the answer is kept as the application's newest reading"
        );

        // A second check joins the first, newest first.
        p.a_minute_later();
        p.engine
            .set_status(r#"{"state":"done","summary":"approved (dev.lovelace)","error":""}"#);
        p.act("check_status");
        p.act("poll");
        assert_eq!(
            p.readings(&p.window()),
            ["approved (dev.lovelace)", "pending (dev.lovelace)"]
        );
    }

    #[test]
    fn an_application_the_registry_knows_nothing_of_says_so() {
        let p = Publisher::new();
        p.engine.set_key(A_KEY);
        p.act("check_status");
        p.engine
            .set_status(r#"{"state":"done","summary":"","error":""}"#);
        p.act("poll");
        assert_eq!(p.text("notice_title"), "No application yet");
        assert!(
            p.text("notice_detail")
                .contains("no application from this key"),
            "{}",
            p.text("notice_detail")
        );
    }

    #[test]
    fn a_registry_call_that_fails_says_why() {
        let p = Publisher::new();
        p.engine.set_key(A_KEY);
        let window = p.window();
        p.type_into(&window, "example.com", "lovelace.dev");
        p.act("apply_for_namespaces");
        assert_eq!(p.text("job"), "apply-1");
        p.engine.set_status(
            r#"{"state":"failed","summary":"","error":"missing proof at DNS TXT _tilo-publisher.lovelace.dev"}"#,
        );
        p.act("poll");
        assert_eq!(p.text("job"), "");
        assert_eq!(p.text("notice_kind"), "error");
        assert!(p.text("notice_detail").contains("missing proof"));
        assert!(
            p.readings(&p.window()).is_empty(),
            "a failed call is no reading of the application"
        );
    }

    #[test]
    fn a_registry_must_be_https_or_this_machine() {
        let p = Publisher::new();
        p.engine.set_key(A_KEY);
        let window = p.window();
        p.type_into(&window, "example.com", "lovelace.dev");
        for address in [
            "http://apps.tilo.so",
            "http://localhost.example.com",
            "not a URL",
            "file:///tmp/registry",
            "https://",
        ] {
            p.type_into(&window, "https://apps.tilo.so", address);
            let refused = p.refused("apply_for_namespaces");
            assert!(
                refused.contains("credential-free https"),
                "{address}: {refused}"
            );
            assert_eq!(p.text("job"), "", "nothing is sent to a refused address");
        }
        for address in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://localhost.:8080",
            "https://apps.example.com",
        ] {
            p.type_into(&window, "https://apps.tilo.so", address);
            p.act("apply_for_namespaces");
            assert_ne!(p.text("job"), "", "{address} is a registry");
            p.engine
                .set_status(r#"{"state":"cancelled","summary":"","error":""}"#);
            p.act("poll");
        }
    }

    #[test]
    fn the_proof_places_follow_the_claims() {
        let p = Publisher::new();
        let window = p.window();
        assert!(
            p.places(&window).is_empty(),
            "no claim, no place to publish the proof"
        );

        p.type_into(&window, "example.com", "lovelace.dev");
        assert_eq!(
            p.places(&p.window()),
            [
                "DNS TXT _tilo-publisher.lovelace.dev",
                "or https://lovelace.dev/.well-known/tilo-publisher.txt",
            ]
        );

        let p = Publisher::new();
        let window = p.window();
        p.type_into(&window, "github:ada", "github:ada");
        assert_eq!(
            p.places(&p.window()),
            ["https://github.com/ada/tilo-publisher"]
        );

        let p = Publisher::new();
        let window = p.window();
        p.type_into(&window, "github:ada", "git.example.net:ada");
        assert_eq!(
            p.places(&p.window()),
            ["https://git.example.net/ada/tilo-publisher"]
        );

        let p = Publisher::new();
        let window = p.window();
        p.type_into(&window, "github:ada", "nowhere");
        assert!(
            p.places(&p.window()).is_empty(),
            "a forge without an account has no proof place"
        );
    }

    #[test]
    fn the_window_shows_the_key_and_what_still_has_to_be_done() {
        let p = Publisher::new();
        p.act("open_key");
        let window = p.window();
        let labels: Vec<String> = of_kind(window.tree(), "ui.Button")
            .into_iter()
            .map(|node| prop(node, "label"))
            .collect();
        assert!(labels.contains(&"Create key".to_owned()), "{labels:?}");
        assert!(
            !labels.contains(&"Copy proof line".to_owned()),
            "{labels:?}"
        );

        p.engine.set_key(A_KEY);
        p.act("open_key");
        let window = p.window();
        let labels: Vec<String> = of_kind(window.tree(), "ui.Button")
            .into_iter()
            .map(|node| prop(node, "label"))
            .collect();
        for button in [
            "Copy proof line",
            "Copy key for CI",
            "Export key…",
            "Check application",
            "Apply",
        ] {
            assert!(labels.contains(&button.to_owned()), "{button}: {labels:?}");
        }
        let facts: Vec<(String, String)> = of_kind(window.tree(), "ui.Property")
            .into_iter()
            .map(|node| (prop(node, "label"), prop(node, "value")))
            .collect();
        assert!(
            facts.contains(&("Public key".to_owned(), PUBLIC.to_owned())),
            "{facts:?}"
        );
        assert!(
            facts.contains(&("Proof line".to_owned(), PROOF.to_owned())),
            "{facts:?}"
        );
    }

    #[test]
    fn an_unreadable_key_says_what_to_do() {
        let p = Publisher::new();
        p.engine.set_key(UNREADABLE);
        p.act("open_key");
        let window = p.window();
        let banners = of_kind(window.tree(), "ui.Banner");
        let words: Vec<String> = banners.iter().map(|node| prop(node, "title")).collect();
        assert!(
            words.iter().any(|title| title.contains("can’t be read")),
            "{words:?}"
        );
    }
}
