//! Mail's files (`apps/tl/mail/app.tl`): a message taken out of Mail as a file a mail program or
//! a script reads, and a message file opened in Mail — the `.eml` provider.

#[cfg(test)]
mod cases {

    use crate::common::*;
    use tilolang_runtime::blob::FileValue;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value};

    /// Mail at a workspace with a device's files, at a moment the person's clock says.
    struct Mail {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Files,
    }

    impl Mail {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Files::default();
            rt.set_blobs(device.clone());
            rt.set_clock(|| "2026-06-30T09:00:00Z".to_owned());
            let mut packages = built("mail");
            let app = packages.pop().expect("Mail is built last");
            for used in &packages {
                used.load(&rt);
            }
            authority.trust(&app.load(&rt));
            // Taking a message in or opening a file leaves the system for the record an outside
            // service keeps: the person's press of it confirms, and the test answers that here.
            for action in ["deliver_message", "open_message_file"] {
                authority.grant_outside(&app.action(action));
            }
            let root = rt.create_root("Mail").unwrap();
            Self {
                rt,
                app,
                root,
                device,
            }
        }

        fn call(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            let args = self.app.args(&self.rt, action, given);
            self.rt
                .invoke_action(&me(), (self.app.action(action).as_str(), args))
                .unwrap_or_else(|e| panic!("{action}: {e}"))
        }

        /// A message delivered into the workspace from outside its accounts: the message it is
        /// kept as.
        fn delivered(&self) -> ResourceRef {
            let root = Value::resource(&self.root);
            let out = self.call(
                "deliver_message",
                &[
                    ("m", root),
                    ("from", Value::text("Ada Lovelace <ada@analytical.example>")),
                    ("to", Value::text("sam@rivera.example")),
                    ("subject", Value::text("Engine notes")),
                    ("body", Value::text("The **notes** are here.\n\nAda")),
                    ("message_id", Value::text("notes@analytical.example")),
                    ("conversation", Value::text("notes@analytical.example")),
                    ("date", Value::text("2024-03-01T09:00:00Z")),
                    (
                        "attachments",
                        Value::List(vec![Value::file(FileValue::new(
                            "notes.pdf",
                            "application/pdf",
                            3,
                            "def",
                        ))]),
                    ),
                ],
            );
            returned_ref(out)
        }

        fn said_of(&self, at: &ResourceRef, field: &str) -> String {
            match self.rt.read(at, field).expect("the field reads") {
                Value::Text(said) => said,
                other => panic!("{field} reads {other:?}"),
            }
        }
    }

    /// The file `out` answers with.
    fn file_of(out: InvokeOutcome) -> FileValue {
        match out {
            InvokeOutcome::Done(Value::File(file)) => (*file).clone(),
            other => panic!("the action answers a file: {other:?}"),
        }
    }

    /// A message leaves Mail as Markdown, as JSON and as an `.eml` a mail program opens: its
    /// sender, when it came and what it says, and the files it carries by name.
    #[test]
    fn a_message_is_exported_as_markdown_json_and_an_eml() {
        let mail = Mail::new();
        let message = mail.delivered();

        let markdown = file_of(mail.call("export_markdown", &[("m", Value::resource(&message))]));
        assert_eq!(markdown.name, "Engine notes.md");
        let said = mail.device.said("Engine notes.md");
        assert!(said.contains("# Engine notes"), "{said}");
        assert!(
            said.contains("Ada Lovelace <ada@analytical.example>"),
            "{said}"
        );
        assert!(said.contains("The **notes** are here."), "{said}");
        assert!(said.contains("Attached: notes.pdf"), "{said}");

        let json_file = file_of(mail.call("export_json", &[("m", Value::resource(&message))]));
        assert_eq!(json_file.name, "Engine notes.json");
        let said = mail.device.said("Engine notes.json");
        assert!(said.contains("\"subject\": \"Engine notes\""), "{said}");
        assert!(said.contains("\"attachments\": [\"notes.pdf\"]"), "{said}");
        assert!(said.contains("\"unread\": true"), "{said}");

        let eml = file_of(mail.call("export_eml", &[("m", Value::resource(&message))]));
        assert_eq!(eml.name, "Engine notes.eml");
        let said = mail.device.said("Engine notes.eml");
        assert!(
            said.contains("From: Ada Lovelace <ada@analytical.example>"),
            "{said}"
        );
        assert!(said.contains("Subject: Engine notes"), "{said}");
        assert!(
            said.contains("Message-ID: <notes@analytical.example>"),
            "{said}"
        );
        assert!(said.contains("\n\nThe **notes** are here."), "{said}");
    }

    /// A message kept as a file opens in Mail: its headers are read into the message it is kept
    /// as, in the conversation its Message-ID names, and the same file opened again is that
    /// message, not another.
    #[test]
    fn a_message_file_opens_in_mail() {
        let mail = Mail::new();
        let text = "From: Grace Hopper <grace@hopper.example>\r\n\
                    To: sam@rivera.example\r\n\
                    Cc: ada@analytical.example\r\n\
                    Subject: Kept plan\r\n\
                    Message-ID: <plan@hopper.example>\r\n\
                    Date: Tue, 30 Jun 2026 09:00:00 +0000\r\n\
                    \r\n\
                    Here is the plan.\r\n";
        let file = mail
            .device
            .keeping("plan.eml", "message/rfc822", text.as_bytes());
        let out = mail.call(
            "open_message_file",
            &[
                ("m", Value::resource(&mail.root)),
                ("file", Value::file(file)),
            ],
        );
        let message = returned_ref(out);
        assert_eq!(mail.said_of(&message, "subject"), "Kept plan");
        assert_eq!(mail.said_of(&message, "from"), "grace@hopper.example");
        assert_eq!(mail.said_of(&message, "to"), "sam@rivera.example");
        assert_eq!(mail.said_of(&message, "cc"), "ada@analytical.example");
        assert_eq!(mail.said_of(&message, "message_id"), "plan@hopper.example");
        assert!(
            mail.said_of(&message, "body").contains("Here is the plan."),
            "{}",
            mail.said_of(&message, "body")
        );
        assert!(
            !matches!(mail.rt.read(&message, "unread"), Ok(Value::Bool(true))),
            "a message opened from a file reads as read"
        );
        // It is kept with the mail held outside the accounts, in the conversation it names.
        let thread = mail
            .rt
            .read(&mail.root, "recovered_mail")
            .expect("the kept mail");
        let Value::List(threads) = thread else {
            panic!("the kept mail is a list: {thread:?}");
        };
        assert_eq!(threads.len(), 1, "one conversation: {threads:?}");

        // Opened again: the message already kept.
        let again = mail
            .device
            .keeping("plan.eml", "message/rfc822", text.as_bytes());
        let out = mail.call(
            "open_message_file",
            &[
                ("m", Value::resource(&mail.root)),
                ("file", Value::file(again)),
            ],
        );
        assert_eq!(returned_ref(out).id_string(), message.id_string());
    }
}
