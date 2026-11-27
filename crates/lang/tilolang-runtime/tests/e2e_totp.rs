//! Authenticator (`apps/tl/totp/app.tl`) end to end: an account kept as a vault item, the code
//! its seed makes read from where the vault keeps it, a setup link or a picture read by the
//! device, the form that starts or changes an account, the list reordered and cut, and the
//! window that shows each account's code.
//!
//! The vault is the test's: it holds no seed of its own, it answers `keep`, `forget` and each
//! item's code, as the workspace's vault does where the app runs. Nothing the app writes holds a
//! key or a code, and reading a code changes nothing.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileValue};
    use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
    use tilolang_runtime::host_work::{VaultCode, VaultHost};
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/totp/app.tl");
    const VAULT: &str = include_str!("../../../../apps/tl/vault/surface.json");

    /// The RFC 6238 test secret: "12345678901234567890" in base32.
    const VECTOR: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    /// The code every item of the test's vault makes, and the instant it stops being good.
    const CODE: &str = "123456";
    const UNTIL: &str = "2030-01-01T00:00:30Z";

    /// What the workspace's vault holds: each item's reference by the seed it keeps, and the
    /// words it was kept with. It answers the code of this moment and the writes, as the vault
    /// does, and keeps every call so the test sees what the app asked.
    #[derive(Clone, Default)]
    struct Vault {
        /// Every item the vault holds, by its reference: its seed and the words it was kept with.
        kept: Rc<RefCell<BTreeMap<String, Kept>>>,
        /// The next item's id.
        next: Rc<RefCell<u32>>,
        /// What the app asked the vault, in order.
        calls: Rc<RefCell<Vec<String>>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Kept {
        secret: String,
        issuer: String,
        label: String,
        algorithm: String,
        digits: i64,
        period: i64,
    }

    impl Vault {
        fn new() -> Self {
            Self::default()
        }

        fn kept(&self) -> BTreeMap<String, Kept> {
            self.kept.borrow().clone()
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    /// One argument of a call by its parameter's name, `None` when it was left out.
    fn argument<'a>(args: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
        args.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    fn text(args: &[(String, Value)], name: &str) -> String {
        match argument(args, name) {
            Some(Value::Text(text)) => text.clone(),
            Some(Value::Opt(Some(inner))) => match inner.as_ref() {
                Value::Text(text) => text.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    fn number(args: &[(String, Value)], name: &str, fallback: i64) -> i64 {
        match argument(args, name) {
            Some(Value::Int(number)) => *number,
            Some(Value::Opt(Some(inner))) => match inner.as_ref() {
                Value::Int(number) => *number,
                _ => fallback,
            },
            _ => fallback,
        }
    }

    impl Invoker for Vault {
        fn invoke(&self, invocation: &Invocation) -> Invoked {
            let Invocation::Action {
                app, name, args, ..
            } = invocation
            else {
                return Invoked::Failed("the vault answers actions alone".to_owned());
            };
            if app.as_str() != "so.tilo.vault" {
                return Invoked::Failed(format!(
                    "this test's vault is no {app}, and `{name}` is not its own"
                ));
            }
            self.calls.borrow_mut().push(name.clone());
            match name.as_str() {
                "keep" => {
                    let changing = match argument(args, "item") {
                        Some(Value::Opt(Some(inner))) => match inner.as_ref() {
                            Value::Text(reference) => Some(reference.clone()),
                            _ => None,
                        },
                        _ => None,
                    };
                    let reference = match changing {
                        Some(reference) if self.kept.borrow().contains_key(&reference) => reference,
                        _ => {
                            let mut next = self.next.borrow_mut();
                            *next = next.saturating_add(1);
                            format!("vault.item:{:032x}", *next)
                        }
                    };
                    let secret = text(args, "secret");
                    let mut kept = self.kept.borrow_mut();
                    let previous = kept.get(&reference).cloned();
                    kept.insert(
                        reference.clone(),
                        Kept {
                            secret: if secret.is_empty() {
                                previous.map(|kept| kept.secret).unwrap_or_default()
                            } else {
                                secret
                            },
                            issuer: text(args, "issuer"),
                            label: text(args, "label"),
                            algorithm: {
                                let algorithm = text(args, "algorithm");
                                if algorithm.is_empty() {
                                    "SHA1".to_owned()
                                } else {
                                    algorithm
                                }
                            },
                            digits: number(args, "digits", 6),
                            period: number(args, "period", 30),
                        },
                    );
                    Invoked::Done(Value::text(reference))
                }
                "forget" => {
                    let reference = text(args, "item");
                    self.kept.borrow_mut().remove(&reference);
                    Invoked::Done(Value::Unit)
                }
                other => Invoked::Failed(format!("the vault has no `{other}`")),
            }
        }
    }

    /// What the runtime sends an invocation to: the vault's calls of itself, and an own action
    /// that waits for the person kept as an ask, never run before their yes.
    #[derive(Clone)]
    struct Router {
        vault: Vault,
        dispatch: Dispatch,
    }

    impl Invoker for Router {
        fn invoke(&self, invocation: &Invocation) -> Invoked {
            if invocation.app() == "so.tilo.vault" {
                self.vault.invoke(invocation)
            } else {
                self.dispatch.invoke(invocation)
            }
        }
    }

    impl VaultHost for Vault {
        fn code(&self, reference: &str) -> Option<VaultCode> {
            let kept = self.kept.borrow();
            let item = kept.get(reference)?;
            Some(VaultCode {
                code: CODE.to_owned(),
                until: UNTIL.to_owned(),
                period: item.period,
                digits: item.digits,
                algorithm: item.algorithm.clone(),
            })
        }
    }

    /// The actions that reach the device: each is granted the outside work its call makes.
    const TOUCHING: &[&str] = &[
        "add_account",
        "import_uri",
        "scan_picture",
        "add_pending",
        "drop_pending",
        "rename_account",
        "relabel_account",
        "recolor_account",
        "recode_account",
        "update_account",
        "save_account",
        "move_up",
        "move_down",
        "delete_account",
        "copy_code",
    ];

    struct Desk {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        selection: Clipboard,
        vault: Vault,
        device: Files,
    }

    impl Desk {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Files::default();
            let selection = Clipboard::default();
            rt.set_blobs(device.clone());
            rt.set_clipboard(selection.clone());
            let totp = totp_engine::TotpEngine::new();
            rt.set_engine(DeviceEngine::calls(
                move |op, input| totp.call(op, input),
                &device,
            ));
            let vault = Vault::new();
            // The vault answers the app's calls of it; an action of the app's own that waits for
            // the person is kept as the ask the dispatch would hold, as a host's does.
            rt.set_foreign_invoker(Router {
                vault: vault.clone(),
                dispatch: Dispatch::new(),
            });
            rt.set_vault(vault.clone());
            let package = tilolang_tilo::catalogue::described("so.tilo.vault", "Vault", VAULT)
                .expect("the vault's package reads");
            // The vault is a package of its own, described beside the app as a host describes
            // the packages a window uses: its actions are named by the registry the app reads.
            rt.load_foreign(package.clone())
                .expect("the vault's package loads");
            let app = App::compile(SOURCE, &[package]);
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
                vault,
                device,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn add(&self, issuer: &str, label: &str, secret: &str) -> ResourceRef {
            self.add_coloured(issuer, label, secret, 0)
        }

        /// The account `issuer` names, kept with the colour slot it shows by.
        fn add_coloured(&self, issuer: &str, label: &str, secret: &str, color: i64) -> ResourceRef {
            returned_ref(self.call(
                "add_account",
                vec![
                    Value::resource(&self.root),
                    Value::text(issuer),
                    Value::text(label),
                    Value::text(secret),
                    Value::text("SHA1"),
                    Value::Int(6),
                    Value::Int(30),
                    Value::Int(color),
                ],
            ))
        }

        /// What the device says about its clock, as the session publishes it beside the time:
        /// `None` for a device whose platform cannot say.
        fn clock(&self, synchronised: Option<bool>) {
            let said = match synchronised {
                Some(said) => format!(r#"{{"synchronised": {said}}}"#),
                None => "{}".to_owned(),
            };
            let at = ResourceRef::parse_id("session:clock", "so.tilo.device#Clock")
                .expect("the clock's address");
            self.rt
                .set_reading(&at, &said)
                .expect("the clock reading reads");
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
                .open_view(
                    &me(),
                    &self.app.view("vault_window"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn picture(&self, bytes: &[u8]) -> FileValue {
            self.device
                .keep("code.png", "image/png", bytes)
                .expect("the picture is kept")
        }
    }

    impl Reads for Desk {
        fn rt(&self) -> &Runtime {
            &self.rt
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
        if node.kind == "ui.Text"
            && let Some(Value::Text(text)) = node.props.get("value")
        {
            out.push(text.clone());
        }
        for child in &node.children {
            texts(child, out);
        }
    }

    /// Every value the window's nodes carry, and every word it says.
    fn shown(window: &ViewInstance) -> Vec<String> {
        fn props(node: &UiNode, out: &mut Vec<String>) {
            for value in node.props.values() {
                if let Value::Text(text) = value {
                    out.push(text.clone());
                }
            }
            for child in &node.children {
                props(child, out);
            }
        }
        let mut said = Vec::new();
        texts(window.tree(), &mut said);
        props(window.tree(), &mut said);
        said
    }

    /// The node of `kind` that says `words`, by its children's words or its own `label`.
    fn keyed(view: &ViewInstance, kind: &str, words: &str) -> String {
        let node = of_kind(view.tree(), kind)
            .into_iter()
            .find(|node| {
                let mut said = Vec::new();
                texts(node, &mut said);
                said.iter().any(|text| text == words)
                    || node.props.get("label") == Some(&Value::text(words))
            })
            .unwrap_or_else(|| panic!("no {kind} says {words}: {:?}", shown(view)));
        node.key.clone()
    }

    /// The key of the window's card grid, as it stands now.
    fn grid_key(s: &Desk) -> String {
        let window = s.window();
        of_kind(window.tree(), "ui.CardGrid")
            .first()
            .expect("the list is one grid")
            .key
            .clone()
    }

    /// The period field, under the options it stands in.
    fn period_field(view: &ViewInstance) -> String {
        let node = of_kind(view.tree(), "ui.NumberInput")
            .into_iter()
            .find(|node| node.props.get("label") == Some(&Value::text("Period in seconds")))
            .unwrap_or_else(|| panic!("no period field: {:?}", shown(view)));
        node.key.clone()
    }

    /// The field a person types into, by the words standing in it while it is empty.
    fn field(view: &ViewInstance, placeholder: &str) -> String {
        let node = of_kind(view.tree(), "ui.TextInput")
            .into_iter()
            .find(|node| node.props.get("placeholder") == Some(&Value::text(placeholder)))
            .unwrap_or_else(|| panic!("no field says {placeholder}"));
        node.key.clone()
    }

    /// What the view state `name` holds, as text: `""` where it is unset.
    fn state_text(view: &ViewInstance, name: &str) -> String {
        match view.view_state(name).as_ref().map(unslotted) {
            None => String::new(),
            Some(Value::Text(text)) => text.clone(),
            Some(other) => panic!("{name} holds {other:?}"),
        }
    }

    /// A flag as a slot keeps it: a scalar is kept as the number it boxes to, or as the flag
    /// itself where the value came whole.
    fn truthy(value: Option<&Value>) -> bool {
        match value.map(unslotted) {
            None => false,
            Some(Value::Bool(said)) => *said,
            Some(Value::Int(said)) => *said != 0,
            Some(other) => panic!("a flag reads {other:?}"),
        }
    }

    /// A state as it is read: a scalar is kept in a slot, which reads as the value it holds.
    fn unslotted(value: &Value) -> &Value {
        match value {
            Value::Opt(Some(inner)) => unslotted(inner),
            other => other,
        }
    }

    /// Presses the button that says `words` in the window as it stands now.
    fn press(s: &Desk, words: &str) {
        let window = s.window();
        let key = keyed(&window, "ui.Button", words);
        window.press(&key).expect("the press runs");
    }

    /// A setup link drawn as the QR code a person photographs, as a PNG: the picture the scan
    /// reads, beside the bytes of one it cannot.
    fn qr_png(uri: &str) -> Vec<u8> {
        use image::{ExtendedColorType, ImageEncoder};
        let code = qrcode::QrCode::with_error_correction_level(uri.as_bytes(), qrcode::EcLevel::M)
            .expect("a setup link fits a QR code");
        let modules = code.to_colors();
        let side = code.width();
        let (scale, quiet) = (6usize, 4usize);
        let pixels_side = side
            .saturating_add(quiet.saturating_mul(2))
            .saturating_mul(scale);
        let mut rgba = vec![255u8; pixels_side.saturating_mul(pixels_side).saturating_mul(4)];
        for y in 0..side {
            for x in 0..side {
                if modules[y.saturating_mul(side).saturating_add(x)] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = x
                                .saturating_add(quiet)
                                .saturating_mul(scale)
                                .saturating_add(dx);
                            let py = y
                                .saturating_add(quiet)
                                .saturating_mul(scale)
                                .saturating_add(dy);
                            let at = py
                                .saturating_mul(pixels_side)
                                .saturating_add(px)
                                .saturating_mul(4);
                            rgba[at..at.saturating_add(4)].copy_from_slice(&[0, 0, 0, 255]);
                        }
                    }
                }
            }
        }
        let (width, height) = (
            u32::try_from(pixels_side).expect("a QR picture is a few hundred pixels wide"),
            u32::try_from(pixels_side).expect("a QR picture is a few hundred pixels tall"),
        );
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, width, height, ExtendedColorType::Rgba8)
            .expect("the picture encodes as a PNG");
        png
    }

    /// The notices the app said in passing, as (words, kind) pairs.
    fn said(s: &Desk) -> Vec<(String, String)> {
        s.rt.take_said()
            .into_iter()
            .map(|said| (said.text, said.kind.word().to_owned()))
            .collect()
    }

    tilo_test_env::scenario! {
        fn an_account_is_a_vault_item_and_its_code_reads_where_the_seed_is;

        phase an_account_is_kept_in_the_vault_and_the_record_holds_no_key;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            assert_eq!(s.text(&a, "issuer"), "GitHub");
            assert_eq!(s.text(&a, "label"), "ada@example.com");
            let reference = s.text(&a, "item");
            assert!(reference.starts_with("vault.item:"), "{reference}");
            assert_eq!(
                s.vault.kept().get(&reference).map(|kept| kept.secret.clone()),
                Some(VECTOR.to_owned()),
                "the vault keeps the seed"
            );
            // The record holds no key and no code: nothing the app writes does.
            for field in ["secret", "key", "code", "digits", "period", "next"] {
                assert!(s.rt.read(&a, field).is_err(), "the account has no {field}");
            }
            // The window veils the code the vault makes until the person asks for it.
            let window = s.window();
            let mut said = Vec::new();
            texts(window.tree(), &mut said);
            assert!(
                said.iter().any(|word| word == "••• •••"),
                "the code is veiled at rest: {said:?}"
            );
            assert!(
                !said.iter().any(|word| word == "123 456"),
                "no code reads until it is asked for: {said:?}"
            );
            let countdown = find(window.tree(), "ui.Countdown").expect("its code's lifetime shows");
            assert_eq!(countdown.props.get("at"), Some(&Value::text(UNTIL)));
        }

        phase reading_a_code_changes_nothing_and_keeps_no_state;
        {
            let s = Desk::new();
            let _a = s.add("GitHub", "ada@example.com", VECTOR);
            let history = s.rt.history(&s.root.doc).expect("the root's history reads");
            let before = history.len();
            for _ in 0..3 {
                let window = s.window();
                let _ = shown(&window);
            }
            let after = s.rt.history(&s.root.doc).expect("the root's history reads");
            assert_eq!(after.len(), before, "showing a code records nothing");
            // No window state holds the code or the seed: the states are the form's, and none
            // of them began holding a code.
            let states: Vec<String> = s
                .app
                .meta()
                .views
                .iter()
                .filter(|view| view.id == s.app.view("vault_window"))
                .flat_map(|view| view.state.iter().map(|state| state.name.clone()))
                .collect();
            assert!(
                states.iter().all(|name| !name.to_lowercase().contains("code")),
                "{states:?}"
            );
            for name in &states {
                if name == "secret" {
                    continue;
                }
                let window = s.window();
                assert!(
                    !shown(&window).contains(&VECTOR.to_owned()),
                    "no state holds the seed"
                );
                let _ = window;
            }
            assert!(!s.vault.calls().iter().any(|call| call == "code"));
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
            let kept = s
                .vault
                .kept()
                .into_values()
                .next()
                .expect("the vault kept the account");
            assert_eq!(kept.secret, "JBSWY3DPEHPK3PXP");
            assert_eq!(kept.algorithm, "SHA256");
            assert_eq!(kept.digits, 8);
            assert_eq!(kept.period, 45);
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
                "scan_picture",
                vec![Value::resource(&s.root), Value::file(picture)],
            );
            assert_eq!(s.text(&s.root, "pending"), "", "a picture that read nothing keeps none");
            let notice = s.text(&s.root, "notice");
            assert!(
                notice.to_lowercase().contains("picture")
                    || notice.contains("QR")
                    || notice.to_lowercase().contains("image"),
                "{notice}"
            );
            assert!(s.accounts().is_empty());
        }

        phase a_picture_fills_the_form_for_review_and_save_adds_the_account;
        {
            let s = Desk::new();
            let picture = s.picture(&qr_png(&format!(
                "otpauth://totp/Acme:bob?secret={VECTOR}&issuer=Acme"
            )));
            let read = s.call(
                "scan_picture",
                vec![Value::resource(&s.root), Value::file(picture)],
            );
            assert!(matches!(read, InvokeOutcome::Done(_)), "{read:?}");
            assert_eq!(
                said(&s),
                [("QR code read".to_owned(), "success".to_owned())],
                "the person is told the picture's code was read"
            );
            // What the picture said waits for the form: nothing is an account yet, and the key
            // it carried waits in the vault, in neither the record nor the window.
            assert!(s.accounts().is_empty(), "a picture read adds nothing by itself");
            assert!(s.text(&s.root, "pending").starts_with("vault.item:"));
            assert_eq!(s.text(&s.root, "pending_issuer"), "Acme");
            assert_eq!(s.text(&s.root, "pending_label"), "bob");
            assert_eq!(s.text(&s.root, "notice"), "", "a picture that read says nothing wrong");
            let made = s.call("add_pending", vec![Value::resource(&s.root)]);
            assert!(matches!(made, InvokeOutcome::Done(_)), "{made:?}");
            let accounts = s.accounts();
            assert_eq!(accounts.len(), 1, "saving the review adds its account");
            assert_eq!(s.text(&accounts[0], "issuer"), "Acme");
            assert_eq!(s.text(&accounts[0], "label"), "bob");
            let reference = s.text(&accounts[0], "item");
            assert_eq!(
                s.vault.kept().get(&reference).map(|kept| kept.secret.clone()),
                Some(VECTOR.to_owned()),
                "the key the picture read is the one the account runs on"
            );
            assert_eq!(s.text(&s.root, "pending"), "", "the review is cleared");
        }

        phase an_account_is_changed_and_its_key_kept_when_left_out;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            let reference = s.text(&a, "item");
            s.call(
                "update_account",
                vec![
                    Value::resource(&a),
                    Value::text("GitHub"),
                    Value::text("ada@work.example"),
                    Value::text(""),
                    Value::text("SHA512"),
                    Value::Int(8),
                    Value::Int(60),
                ],
            );
            assert_eq!(s.text(&a, "label"), "ada@work.example");
            let kept = s.vault.kept().get(&reference).cloned().expect("the item stays");
            assert_eq!(kept.secret, VECTOR, "the key left out is the one it held");
            assert_eq!(kept.algorithm, "SHA512");
            assert_eq!(kept.digits, 8);
            assert_eq!(kept.period, 60);
            // A new key replaces the one the vault held.
            s.call(
                "update_account",
                vec![
                    Value::resource(&a),
                    Value::text("GitHub"),
                    Value::text("ada@work.example"),
                    Value::text("JBSWY3DPEHPK3PXP"),
                    Value::text("SHA1"),
                    Value::Int(6),
                    Value::Int(30),
                ],
            );
            assert_eq!(
                s.vault.kept().get(&reference).map(|kept| kept.secret.clone()),
                Some("JBSWY3DPEHPK3PXP".to_owned())
            );
            let refused = s.rt.invoke_action(
                &me(),
                (
                    s.app.action("update_account").as_str(),
                    vec![
                        Value::resource(&a),
                        Value::text(""),
                        Value::text(""),
                        Value::text(""),
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
            let gone = s.text(&second, "item");
            s.call("delete_account", vec![Value::resource(&s.root), Value::resource(&second)]);
            let order: Vec<String> = s.accounts().iter().map(|a| s.text(a, "issuer")).collect();
            assert_eq!(order, ["Zed", "GitHub"]);
            assert!(
                !s.vault.kept().contains_key(&gone),
                "the key went with the account"
            );
        }

        phase a_code_is_copied_without_its_spaces;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "", VECTOR);
            s.call("copy_code", vec![Value::resource(&a)]);
            let held = s.selection.held().expect("the code was copied");
            assert_eq!(String::from_utf8(held.1).unwrap(), CODE);
        }

        phase the_window_shows_each_account_and_the_form_that_starts_one;
        {
            let s = Desk::new();
            // Nothing yet: the empty state asks for the first account.
            let window = s.window();
            let empty = find(window.tree(), "ui.EmptyState").expect("the empty state stands");
            assert_eq!(empty.props.get("title"), Some(&Value::text("No accounts yet")));
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            let window = s.window();
            let card = find(window.tree(), "ui.CardGridCard")
                .expect("the account's card stands in the window");
            assert_eq!(card.props.get("title"), Some(&Value::text("GitHub")));
            let countdown = find(window.tree(), "ui.Countdown").expect("its code's lifetime shows");
            assert!(countdown.props.contains_key("at"), "the instant it changes");
            let mut said = Vec::new();
            texts(window.tree(), &mut said);
            assert!(
                said.iter().any(|w| w == "••• •••"),
                "the card veils the code until it is asked for: {said:?}"
            );
            assert_eq!(s.text(&a, "issuer"), "GitHub");
        }

        phase an_account_opens_on_its_own_with_its_code_veiled;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            let mut window = s
                .rt
                .open_view(&me(), &s.app.view("account_window"), vec![Value::resource(&a)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            let mut said = Vec::new();
            texts(window.tree(), &mut said);
            assert!(
                said.iter().any(|w| w == "GitHub"),
                "the account's view names it: {said:?}"
            );
            assert!(
                said.iter().any(|w| w == "••• •••"),
                "its code is veiled at rest: {said:?}"
            );
            let countdown = find(window.tree(), "ui.Countdown").expect("its code's lifetime shows");
            assert_eq!(countdown.props.get("at"), Some(&Value::text(UNTIL)));
        }

        phase the_form_saves_what_was_typed_and_refuses_a_bad_key;
        {
            let s = Desk::new();
            let saved = s.call(
                "save_account",
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
            assert!(
                matches!(saved, InvokeOutcome::Done(Value::Bool(true))),
                "a save that landed answers that it did: {saved:?}"
            );
            assert_eq!(s.accounts().len(), 1);
            assert_eq!(s.text(&s.root, "notice"), "");
            // A key the vault cannot read answers that nothing was saved, and says why where the
            // window shows it: the form stays open on what was typed.
            let kept = s.call(
                "save_account",
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
            assert!(
                matches!(kept, InvokeOutcome::Done(Value::Bool(false))),
                "a refused save answers that it was not saved: {kept:?}"
            );
            assert!(
                s.text(&s.root, "notice").contains("base32"),
                "the notice says why: {}",
                s.text(&s.root, "notice")
            );
            assert_eq!(s.accounts().len(), 1, "the bad key added nothing");
        }
    }

    tilo_test_env::scenario! {
        fn the_window_a_person_works_in;

        phase a_refused_save_keeps_the_form_open_and_the_numbers_open_the_options;
        {
            let s = Desk::new();
            press(&s, "Add account");
            let window = s.window();
            assert_eq!(state_text(&window, "screen"), "form", "Add account opens the form");
            // A service and no key: the vault has nothing to keep, and the form stays.
            window
                .edit(&field(&window, "Service name"), &Value::text("GitHub"))
                .expect("the service is typed");
            press(&s, "Save");
            let window = s.window();
            assert_eq!(state_text(&window, "screen"), "form", "a refusal keeps the form");
            assert_eq!(
                s.text(&s.root, "notice"),
                "The account needs the key the service gave"
            );
            assert!(s.accounts().is_empty(), "nothing was kept");
            // A key the vault cannot read: the form stays too.
            window
                .edit(&field(&window, "Key from the service"), &Value::text("not base32!"))
                .expect("the key is typed");
            press(&s, "Save");
            let window = s.window();
            assert_eq!(state_text(&window, "screen"), "form");
            assert!(
                s.text(&s.root, "notice").contains("base32"),
                "{}",
                s.text(&s.root, "notice")
            );
            // A period no code keeps opens the options it stands under, and says why.
            window
                .edit(
                    &field(&window, "Key from the service"),
                    &Value::text(VECTOR),
                )
                .expect("a key the vault reads is typed");
            let window = s.window();
            window
                .edit(&period_field(&window), &Value::Int(1))
                .expect("a period no code keeps is typed");
            press(&s, "Save");
            let window = s.window();
            assert_eq!(state_text(&window, "screen"), "form", "a bad period keeps the form");
            let options = find(window.tree(), "ui.DisclosureRow").expect("the options stand");
            assert!(
                truthy(options.props.get("expanded")),
                "the options the bad period is under open: {:?}",
                options.props
            );
            assert!(
                s.text(&s.root, "notice").contains("5 to 300"),
                "{}",
                s.text(&s.root, "notice")
            );
            // Fixed, the same press saves and leaves for the list.
            let window = s.window();
            window
                .edit(&period_field(&window), &Value::Int(30))
                .expect("a period a code keeps is typed");
            press(&s, "Save");
            let window = s.window();
            assert_eq!(state_text(&window, "screen"), "list", "a saved account shows in the list");
            assert_eq!(s.accounts().len(), 1);
            assert_eq!(s.text(&s.root, "notice"), "");
        }

        phase a_pasted_setup_link_fills_the_form_before_save;
        {
            let s = Desk::new();
            press(&s, "Add account");
            let window = s.window();
            window
                .edit(
                    &field(&window, "Setup link"),
                    &Value::text("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&issuer=Acme&algorithm=SHA256&digits=8&period=45"),
                )
                .expect("the link is pasted");
            press(&s, "Fill from the link");
            let window = s.window();
            assert_eq!(state_text(&window, "issuer"), "Acme", "the issuer is filled");
            assert_eq!(state_text(&window, "label"), "bob");
            assert_eq!(state_text(&window, "secret"), "JBSWY3DPEHPK3PXP");
            assert_eq!(state_text(&window, "algorithm"), "SHA256");
            assert!(s.accounts().is_empty(), "the link filled the form and added nothing");
            // The person reads what the link said, then saves it.
            press(&s, "Save");
            assert_eq!(s.accounts().len(), 1, "the reviewed account is saved");
            let kept = s.vault.kept().into_values().next().expect("the vault kept it");
            assert_eq!(kept.algorithm, "SHA256");
            assert_eq!(kept.digits, 8);
            assert_eq!(kept.period, 45);
        }

        phase a_code_is_veiled_at_rest_and_a_press_shows_it_then_copies;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            let window = s.window();
            let mut shown_words = shown(&window);
            assert!(
                shown_words.iter().any(|word| word == "••• •••"),
                "the code is veiled at rest: {shown_words:?}"
            );
            assert!(!shown_words.iter().any(|word| word == "123 456"), "no digits read");
            press(&s, "Reveal");
            let window = s.window();
            shown_words = shown(&window);
            assert!(
                shown_words.iter().any(|word| word == "123 456"),
                "the press shows the code: {shown_words:?}"
            );
            press(&s, "Copy");
            assert_eq!(
                s.selection.held().map(|(_, bytes)| String::from_utf8(bytes).unwrap()),
                Some(CODE.to_owned()),
                "the same control copies the code it showed"
            );
            assert_eq!(
                said(&s),
                [("Copied GitHub code".to_owned(), "success".to_owned())],
                "the person is told it was copied"
            );
            // Ten seconds later the code is veiled again, with nothing kept of it.
            let window = s.window();
            let until = match window.view_state("shown_until").as_ref().map(unslotted) {
                Some(Value::Int(until)) => *until,
                other => panic!("the reveal's second reads {other:?}"),
            };
            window.set_view_state("second", Value::Int(until));
            let window = s.window();
            assert!(
                shown(&window).iter().any(|word| word == "••• •••"),
                "the code is veiled once its ten seconds are up"
            );
            // A clipboard that does not take it says so, rather than leaving it looking copied.
            s.selection.refuse(true);
            press(&s, "Reveal");
            press(&s, "Copy");
            assert_eq!(
                said(&s),
                [("Could not copy the code".to_owned(), "error".to_owned())],
                "a copy that failed is shown"
            );
            assert_eq!(
                s.selection.held().map(|(_, bytes)| String::from_utf8(bytes).unwrap()),
                Some(CODE.to_owned()),
                "and the code that was there before is not taken for this press"
            );
            let _ = a;
        }

        phase an_account_opens_in_its_own_window;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            assert_eq!(
                s.rt.default_view(&a.ty),
                Some(s.app.view("account_window")),
                "a hit on an account opens the account, not the list"
            );
            let window = s
                .rt
                .open_view(
                    &me(),
                    &s.app.view("account_window"),
                    vec![Value::resource(&a)],
                )
                .expect("the account's window opens");
            let mut said_words = Vec::new();
            texts(window.tree(), &mut said_words);
            assert!(said_words.iter().any(|word| word == "GitHub"), "{said_words:?}");
            assert!(
                said_words.iter().any(|word| word == "••• •••"),
                "its code is veiled as the list's is"
            );
            let key = keyed(&window, "ui.Button", "Reveal");
            window.press(&key).expect("the press runs");
            let window = s
                .rt
                .open_view(
                    &me(),
                    &s.app.view("account_window"),
                    vec![Value::resource(&a)],
                )
                .expect("the account's window opens");
            let mut said_words = Vec::new();
            texts(window.tree(), &mut said_words);
            assert!(
                said_words.iter().any(|word| word == "123 456"),
                "the press shows its code: {said_words:?}"
            );
        }

        phase a_card_wears_the_accounts_own_colour;
        {
            let s = Desk::new();
            let a = s.add_coloured("GitHub", "ada@example.com", VECTOR, 2);
            let window = s.window();
            let avatar = find(window.tree(), "ui.Avatar").expect("the card has an avatar");
            assert_eq!(
                avatar.props.get("colour"),
                Some(&Value::text("#b0568f")),
                "the avatar wears the account's third colour"
            );
            assert_eq!(avatar.props.get("name"), Some(&Value::text("GitHub")));
            let _ = a;
        }

        phase a_clock_that_is_not_synchronised_is_said;
        {
            let s = Desk::new();
            s.add("GitHub", "", VECTOR);
            s.clock(Some(false));
            let warning = "This device's clock is not synchronised, so a service may reject these codes. Turn on automatic time, then try again.";
            let window = s.window();
            assert!(
                shown(&window).iter().any(|word| word == warning),
                "a clock out of time is said in the window"
            );
            s.clock(Some(true));
            let window = s.window();
            assert!(
                !shown(&window).iter().any(|word| word == warning),
                "a synchronised clock says nothing"
            );
            s.clock(None);
            let window = s.window();
            assert!(
                !shown(&window).iter().any(|word| word == warning),
                "a device that cannot say says nothing: a warning nobody can act on is noise"
            );
        }

        phase the_grid_answers_the_edit_and_delete_keys_of_the_card_it_selects;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "ada@example.com", VECTOR);
            let window = s.window();
            let edited = window.fire_with(&grid_key(&s), "edit", Some(&Value::resource(&a)));
            assert!(matches!(edited, Ok(InvokeOutcome::Done(_))), "{edited:?}");
            let window = s.window();
            assert_eq!(state_text(&window, "screen"), "form", "the edit key opens the form");
            assert_eq!(state_text(&window, "issuer"), "GitHub", "for the account it was on");
            press(&s, "Back");
            let window = s.window();
            // Deleting an account reaches the vault that holds its key, which no undo takes
            // back: the press asks the person first, and acts on their yes.
            let asked = window.fire_with(&grid_key(&s), "delete", Some(&Value::resource(&a)));
            assert!(matches!(asked, Ok(InvokeOutcome::Asked(_))), "{asked:?}");
            assert!(!s.accounts().is_empty(), "nothing is deleted before the person's yes");
            let deleted = s
                .rt
                .invoke_approved_through(
                    &me(),
                    None,
                    (
                        s.app.action("delete_account").as_str(),
                        vec![Value::resource(&s.root), Value::resource(&a)],
                    ),
                    None,
                )
                .unwrap();
            assert!(matches!(deleted, InvokeOutcome::Done(_)), "{deleted:?}");
            assert!(s.accounts().is_empty(), "the delete deletes the account it was on");
            assert!(s.vault.kept().is_empty(), "and the key the vault kept for it");
        }

        phase the_app_is_found_by_the_other_names_people_know_it_by;
        {
            let s = Desk::new();
            let keywords: Vec<String> = s.app.meta().keywords.clone();
            for alias in ["mfa", "one-time password", "login code", "authenticator app"] {
                assert!(
                    keywords.iter().any(|word| word == alias),
                    "search finds it by {alias}: {keywords:?}"
                );
            }
        }

        phase the_window_names_the_accounts_it_shows;
        {
            let s = Desk::new();
            let title = |s: &Desk| {
                let window = s.window();
                let shell = find(window.tree(), "ui.NavShell").expect("the window's shell stands");
                shell.props.get("title").cloned().unwrap_or(Value::Bool(false))
            };
            assert_eq!(title(&s), Value::text("Authenticator"), "nothing to name yet");
            s.add("GitHub", "", VECTOR);
            assert_eq!(title(&s), Value::text("GitHub"), "one account names the window");
            s.add("Acme", "", VECTOR);
            assert_eq!(
                title(&s),
                Value::text("GitHub and 1 more"),
                "several accounts still name the window"
            );
        }

        phase the_cards_controls_name_the_account_they_act_on;
        {
            let s = Desk::new();
            s.add("GitHub", "", VECTOR);
            let window = s.window();
            let reveal = of_kind(window.tree(), "ui.Button")
                .into_iter()
                .find(|node| node.props.get("label") == Some(&Value::text("Reveal")))
                .expect("the card offers Reveal");
            assert_eq!(
                reveal.props.get("accessible_name"),
                Some(&Value::text("Reveal code for GitHub")),
                "the control says which account it shows a code for"
            );
            press(&s, "Reveal");
            let window = s.window();
            let copy = of_kind(window.tree(), "ui.Button")
                .into_iter()
                .find(|node| node.props.get("label") == Some(&Value::text("Copy")))
                .expect("the card offers Copy once the code shows");
            assert_eq!(
                copy.props.get("accessible_name"),
                Some(&Value::text("Copy code for GitHub")),
                "the copy names the account too"
            );
        }

        phase the_form_offers_a_colour_and_a_colour_only_change_says_so;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "", VECTOR);
            press(&s, "Add account");
            let window = s.window();
            let picker =
                find(window.tree(), "ui.ColorSwatchPicker").expect("the form offers colours");
            let swatches = match picker.props.get("swatches") {
                Some(Value::List(swatches)) => swatches.len(),
                other => panic!("the palette reads {other:?}"),
            };
            assert_eq!(swatches, 8, "the palette is the eight slots the cards paint");
            press(&s, "Back");
            // A person recolours the account: only the colour changes, and history says that.
            let history = s.rt.history(&s.root.doc).expect("the root's history reads");
            let before = history.len();
            let saved = s.call(
                "save_account",
                vec![
                    Value::resource(&s.root),
                    Value::Opt(Some(Box::new(Value::resource(&a)))),
                    Value::text("GitHub"),
                    Value::text(""),
                    Value::text(""),
                    Value::text("SHA1"),
                    Value::Int(6),
                    Value::Int(30),
                    Value::Int(2),
                ],
            );
            assert!(
                matches!(saved, InvokeOutcome::Done(Value::Bool(true))),
                "{saved:?}"
            );
            let window = s.window();
            let avatar = find(window.tree(), "ui.Avatar").expect("the card wears it");
            assert_eq!(avatar.props.get("colour"), Some(&Value::text("#b0568f")));
            let history = s.rt.history(&s.root.doc).expect("the root's history reads");
            let since: Vec<String> = history
                .iter()
                .skip(before)
                .map(|note| note.action.clone())
                .collect();
            assert!(
                since.contains(&s.app.action("recolor_account").as_str().to_owned()),
                "history names the recolour: {since:?}"
            );
            for other in ["rename_account", "relabel_account", "recode_account"] {
                assert!(
                    !since.contains(&s.app.action(other).as_str().to_owned()),
                    "only the colour changed: {since:?}"
                );
            }
        }

        phase each_field_change_is_the_action_that_names_it;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "", VECTOR);
            // Each save changes one part of the account, and the change history names is the
            // action for that part: a person reads what changed, not that something did.
            let edit = |issuer: &str, label: &str, digits: i64| -> Vec<String> {
                let history = s.rt.history(&s.root.doc).expect("the root's history reads");
                let before = history.len();
                let saved = s.call(
                    "save_account",
                    vec![
                        Value::resource(&s.root),
                        Value::Opt(Some(Box::new(Value::resource(&a)))),
                        Value::text(issuer),
                        Value::text(label),
                        Value::text(""),
                        Value::text("SHA1"),
                        Value::Int(digits),
                        Value::Int(30),
                        Value::Int(0),
                    ],
                );
                assert!(matches!(saved, InvokeOutcome::Done(Value::Bool(true))), "{saved:?}");
                let history = s.rt.history(&s.root.doc).expect("the root's history reads");
                history
                    .iter()
                    .skip(before)
                    .map(|note| note.action.clone())
                    .collect()
            };
            let renamed = edit("GitHub Corp", "", 6);
            assert!(renamed.contains(&s.app.action("rename_account").as_str().to_owned()), "{renamed:?}");
            assert!(
                !renamed.contains(&s.app.action("relabel_account").as_str().to_owned()),
                "{renamed:?}"
            );
            assert_eq!(s.text(&a, "issuer"), "GitHub Corp");
            let relabelled = edit("GitHub Corp", "ada@example.com", 6);
            assert!(
                relabelled.contains(&s.app.action("relabel_account").as_str().to_owned()),
                "{relabelled:?}"
            );
            assert!(
                !relabelled.contains(&s.app.action("rename_account").as_str().to_owned()),
                "{relabelled:?}"
            );
            assert_eq!(s.text(&a, "label"), "ada@example.com");
            let recoded = edit("GitHub Corp", "ada@example.com", 8);
            assert!(
                recoded.contains(&s.app.action("recode_account").as_str().to_owned()),
                "{recoded:?}"
            );
            let kept = s.vault.kept().into_values().next().expect("the vault kept it");
            assert_eq!(kept.digits, 8);
        }

        phase every_window_that_shows_a_code_is_left_out_of_captures;
        {
            let s = Desk::new();
            let a = s.add("GitHub", "", VECTOR);
            assert!(
                s.window().capture_protected(),
                "the list's codes are left out of a capture"
            );
            let window = s
                .rt
                .open_view(
                    &me(),
                    &s.app.view("account_window"),
                    vec![Value::resource(&a)],
                )
                .expect("the account's window opens");
            assert!(
                window.capture_protected(),
                "the account's code is left out of a capture too"
            );
        }
    }
}
