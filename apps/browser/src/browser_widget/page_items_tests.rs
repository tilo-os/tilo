//! A field filled from a vault item by reference ([`super::page_items`]): what reaches the
//! engine, what never leaves the window, and what the window strikes out afterwards.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::json;
use tilo_webview::page::STRUCK;
use tilo_webview::{
    FillInto, PageFailure, PageNavigation, PageReply, PageRequest, PageResourceNote, ReleasedText,
    WebViewCommand, WebViewEngine as _,
};
use ui::tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};

use super::tests::{effect_operation, synced};
use super::*;
use crate::page_items::{ASKED_FIRST, ItemAnswer, ItemAsk, ItemSource, released_origin};

const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";
/// What only the site's own page may be written. It holds what an address escapes.
const PASSWORD: &str = "open sesame/7+7%";
const CODE: &str = "481516";

/// A person's vault as a window reaches it: what it was asked, and what a test has it answer.
#[derive(Default)]
struct Vault {
    asked: RefCell<Vec<ItemAsk>>,
    answers: RefCell<Vec<(InvocationId, ItemAnswer)>>,
    forgotten: RefCell<Vec<InvocationId>>,
}

impl Vault {
    fn answers(&self, invocation: InvocationId, answer: ItemAnswer) {
        self.answers.borrow_mut().push((invocation, answer));
    }

    fn releases(&self, invocation: InvocationId, text: &str, into: FillInto) {
        self.answers(
            invocation,
            ItemAnswer::Released {
                text: ReleasedText::new(text.to_owned()),
                into,
            },
        );
    }
}

impl ItemSource for Vault {
    fn ask(&self, ask: ItemAsk) {
        self.asked.borrow_mut().push(ask);
    }

    fn answered(&self, invocation: InvocationId) -> Option<ItemAnswer> {
        let mut answers = self.answers.borrow_mut();
        let index = answers.iter().position(|(asked, _)| *asked == invocation)?;
        Some(answers.remove(index).1)
    }

    fn forget(&self, invocation: InvocationId) {
        self.forgotten.borrow_mut().push(invocation);
    }
}

struct Window {
    engine: Arc<TestEngine>,
    widget: BrowserWidget,
    vault: Rc<Vault>,
    doc: AutoCommit,
    root: ObjId,
}

impl Window {
    /// A visitor's window at `url` with a way to its person's vault.
    fn at(url: &str) -> Self {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate(url.into()));
        let mut widget =
            BrowserWidget::with_engine_mode(engine.clone(), true, BrowsingMode::Visitor);
        let vault = Rc::new(Vault::default());
        widget.set_item_source(vault.clone());
        let (doc, root) = synced(&mut widget);
        Self {
            engine,
            widget,
            vault,
            doc,
            root,
        }
    }

    /// Start the published page action `id`, as the action API does for the person.
    fn start(&mut self, id: &str, args: &serde_json::Value) -> Result<InvocationId, String> {
        let action = self
            .widget
            .published()
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.page")
            .and_then(|page| page.actions.iter().find(|action| action.id == id).cloned())
            .ok_or_else(|| format!("{id} is not offered"))?;
        let encoded = action
            .build_json(args.as_object().ok_or("arguments are an object")?)
            .map_err(|error| error.to_string())?;
        let invocation = InvocationId::new();
        match self
            .widget
            .admit_for(invocation, None, effect_operation(&encoded.payload))
            .map(Some)?
        {
            Some(ui::framework::Effected::Pending) => Ok(invocation),
            other => Err(format!("a page action must be accepted, got {other:?}")),
        }
    }

    fn fill(&mut self, element: &str) -> InvocationId {
        self.start("fill-page-item", &json!({"element": element, "item": ITEM}))
            .unwrap()
    }

    /// Let the engine's answers arrive and read `invocation`'s, as its own caller does.
    fn settled(&mut self, invocation: InvocationId) -> InvocationUpdate {
        for _ in 0..4 {
            self.widget.sync(&self.doc, &self.root);
            if let Some(update) = self.widget.poll_invocation(invocation) {
                return update;
            }
        }
        panic!("{invocation:?} never settled");
    }

    /// The fills the engine was sent, as the copies it keeps of them.
    fn fills(&self) -> Vec<PageRequest> {
        self.engine
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::Page { request, .. } if request.action() == "fill" => Some(request),
                _ => None,
            })
            .collect()
    }
}

fn offers(widget: &BrowserWidget, id: &str) -> bool {
    widget
        .published()
        .children
        .iter()
        .filter(|resource| resource.id.0 == "browser.page")
        .any(|page| page.actions.iter().any(|action| action.id == id))
}

fn succeeded(update: InvocationUpdate) -> serde_json::Value {
    match update {
        InvocationUpdate::Succeeded { output, .. } => output.value.expect("an answer has a value"),
        other => panic!("the page action must succeed, got {other:?}"),
    }
}

fn failed(update: InvocationUpdate) -> ActionFailure {
    match update {
        InvocationUpdate::Failed { error, .. } => error,
        other => panic!("the page action must fail, got {other:?}"),
    }
}

/// What the engine answers when the page says each of `said` wherever a page can say a thing.
fn saying(said: &[&str]) -> PageReply {
    PageReply {
        url: format!(
            "https://example.test/in?p={}",
            said.first().copied().unwrap_or_default()
        ),
        message: format!("the page says {}", said.join(" and ")),
        outline: Some(format!("- textbox \"{}\"", said.join(" "))),
        data: Some(json!({
            "entries": said
                .iter()
                .map(|text| json!({
                    "level": "log",
                    "text": format!("given {text}"),
                    "url": format!("https://example.test/in?p={text}"),
                }))
                .collect::<Vec<_>>(),
            "next": 3,
            "dropped": 0,
        })),
        image: None,
        navigation: PageNavigation::None,
        loading: false,
        record: PageResourceNote::default(),
    }
}

#[test]
fn a_field_is_filled_from_a_vault_item_and_only_the_engine_is_given_its_text() {
    // Without a way to its person's vault a window does not offer the action.
    let plain = {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate(
            "https://example.test/login".into(),
        ));
        let mut widget = BrowserWidget::with_engine_mode(engine, true, BrowsingMode::Visitor);
        let _ = synced(&mut widget);
        widget
    };
    assert!(!offers(&plain, "fill-page-item"));
    assert!(offers(&plain, "fill-page-secret"));

    let mut window = Window::at("https://example.test/login?next=%2Fhome");
    assert!(offers(&window.widget, "fill-page-item"));
    assert!(!window.widget.page_holds_sign_in());
    let invocation = window
        .start(
            "fill-page-item",
            &json!({"element": "d1e5", "item": ITEM, "reason": " To sign in for you. "}),
        )
        .unwrap();

    // The vault is asked for the item at the page's own site, and until it answers the
    // engine is asked nothing.
    let asked = window.vault.asked.borrow().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].invocation, invocation);
    assert_eq!(asked[0].item, ITEM);
    assert_eq!(asked[0].origin, "https://example.test");
    assert_eq!(asked[0].reason, "To sign in for you.");
    window.widget.sync(&window.doc, &window.root);
    assert!(window.widget.poll_invocation(invocation).is_none());
    assert!(window.engine.released_fills().is_empty());
    assert!(window.fills().is_empty(), "nothing reached the engine");

    window
        .vault
        .releases(invocation, PASSWORD, FillInto::Password);
    let answer = succeeded(window.settled(invocation));
    assert_eq!(answer["filled"], true);
    assert_eq!(answer["site"], "https://example.test");

    // The engine was written the text once, with the site it was released for and the kind
    // of field it is for.
    let written = window.engine.released_fills();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].element, "d1e5");
    assert_eq!(written[0].at, "https://example.test");
    assert_eq!(written[0].into, FillInto::Password);
    assert_eq!(written[0].text.as_deref(), Some(PASSWORD));

    // Nothing else holds it: not the answer, not a print of what the engine was sent, and
    // no copy of the request.
    let printed = format!("{answer} {:?}", window.engine.commands());
    assert!(
        !printed.contains(PASSWORD) && !printed.contains("sesame"),
        "{printed}"
    );
    let sent = window.fills();
    assert_eq!(sent.len(), 1);
    for request in sent {
        match request {
            PageRequest::FillReleased { text, at, .. } => {
                assert_eq!(at, "https://example.test");
                assert!(text.take().is_none(), "a copy holds no text");
            }
            other => panic!("a fill by reference is no {other:?}"),
        }
    }

    // A secret went into the page: it holds its person's sign-in from now on.
    assert!(window.widget.page_holds_sign_in());
}

#[test]
fn what_the_page_says_of_a_released_text_is_struck_out_of_every_answer() {
    let mut window = Window::at("https://example.test/login");
    let password = window.fill("d1e5");
    window
        .vault
        .releases(password, PASSWORD, FillInto::Password);
    let _ = succeeded(window.settled(password));
    // The second step: a code generator's code of this moment, into a field of its own.
    let code = window.fill("d1e7");
    window.vault.releases(code, CODE, FillInto::Code);
    let _ = succeeded(window.settled(code));
    let written = window.engine.released_fills();
    assert_eq!(written.len(), 2);
    assert_eq!(written[1].into, FillInto::Code);
    assert_eq!(written[1].element, "d1e7");
    assert_eq!(written[1].text.as_deref(), Some(CODE));

    // The page logs what it was given, and its form sends it in an address: as it is, as a
    // script escapes it, and as a sent form writes it.
    let said = [
        PASSWORD,
        "open%20sesame%2F7%2B7%25",
        "open+sesame%2f7%2b7%25",
        CODE,
    ];
    for read in ["read-page-console", "read-page-network", "read-page"] {
        window.engine.push_page_reply(Ok(saying(&said)));
        let asked = window.start(read, &json!({})).unwrap();
        let answer = succeeded(window.settled(asked)).to_string();
        for held in said.into_iter().chain(["sesame"]) {
            assert!(!answer.contains(held), "{read}: {answer}");
        }
        assert!(answer.contains(STRUCK), "{read}: {answer}");
    }

    // A refusal of the engine's says none of it either.
    window.engine.push_page_reply(Err(PageFailure::new(
        "stale",
        format!("no element holds {PASSWORD} or {CODE}"),
    )));
    let refused = window
        .start("click-page", &json!({"element": "d1e9"}))
        .unwrap();
    let error = failed(window.settled(refused));
    assert!(
        !error.message.contains("sesame") && !error.message.contains(CODE),
        "{}",
        error.message
    );
    assert!(error.message.contains(STRUCK), "{}", error.message);
}

#[test]
fn a_fill_whose_item_is_not_released_fills_nothing() {
    let mut window = Window::at("https://example.test/login");

    // Its person is asked first: the fill ends saying so, and is not to be made again.
    let asked = window.fill("d1e5");
    window.vault.answers(
        asked,
        ItemAnswer::Asked {
            words: "Your person is asked in the conversation.".into(),
        },
    );
    let error = failed(window.settled(asked));
    assert_eq!(error.code, ASKED_FIRST);
    assert_eq!(error.message, "Your person is asked in the conversation.");
    assert!(!error.retryable);

    // The vault refuses the site.
    let refused = window.fill("d1e5");
    window.vault.answers(
        refused,
        ItemAnswer::Refused {
            reason: "That item does not belong to this site".into(),
        },
    );
    let error = failed(window.settled(refused));
    assert_eq!(error.code, "browser.page.item_refused");
    assert_eq!(error.message, "That item does not belong to this site");

    // Its caller gives up before the vault answers, and the vault is told.
    let given_up = window.fill("d1e5");
    let stopped = failed(
        window
            .widget
            .poll_invocation_cancelling(given_up, true)
            .unwrap(),
    );
    assert_eq!(stopped.code, "browser.page.cancelled");
    assert_eq!(*window.vault.forgotten.borrow(), vec![given_up]);

    // The vault never answers: the fill ends when its wait is over.
    let unanswered = window.fill("d1e5");
    for waiting in &mut window.widget.item_fills {
        waiting.until = std::time::Instant::now();
    }
    let error = failed(window.settled(unanswered));
    assert_eq!(error.code, "browser.page.item_unanswered");
    assert_eq!(*window.vault.forgotten.borrow(), vec![given_up, unanswered]);

    // What is no reference, and what names no element, never reaches the vault, and the
    // refusal repeats nothing of what it was given.
    let before = window.vault.asked.borrow().len();
    for item in [PASSWORD, "vault.item:short", "vault.item:"] {
        let refusal = window
            .start("fill-page-item", &json!({"element": "d1e5", "item": item}))
            .unwrap_err();
        assert!(!refusal.contains("sesame"), "{refusal}");
    }
    assert!(
        window
            .start(
                "fill-page-item",
                &json!({"element": "not a ref", "item": ITEM})
            )
            .is_err()
    );
    assert_eq!(window.vault.asked.borrow().len(), before);

    // None of it reached the engine, and the page holds no sign-in.
    assert!(window.engine.released_fills().is_empty());
    assert!(window.fills().is_empty());
    assert!(!window.widget.page_holds_sign_in());
}

#[test]
fn an_item_is_asked_for_only_where_a_text_may_be_written() {
    for (address, origin) in [
        (
            "https://example.test/login?next=%2F",
            Some("https://example.test"),
        ),
        (
            "https://example.test:8443/a",
            Some("https://example.test:8443"),
        ),
        (
            "https://b\u{fc}cher.example/",
            Some("https://xn--bcher-kva.example"),
        ),
        ("http://localhost:3000/login", Some("http://localhost:3000")),
        ("http://127.0.0.1:8080/", Some("http://127.0.0.1:8080")),
        ("http://example.test/login", None),
        ("http://localhost.example.test/", None),
        ("about:blank", None),
        ("not an address", None),
    ] {
        assert_eq!(released_origin(address).as_deref(), origin, "{address}");
    }

    // A page that did not arrive over https is refused before the vault is asked.
    let mut window = Window::at("http://example.test/login");
    let refusal = window
        .start("fill-page-item", &json!({"element": "d1e5", "item": ITEM}))
        .unwrap_err();
    assert!(refusal.contains("https"), "{refusal}");
    assert!(window.vault.asked.borrow().is_empty());
    assert!(window.fills().is_empty());
}

/// A visitor's window at `url` whose session is `session`, stood in for: the window, and
/// what it was synced against.
fn window_of(url: &str, session: &Rc<crate::session_items::testing::Session>) -> Window {
    let engine = TestEngine::new();
    engine.send(WebViewCommand::Navigate(url.into()));
    let mut widget = BrowserWidget::with_engine_mode(engine.clone(), true, BrowsingMode::Visitor);
    widget.set_item_requests(session.clone());
    let (doc, root) = synced(&mut widget);
    widget.sync(&doc, &root);
    Window {
        engine,
        widget,
        vault: Rc::new(Vault::default()),
        doc,
        root,
    }
}

#[test]
fn a_window_whose_session_lends_items_has_a_field_filled_through_its_session() {
    use crate::session_items::testing::Session;
    use ui::collaboration::browser_service_sessions::{
        BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
    };

    let session = Rc::new(Session::default());
    let mut window = window_of("https://example.test/login?next=%2Fhome", &session);
    // The window asks its session whether items are lent, and offers nothing before the
    // session says so.
    assert_eq!(session.sent(), [Command::LendsItems {}]);
    assert!(!offers(&window.widget, "fill-page-item"));
    session.answers(0, Ok(Reply::ItemsLent { lent: true }));
    window.widget.sync(&window.doc, &window.root);
    assert!(offers(&window.widget, "fill-page-item"));

    // A fill asks the session for the item at the site the engine committed, and until the
    // session answers the engine is asked nothing.
    let invocation = window
        .start(
            "fill-page-item",
            &json!({"element": "d1e5", "item": ITEM, "reason": "To sign in for you."}),
        )
        .unwrap();
    assert_eq!(
        session.sent()[1],
        Command::UseItem {
            invocation,
            item: ITEM.into(),
            origin: "https://example.test".into(),
        }
    );
    window.widget.sync(&window.doc, &window.root);
    assert!(window.widget.poll_invocation(invocation).is_none());
    assert!(window.engine.released_fills().is_empty());

    session.answers(1, Session::text(invocation, PASSWORD, "password"));
    let answer = succeeded(window.settled(invocation));
    assert_eq!(answer["filled"], true);
    assert_eq!(answer["site"], "https://example.test");
    // The engine was written the text once, for the site it was asked for.
    let written = window.engine.released_fills();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].element, "d1e5");
    assert_eq!(written[0].at, "https://example.test");
    assert_eq!(written[0].into, FillInto::Password);
    assert!(
        written[0].text.as_deref() == Some(PASSWORD),
        "the engine was written another text than the session answered"
    );
    // Nothing else holds it: not the answer, not what the session was sent, and no print of
    // what the engine was sent.
    let printed = format!(
        "{answer} {:?} {:?}",
        window.engine.commands(),
        session.sent()
    );
    assert!(
        !printed.contains(PASSWORD) && !printed.contains("sesame"),
        "a print holds the item's text"
    );

    // A refusal ends the fill with the reason the session carried, and that its person is
    // asked first ends it with the words for whoever asked. Neither reaches the engine.
    let refused = window.fill("d1e5");
    session.answers(
        2,
        Ok(Reply::item_refused(
            refused,
            "Ada said no, so the item is not used.",
        )),
    );
    let failure = failed(window.settled(refused));
    assert_eq!(failure.code, "browser.page.item_refused");
    assert_eq!(failure.message, "Ada said no, so the item is not used.");
    let first = window.fill("d1e5");
    session.answers(3, Ok(Reply::item_asked(first, "Ada is asked first.")));
    let failure = failed(window.settled(first));
    assert_eq!(failure.code, ASKED_FIRST);
    assert_eq!(failure.message, "Ada is asked first.");
    assert_eq!(window.engine.released_fills().len(), 1);
    assert_eq!(session.sent().len(), 4);
}

#[test]
fn a_window_whose_session_lends_no_items_or_does_not_say_never_offers_the_fill() {
    use crate::session_items::testing::Session;
    use ui::collaboration::browser_service_sessions::{
        BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
    };

    // A person's own session: asked once, and the fill is never offered.
    let session = Rc::new(Session::default());
    let mut window = window_of("https://example.test/login", &session);
    session.answers(0, Ok(Reply::ItemsLent { lent: false }));
    for _ in 0..4 {
        window.widget.sync(&window.doc, &window.root);
    }
    assert!(!offers(&window.widget, "fill-page-item"));
    assert_eq!(session.sent(), [Command::LendsItems {}]);
    assert!(
        window
            .start("fill-page-item", &json!({"element": "d1e5", "item": ITEM}))
            .is_err()
    );

    // A session that refuses the question: asked a bounded number of times, never offered,
    // and no item is ever asked for.
    let session = Rc::new(Session::default());
    let mut window = window_of("https://example.test/login", &session);
    for _ in 0..40 {
        if session.busy() {
            let asked = session.sent().len() - 1;
            session.answers(asked, Err("private service-session request refused".into()));
        }
        window.widget.sync(&window.doc, &window.root);
    }
    assert!(!offers(&window.widget, "fill-page-item"));
    assert!(session.sent().len() <= 6, "{:?}", session.sent());
    assert!(
        session
            .sent()
            .iter()
            .all(|sent| *sent == Command::LendsItems {})
    );
}
