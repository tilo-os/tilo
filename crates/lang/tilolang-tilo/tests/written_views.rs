//! Written views (`tilolang::written`): an agent answers with a real `view` over Mail, an app no
//! package names for it; the person keeps it among their own views; they send it to someone
//! else, who adds it to theirs and sees it over their own mail; and it is how a type shows when
//! the person makes it so.
//!
//! Each person is a host over their own workspace. Mail's account and its mail are what the mail
//! service holds, which no action of the app writes: they come in through the real mail origin,
//! from the sample server, before Mail's scenario opens the launch conversation, as a seeded
//! workspace's do. Each keeps their views and their mail; only the conversation they share
//! travels between them.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, InvokeOutcome, ModuleHash, ResourceRef, UiNode, Value};

use crate::fixtures::{Replica, Workspace, action_id, target};

/// Mail's package (`apps/tl/mail`): its app and the scenarios staged beside it.
const MAIL: (&str, &str) = (
    include_str!("../../../../apps/tl/mail/app.tl"),
    include_str!("../../../../apps/tl/mail/app.ids"),
);
const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);
const VIEWS: (&str, &str) = (
    include_str!("../../../../apps/tl/views/app.tl"),
    include_str!("../../../../apps/tl/views/app.ids"),
);

const MAIL_APP: &str = "so.tilo.mail";
const CHAT_APP: &str = "so.tilo.chat";
const VIEWS_APP: &str = "so.tilo.lang.views";

/// The moment the sample account's mail is taken in at: the end of the working week it holds.
const NOW: &str = "2026-06-30T10:41:00Z";

fn person(id: &str) -> Actor {
    tilolang_runtime::ids::MemberId::person(id).into()
}

fn agent(id: &str) -> Actor {
    tilolang_runtime::ids::MemberId::agent(id).into()
}

/// What a host sends its calls to: every call is kept for the test to read, and a call that
/// needs a person's yes waits as `call` until it is answered, as the fixture host's does.
#[derive(Clone, Default)]
struct Recording {
    calls: Rc<RefCell<Vec<Invocation>>>,
}

impl Recording {
    fn new() -> Self {
        Self::default()
    }

    /// The call a run waits on: the last one sent.
    fn waiting(&self) -> Invocation {
        self.calls
            .borrow()
            .last()
            .cloned()
            .expect("a call was sent")
    }
}

impl Invoker for Recording {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.calls.borrow_mut().push(invocation.clone());
        match invocation {
            Invocation::Action { asks: true, .. } => Invoked::Pending("call".to_owned()),
            _ => Invoked::Refused("this test host runs no call unasked".to_owned()),
        }
    }
}

/// One person: a host over their own workspace, every app installed.
struct Person {
    who: Actor,
    replica: Replica,
    dispatch: Recording,
    views: ResourceRef,
}

impl Person {
    fn new(who: &str) -> Self {
        let (replica, _) = Replica::new(who, Workspace::new(), &[MAIL, CHAT, VIEWS]);
        let dispatch = Recording::new();
        replica.host.runtime().set_foreign_invoker(dispatch.clone());
        let views = replica.host.root_of(VIEWS_APP).unwrap();
        Self {
            who: person(who),
            replica,
            dispatch,
            views,
        }
    }

    fn call(&self, app: &str, action: &str, args: Vec<Value>) -> InvokeOutcome {
        self.call_as(&self.who, app, action, args)
    }

    fn call_as(&self, who: &Actor, app: &str, action: &str, args: Vec<Value>) -> InvokeOutcome {
        let meta = self.replica.host.package(app).unwrap().meta;
        let id = action_id(&meta, action);
        self.replica
            .host
            .runtime()
            .invoke_action(who, (id.as_str(), args))
            .unwrap_or_else(|e| panic!("{action}: {e}"))
    }

    /// Their inbox: the sample account and its mail taken in through Mail's origin, as a real
    /// server's answer comes in, then Mail's busy-inbox scenario run as them.
    fn inbox(&self) -> ResourceRef {
        let mail = self.replica.host.root_of(MAIL_APP).unwrap();
        tilolang_mail::sample::take_in(&self.replica.host, MAIL_APP, NOW).unwrap();
        self.replica.commit();
        let meta = self.replica.host.package(MAIL_APP).unwrap().meta;
        let scenario = meta
            .scenarios
            .iter()
            .find(|scenario| scenario.name == "a busy inbox")
            .expect("Mail stages an inbox")
            .action
            .clone();
        self.replica
            .host
            .runtime()
            .invoke_action(&self.who, (scenario.as_str(), Vec::new()))
            .unwrap();
        mail
    }

    fn views(&self) -> ResourceRef {
        self.views.clone()
    }

    fn read(&self, at: &ResourceRef, field: &str) -> Value {
        self.replica.host.runtime().read(at, field).unwrap()
    }

    fn text(&self, at: &ResourceRef, field: &str) -> String {
        self.read(at, field).as_text().unwrap().to_owned()
    }

    fn items(&self, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
        let Value::List(items) = self.read(at, field) else {
            panic!("{field} is not a list");
        };
        items.into_iter().map(|v| target(&v)).collect()
    }

    fn author(&self, view: &ResourceRef) -> Actor {
        match self.read(view, "author") {
            Value::Actor(p) => p,
            other => panic!("the author is an actor, not {other:?}"),
        }
    }

    /// What the view `v` of theirs shows over `at`, drawn as the system draws it: as them, its
    /// author the one the host recorded.
    fn shown(&self, v: &ResourceRef, at: &ResourceRef) -> Vec<String> {
        let view = self
            .replica
            .host
            .runtime()
            .open_written_view(
                &self.who,
                &self.text(v, "definition"),
                &self.author(v),
                vec![Value::resource(at)],
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let mut view = view;
        view.set_lazy_rows(false).unwrap();
        titles(view.tree())
    }
}

fn titles(tree: &UiNode) -> Vec<String> {
    tree.of_kind("ui.ListRow")
        .into_iter()
        .map(|row| text_prop(row, "title"))
        .collect()
}

fn text_prop(node: &UiNode, name: &str) -> String {
    match node.props.get(name) {
        Some(Value::Text(text)) => text.clone(),
        other => panic!("property {name} of {} is {other:?}", node.kind),
    }
}

/// The resource an action returned.
fn returned_ref(out: InvokeOutcome) -> ResourceRef {
    match out {
        InvokeOutcome::Done(value) => target(&value),
        other => panic!("expected a returned resource, got {other:?}"),
    }
}

/// The view an agent answers "what's unread in my mail?" with: a view over Mail, which Views
/// names nowhere.
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

/// The agent's answer, as its task keeps it in the person's views: checked first, the type it
/// shows found by the check.
fn answer(ada: &Person, task: &Actor) -> ResourceRef {
    let info = ada
        .replica
        .host
        .runtime()
        .check_written_view(UNREAD, task)
        .unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(info.name, "unread_mail");
    assert_eq!(info.shows.as_deref(), Some("so.tilo.mail.Mail"));
    returned_ref(ada.call_as(
        task,
        VIEWS_APP,
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
    let ada = Person::new("ada");
    let mail = ada.inbox();
    let task = agent("agent-task:unread");
    let v = answer(&ada, &task);
    // The host recorded the task as its author; nobody can claim another.
    assert_eq!(ada.author(&v), task);
    assert_eq!(ada.read(&v, "kept"), Value::Bool(false));
    // It is a real view over Mail: her unread conversations, live, and nothing she read.
    let shown = ada.shown(&v, &mail);
    assert!(
        shown.contains(&LAUNCH.to_owned()) && shown.contains(&DINNER.to_owned()),
        "{shown:?}"
    );
    assert!(!shown.contains(&INVOICE.to_owned()), "{shown:?}");
    // She keeps it: it is hers now, in her own record.
    ada.call(VIEWS_APP, "keep", vec![Value::resource(&v)]);
    assert_eq!(ada.read(&v, "kept"), Value::Bool(true));
    // And makes it how her Mail shows on the desktop.
    ada.call(
        VIEWS_APP,
        "make_default",
        vec![Value::resource(&ada.views()), Value::resource(&v)],
    );
    let defaults = ada.items(&ada.views(), "defaults");
    assert_eq!(defaults.len(), 1);
    assert_eq!(ada.text(&defaults[0], "shows"), "so.tilo.mail.Mail");
    // A view that does not compile says why, located in what was written.
    let wrong = ada
        .replica
        .host
        .runtime()
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
        ada.replica
            .host
            .runtime()
            .check_written_view("view t(x: board.Card) {\n    text(x.title)\n}", &ada.who)
            .is_err()
    );
}

#[test]
fn a_sent_view_shows_its_recipient_their_own_mail_and_asks_before_acting() {
    let ada = Person::new("ada");
    let bo = Person::new("bo");
    let ada_mail = ada.inbox();
    let bo_mail = bo.inbox();
    // Bo's mail is his own: he read the launch plan.
    let launch = bo
        .items(&bo.items(&bo_mail, "accounts")[0], "threads")
        .into_iter()
        .find(|t| bo.text(t, "subject") == LAUNCH)
        .unwrap();
    bo.call(
        MAIL_APP,
        "mark_read",
        vec![Value::resource(&launch), Value::Bool(false)],
    );
    let v = answer(&ada, &agent("agent-task:unread"));
    ada.call(VIEWS_APP, "keep", vec![Value::resource(&v)]);

    // Ada sends it to the team's channel. Only what the send writes travels between them.
    let watch = ada.replica.host.store().watch();
    let _ = watch.take();
    let channels = ada.replica.host.root_of(CHAT_APP).unwrap();
    let general = returned_ref(ada.call(
        CHAT_APP,
        "new_channel",
        vec![Value::resource(&channels), Value::text("general")],
    ));
    let outcome = ada.call(
        VIEWS_APP,
        "send",
        vec![Value::resource(&v), Value::resource(&general)],
    );
    assert!(matches!(outcome, InvokeOutcome::Done(_)), "{outcome:?}");
    let shared = watch.take().docs;
    assert!(!shared.is_empty(), "the send wrote nothing");
    ada.replica.send_docs_to(&bo.replica, &shared);
    let message = bo
        .items(&general, "messages")
        .pop()
        .expect("the view arrived");

    // Bo adds it to his own views: its author is who sent it, as the log recorded her.
    let bos = returned_ref(bo.call(
        VIEWS_APP,
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

/// What a view `writer` wrote does with an action asks `p` first; once they trust the writer,
/// it does not.
fn asks_before_acting(p: &Person, writer: &Actor, mail: &ResourceRef) {
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
            .filter(|t| p.read(t, "unread") == Value::Bool(true))
            .count()
    };
    let unread_before = unread();
    let view = p
        .replica
        .host
        .runtime()
        .open_written_view(&p.who, acting, writer, vec![Value::resource(mail)])
        .unwrap();
    let button = view.tree().of_kind("ui.Button")[0].key.clone();
    match view.press(&button).unwrap() {
        InvokeOutcome::Asked(_) => {}
        other => panic!("an untrusted author's view asks first, not {other:?}"),
    }
    let Invocation::Action { author, asks, .. } = p.dispatch.waiting() else {
        panic!("the press asks for an action");
    };
    assert!(asks, "the dispatch is told a person must be asked");
    assert_eq!(
        author,
        Some(ModuleHash::written_by(writer)),
        "the dispatch names who wrote the view"
    );
    assert_eq!(unread(), unread_before, "asking changed nothing");
    p.replica
        .host
        .authority()
        .trust(&ModuleHash::written_by(writer));
    let view = p
        .replica
        .host
        .runtime()
        .open_written_view(&p.who, acting, writer, vec![Value::resource(mail)])
        .unwrap();
    let button = view.tree().of_kind("ui.Button")[0].key.clone();
    assert!(matches!(
        view.press(&button).unwrap(),
        InvokeOutcome::Done(_)
    ));
    assert_eq!(unread().checked_add(1), Some(unread_before));
}
