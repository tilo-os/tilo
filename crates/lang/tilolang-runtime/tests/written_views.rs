//! Written views (`tilolang::written`): an agent answers with a real `view` over Mail, an app no
//! package names for it; the person keeps it among their own views; they send it to someone
//! else, who adds it to theirs and sees it over their own mail; and it is how a type shows when
//! the person makes it so. Each person is a runtime over their own store, as each keeps Views and
//! Mail in their own record; only the chat they share syncs between them.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang::IdsLock;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, Principal, ResourceRef, Runtime, UiNode, Value,
    };

    macro_rules! app_source {
        ($name:literal) => {
            (
                concat!("apps/tl/", $name),
                include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
                include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
            )
        };
    }

    /// Compile a package with its committed lock, which must be complete.
    fn build((dir, src, ids): (&str, &str, &str), imports: &[&App]) -> App {
        let lock = IdsLock::parse(ids).unwrap_or_else(|e| panic!("{dir}/app.ids: {e}"));
        let metas: Vec<tilolang::PackageMeta> = imports.iter().map(|a| a.meta().clone()).collect();
        let compiled = tilolang::compile_package(src, &lock, &metas)
            .unwrap_or_else(|d| panic!("{dir}: {d:#?}"));
        assert_eq!(
            compiled.ids.to_text(),
            ids,
            "{dir}/app.ids is out of date: run `tilolang build {dir}`"
        );
        App { compiled }
    }

    struct Apps {
        mail: App,
        chat: App,
        views: App,
    }

    fn apps() -> Apps {
        let mail = build(app_source!("mail"), &[]);
        let chat = build(app_source!("chat"), &[]);
        let views = build(app_source!("views"), &[&chat]);
        Apps { mail, chat, views }
    }

    /// One person: their runtime over their own store, every app installed.
    struct Person {
        who: Principal,
        rt: Runtime,
        store: AmStore,
        auth: TestAuthority,
        views: ResourceRef,
    }

    impl Person {
        fn new(apps: &Apps, who: &str) -> Self {
            let store = AmStore::new();
            let auth = TestAuthority::new();
            let rt = Runtime::new(store.clone(), auth.clone()).unwrap();
            // Installed packages are code the platform trusts, as it does every installed app.
            for app in [&apps.mail, &apps.chat, &apps.views] {
                auth.trust(&app.load(&rt));
            }
            let views = rt.create_root("Views").unwrap();
            Self {
                who: Principal::new(who),
                rt,
                store,
                auth,
                views,
            }
        }

        fn call(&self, app: &App, action: &str, args: Vec<Value>) -> InvokeOutcome {
            self.call_as(&self.who, app, action, args)
        }

        fn call_as(
            &self,
            who: &Principal,
            app: &App,
            action: &str,
            args: Vec<Value>,
        ) -> InvokeOutcome {
            let id = app.action(action);
            self.rt
                .invoke_action(who, (id.as_str(), args))
                .unwrap_or_else(|e| panic!("{action}: {e}"))
        }

        /// Their inbox: Mail's busy-inbox scenario, run as them.
        fn inbox(&self, apps: &Apps) -> ResourceRef {
            let mail = self.rt.create_root("Mail").unwrap();
            let scenario = apps
                .mail
                .meta()
                .scenarios
                .iter()
                .find(|s| s.name == "a busy inbox")
                .unwrap()
                .action
                .clone();
            self.rt
                .invoke_action(&self.who, (scenario.as_str(), Vec::new()))
                .unwrap();
            mail
        }

        fn views(&self) -> ResourceRef {
            self.views.clone()
        }

        fn text(&self, at: &ResourceRef, field: &str) -> String {
            self.rt
                .read(at, field)
                .unwrap()
                .as_text()
                .unwrap()
                .to_owned()
        }

        fn items(&self, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            let Value::List(items) = self.rt.read(at, field).unwrap() else {
                panic!("{field} is not a list");
            };
            items
                .into_iter()
                .map(|v| match v {
                    Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                    other => panic!("not a reference: {other:?}"),
                })
                .collect()
        }

        fn author(&self, view: &ResourceRef) -> Principal {
            match self.rt.read(view, "author").unwrap() {
                Value::Principal(p) => p,
                other => panic!("the author is a principal, not {other:?}"),
            }
        }

        /// What the view `v` of theirs shows over `subject`, drawn as the system draws it: as
        /// them, its author the one the host recorded.
        fn shown(&self, v: &ResourceRef, subject: &ResourceRef) -> Vec<String> {
            let view = self
                .rt
                .open_written_view(
                    &self.who,
                    &self.text(v, "definition"),
                    &self.author(v),
                    vec![Value::resource(subject)],
                )
                .unwrap_or_else(|e| panic!("{e}"));
            let mut view = view;
            view.set_lazy_rows(false).unwrap();
            titles(view.tree())
        }
    }

    fn titles(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.ListRow")
            .into_iter()
            .map(|row| text_prop(row, "title"))
            .collect()
    }

    /// The view an agent answers "what's unread in my mail?" with: a view over Mail, which
    /// Views names nowhere.
    const UNREAD: &str = "/// The conversations still unread, newest first.\n\
view unread_mail(m: mail.Mail) {\n\
    for a in m.accounts {\n\
        list(a.threads.filter(t -> t.unread)) { t ->\n\
            row(t)\n\
        }\n\
    }\n\
}";

    const LAUNCH: &str = "Launch plan for the autumn release";
    const DINNER: &str = "Dinner on Friday?";
    const INVOICE: &str = "Invoice for June";

    /// The agent's answer, as its task keeps it in the person's views: checked first, the type
    /// it shows found by the check.
    fn answer(ada: &Person, apps: &Apps, task: &Principal) -> ResourceRef {
        let info = ada
            .rt
            .check_written_view(UNREAD, task)
            .unwrap_or_else(|d| panic!("{d:#?}"));
        assert_eq!(info.name, "unread_mail");
        assert_eq!(info.shows.as_deref(), Some("so.tilo.lang.mail.Mail"));
        returned_ref(ada.call_as(
            task,
            &apps.views,
            "answer",
            vec![
                Value::resource(&ada.views()),
                Value::text("Unread mail"),
                Value::text("What's unread in my mail?"),
                Value::text(UNREAD),
                Value::text(info.shows.unwrap()),
            ],
        ))
    }

    #[test]
    fn an_agent_answers_with_a_view_over_mail_and_the_person_keeps_it() {
        let apps = apps();
        let ada = Person::new(&apps, "person:ada");
        let mail = ada.inbox(&apps);
        let task = Principal::new("agent-task:unread");
        let v = answer(&ada, &apps, &task);
        // The host recorded the task as its author; nobody can claim another.
        assert_eq!(ada.author(&v), task);
        assert_eq!(ada.rt.read(&v, "kept").unwrap(), Value::Bool(false));
        // It is a real view over Mail: her unread conversations, live, and nothing she read.
        let shown = ada.shown(&v, &mail);
        assert!(
            shown.contains(&LAUNCH.to_owned()) && shown.contains(&DINNER.to_owned()),
            "{shown:?}"
        );
        assert!(!shown.contains(&INVOICE.to_owned()), "{shown:?}");
        // She keeps it: it is hers now, in her own record.
        ada.call(&apps.views, "keep", vec![Value::resource(&v)]);
        assert_eq!(ada.rt.read(&v, "kept").unwrap(), Value::Bool(true));
        // And makes it how her Mail shows on the desktop.
        ada.call(
            &apps.views,
            "make_default",
            vec![Value::resource(&ada.views()), Value::resource(&v)],
        );
        let defaults = ada.items(&ada.views(), "defaults");
        assert_eq!(defaults.len(), 1);
        assert_eq!(ada.text(&defaults[0], "shows"), "so.tilo.lang.mail.Mail");
        // A view that does not compile says why, located in what was written.
        let wrong = ada
            .rt
            .check_written_view("view nope(m: mail.Mailz) {\n    text(\"x\")\n}", &ada.who)
            .unwrap_err();
        assert!(
            wrong
                .iter()
                .any(|d| d.line == 1 && d.message.contains("Mailz")),
            "{wrong:#?}"
        );
        // Over an app the workspace does not have, the check refuses it.
        assert!(
            ada.rt
                .check_written_view("view t(x: board.Card) {\n    text(x.title)\n}", &ada.who)
                .is_err()
        );
    }

    #[test]
    fn a_sent_view_shows_its_recipient_their_own_mail_and_asks_before_acting() {
        let apps = apps();
        let mut ada = Person::new(&apps, "person:ada");
        let mut bo = Person::new(&apps, "person:bo");
        let ada_mail = ada.inbox(&apps);
        let bo_mail = bo.inbox(&apps);
        // Bo's mail is his own: he read the launch plan.
        let launch = bo
            .items(&bo.items(&bo_mail, "accounts")[0], "threads")
            .into_iter()
            .find(|t| bo.text(t, "subject") == LAUNCH)
            .unwrap();
        bo.call(
            &apps.mail,
            "mark_read",
            vec![Value::resource(&launch), Value::Bool(false)],
        );
        let v = answer(&ada, &apps, &Principal::new("agent-task:unread"));
        ada.call(&apps.views, "keep", vec![Value::resource(&v)]);

        // Ada sends it to the team's channel. Only the chat travels between them.
        let before: Vec<_> = ada.store.doc_ids();
        let channels = ada.rt.create_root("Channels").unwrap();
        let general = returned_ref(ada.call(
            &apps.chat,
            "new_channel",
            vec![Value::resource(&channels), Value::text("general")],
        ));
        let send_flow = apps.views.flow("send");
        let sent = ada
            .rt
            .run_flow(
                &ada.who,
                &send_flow,
                vec![Value::resource(&v), Value::resource(&general)],
            )
            .unwrap();
        assert!(
            matches!(sent, tilolang_runtime::FlowOutcome::Done(_)),
            "{sent:?}"
        );
        let shared: Vec<_> = ada
            .store
            .doc_ids()
            .into_iter()
            .filter(|d| !before.contains(d))
            .collect();
        ada.store.sync_docs_with(&mut bo.store, &shared).unwrap();
        let message = bo
            .items(&general, "messages")
            .pop()
            .expect("the view arrived");

        // Bo adds it to his own views: its author is who sent it, as the log recorded her.
        let bos = returned_ref(bo.call(
            &apps.views,
            "add",
            vec![Value::resource(&bo.views()), Value::resource(&message)],
        ));
        assert_eq!(bo.author(&bos), ada.who);
        assert_eq!(bo.text(&bos, "definition"), UNREAD);
        assert_eq!(bo.text(&bos, "title"), "Unread mail");
        // Neither person's views reached the other: each keeps theirs.
        assert_eq!(ada.items(&ada.views(), "items").len(), 1);
        assert_eq!(bo.items(&bo.views(), "items").len(), 1);

        // The same view shows each person their own mail: Bo read the launch plan, Ada did not.
        let hers = ada.shown(&v, &ada_mail);
        let his = bo.shown(&bos, &bo_mail);
        assert!(hers.contains(&LAUNCH.to_owned()), "{hers:?}");
        assert!(!his.contains(&LAUNCH.to_owned()), "{his:?}");
        assert!(his.contains(&DINNER.to_owned()), "{his:?}");

        // What it does with an action asks Bo first, as Ada wrote it; once he trusts her, not.
        asks_before_acting(&bo, &ada.who, &bo_mail);
    }

    /// What a view `writer` wrote does with an action asks `p` first; once they trust the
    /// writer, it does not.
    fn asks_before_acting(p: &Person, writer: &Principal, mail: &ResourceRef) {
        let acting = "view mark_all(m: mail.Mail) {\n\
    for a in m.accounts {\n\
        for t in a.threads.filter(t -> t.unread) {\n\
            button(\"Read\", on: mail.mark_read(t, false))\n\
        }\n\
    }\n\
}";
        let unread = || -> usize {
            p.items(&p.items(mail, "accounts")[0], "threads")
                .iter()
                .filter(|t| p.rt.read(t, "unread").unwrap() == Value::Bool(true))
                .count()
        };
        let unread_before = unread();
        let view =
            p.rt.open_written_view(&p.who, acting, writer, vec![Value::resource(mail)])
                .unwrap();
        let button = of_kind(view.tree(), "ui.Button")[0].key.clone();
        match view.press(&button).unwrap() {
            InvokeOutcome::Suggested(s) => assert_eq!(
                s.chain,
                vec![tilolang_runtime::ModuleHash::principal(&writer.0)],
                "the chain names who wrote the view"
            ),
            other => panic!("an untrusted author's view asks first, not {other:?}"),
        }
        assert_eq!(unread(), unread_before, "asking changed nothing");
        p.auth.trust_principal(&writer.0);
        let view =
            p.rt.open_written_view(&p.who, acting, writer, vec![Value::resource(mail)])
                .unwrap();
        let button = of_kind(view.tree(), "ui.Button")[0].key.clone();
        assert!(matches!(
            view.press(&button).unwrap(),
            InvokeOutcome::Done(_)
        ));
        assert_eq!(unread().checked_add(1), Some(unread_before));
    }
}
