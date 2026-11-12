//! A server's sign-in: the user name and password a site (or a proxy) asks
//! for before it answers (HTTP authentication).
//!
//! The request waits until it is answered. The question is the system's one
//! password prompt over the page's window, and the window's [`ROW`] offers
//! the same two answers, so a person, a screen reader and an agent answer
//! the one question the request waits on.
//!
//! Who asks is the origin the credentials go to, and nothing else names the
//! prompt: never the page's title, and never the realm, which is the
//! server's own text and is shown as its words. The engine asks only for the
//! page's own document and what it loads from its own origin
//! ([`ui::webview::http_auth`]), and an answer names the asker the prompt
//! showed, so what is typed goes to the one shown or nowhere.
//!
//! The password is a secret. It lives in the prompt's zeroizing field until
//! it is sent, is no part of the row, and nothing answers with it. What the
//! person typed is the prompt's alone: no action reads the fields, and an
//! agent that answers gives its own credentials, as a secret.
//!
//! Nothing waits for good. The engine ends a sign-in whose page went
//! elsewhere, whose tab closed, or that nobody answered in its time, and
//! says so; a newer one takes the place of one still open.
use std::sync::{Arc, Weak};

use ui::tilo_actions::{ResourcePresentation, ResourceSpec};
use ui::webview::{
    HttpAuthChallenge, HttpAuthCredentials, SharedEngine, WebViewCommand, WebViewEngine,
    WebViewEvent,
};
use ui::widget::{ModalDialog, PromptOutcome, SecretAnswer, SecretKind, SecretPrompt, Widget};

use crate::CATALOG;
use crate::effects::BrowserEffect;

/// The window's row for the sign-in a request waits on, and the prompt's
/// test id.
pub const ROW: &str = "browser.page-sign-in";

/// The longest site a title names. A longer one is named in full under the
/// title, where it wraps, so no part of who asks is ever cut away.
const MAX_TITLED_SITE: usize = 48;

struct Open {
    challenge: HttpAuthChallenge,
    /// Who asks: the prompt's title and the row's label.
    title: String,
    /// Who asks in full, then the server's own words, then what the person
    /// should know of the connection.
    detail: String,
    dialog: Box<ModalDialog<SecretPrompt>>,
}

/// What an answer did, for whoever gave it.
pub struct Answered {
    asker: String,
    /// Credentials were sent, rather than the sign-in cancelled.
    pub credentials: bool,
}

impl Answered {
    /// What the answer did and what to do next, in the words an agent reads.
    /// It names who was answered and never what they were answered with.
    pub fn said(&self) -> String {
        if self.credentials {
            format!(
                "Sent the user name and password to {}. The page goes on from here: read-page \
                 shows what it says now, and the {ROW} row asks again if they were not accepted.",
                self.asker
            )
        } else {
            format!(
                "Cancelled the sign-in {} asked for. Nothing was sent, and the page shows the \
                 server's own answer.",
                self.asker
            )
        }
    }
}

/// The sign-in a request of the page waits on, if any.
#[derive(Default)]
pub struct PageSignIn {
    /// The page engine asked for its sign-ins; another engine (a restarted
    /// one) is asked again, and the sign-in of the old one goes.
    asked: Option<Weak<dyn WebViewEngine>>,
    open: Option<Open>,
    /// The question the person answered last and the user name they gave:
    /// asked again because it was not accepted, the prompt starts with it.
    given: Option<(String, String)>,
}

impl PageSignIn {
    /// Ask `page` for its sign-ins, once per engine. Returns whether what
    /// is shown changed.
    pub fn sync(&mut self, page: &SharedEngine) -> bool {
        let asked = self
            .asked
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|asked| Arc::ptr_eq(&asked, page));
        if asked {
            return false;
        }
        page.accept_http_auth();
        self.asked = Some(Arc::downgrade(page));
        self.given = None;
        self.open.take().is_some()
    }

    /// Follow what the engine said of the page's sign-ins. Returns whether
    /// what is shown changed.
    pub fn apply(&mut self, event: &WebViewEvent, page: &SharedEngine) -> bool {
        match event {
            // The engine holds one sign-in of a page at a time: a new one
            // means the last is no longer waited on.
            WebViewEvent::HttpAuth(challenge) => {
                let shown = self.open.take().is_some();
                // Who asks is all a prompt may be named by. A challenge that
                // does not say, or says more than an origin, is shown to
                // nobody and answered with nothing.
                if !challenge.well_formed() {
                    page.send(WebViewCommand::AnswerHttpAuth {
                        id: challenge.id,
                        asker: challenge.asker.clone(),
                        credentials: None,
                    });
                    return shown;
                }
                // Only the question the person just answered starts with the
                // user name they gave it.
                let again = asked_of(challenge);
                self.given
                    .take_if(|(question, _)| !challenge.retry || *question != again);
                let username = self.given.as_ref().map(|(_, username)| username.as_str());
                self.open = Some(Open::new(challenge.clone(), username));
                true
            }
            WebViewEvent::HttpAuthClosed { id } => {
                self.open.take_if(|open| open.challenge.id == *id).is_some()
            }
            _ => false,
        }
    }

    /// The sign-in a request waits on.
    pub fn current(&self) -> Option<&HttpAuthChallenge> {
        self.open.as_ref().map(|open| &open.challenge)
    }

    /// What the person answered in the prompt, once: the sign-in it answers,
    /// and their credentials or none when they cancelled. The password
    /// leaves the prompt's field here and is held nowhere else.
    pub fn take_answer(&mut self) -> Option<(u32, Option<HttpAuthCredentials>)> {
        let open = self.open.as_mut()?;
        let prompt = open.dialog.content_mut();
        if !prompt.answer_pending() {
            return None;
        }
        let id = open.challenge.id;
        match prompt.take_outcome()? {
            PromptOutcome::Cancel => Some((id, None)),
            PromptOutcome::Submit(answer) => {
                let credentials = credentials(answer);
                if !credentials.within_limits() {
                    prompt.reject(CATALOG.t("browser-page-sign-in-too-long"));
                    return None;
                }
                self.given = Some((asked_of(&open.challenge), credentials.username().to_owned()));
                Some((id, Some(credentials)))
            }
        }
    }

    /// Answer the sign-in `id` with `credentials`, or cancel it with none.
    /// The answer names the asker the prompt showed, so the engine sends it
    /// to that one or nowhere.
    pub fn answer(
        &mut self,
        page: &SharedEngine,
        id: u32,
        credentials: Option<HttpAuthCredentials>,
    ) -> Result<Answered, String> {
        let open = self
            .open
            .as_ref()
            .filter(|open| open.challenge.id == id)
            .ok_or("no request waits on that sign-in any more")?;
        if credentials
            .as_ref()
            .is_some_and(|credentials| !credentials.within_limits())
        {
            return Err("the user name or password is too long to send".into());
        }
        let answered = Answered {
            asker: open.challenge.asker.clone(),
            credentials: credentials.is_some(),
        };
        page.send(WebViewCommand::AnswerHttpAuth {
            id,
            asker: open.challenge.asker.clone(),
            credentials,
        });
        self.open = None;
        Ok(answered)
    }

    /// The prompt over the page while a request waits.
    pub fn widget(&self) -> Option<&dyn Widget> {
        self.open.as_ref().map(|open| &*open.dialog as &dyn Widget)
    }

    pub fn widget_mut(&mut self) -> Option<&mut dyn Widget> {
        self.open
            .as_mut()
            .map(|open| &mut *open.dialog as &mut dyn Widget)
    }

    /// The prompt itself, for what a test reads of it.
    #[cfg(test)]
    pub(crate) fn prompt(&self) -> Option<&SecretPrompt> {
        self.open.as_ref().map(|open| open.dialog.content())
    }

    /// The sign-in as the window's row: who asks, what the server says of
    /// itself, and its two answers, each bound to this sign-in so an answer
    /// meant for it never reaches the next. Nothing typed is part of it.
    pub fn resource(&self) -> Option<ResourceSpec> {
        use ui::tilo_resource::Effect as _;
        let open = self.open.as_ref()?;
        let sign_in = open.challenge.id;
        let mut row = ResourceSpec::new(ROW, ROW, open.title.clone()).with_presentation(
            ResourcePresentation {
                detail: None,
                badge: None,
                preview: Some(crate::page_dialogs::preview(&open.detail)),
            },
        );
        // Both credentials are asked of whoever answers here: what the
        // prompt's fields hold is the person's, and no action sends it.
        let mut answer = BrowserEffect::AnswerPageSignIn {
            sign_in,
            username: String::new(),
            password: String::new(),
        }
        .bound_asking(&["username", "password"]);
        answer.type_mut().label = CATALOG.t("browser-page-sign-in-submit");
        row.actions.push(answer);
        let mut cancel = BrowserEffect::CancelPageSignIn { sign_in }.bound();
        cancel.type_mut().label = CATALOG.t("browser-page-dialog-cancel");
        row.actions.push(cancel);
        Some(row)
    }
}

impl Open {
    fn new(challenge: HttpAuthChallenge, username: Option<&str>) -> Self {
        let (title, detail) = words(&challenge);
        let mut prompt = SecretPrompt::new(SecretKind::UsernameAndSecret)
            .with_id(ROW)
            .with_title(title.clone())
            .with_resource(detail.clone())
            .with_primary_label(CATALOG.t("browser-page-sign-in-submit"));
        if let Some(username) = username {
            prompt = prompt.with_username(username);
        }
        if challenge.retry {
            prompt = prompt.with_error(Some(CATALOG.t("browser-page-sign-in-retry")));
        }
        let mut dialog = Box::new(prompt.into_dialog().with_test_id(ROW));
        // The window drives the prompt outside the app's focus order.
        Widget::set_focus(dialog.content_mut(), true);
        Self {
            challenge,
            title,
            detail,
            dialog,
        }
    }
}

/// What the person typed, as the credentials to send. The password is moved
/// out of the prompt's answer, which keeps no copy.
fn credentials(mut answer: SecretAnswer) -> HttpAuthCredentials {
    HttpAuthCredentials::new(
        answer.username.take().unwrap_or_default(),
        std::mem::take(&mut *answer.secret),
    )
}

/// What tells one question from another: who asks, and for what realm.
fn asked_of(challenge: &HttpAuthChallenge) -> String {
    format!(
        "{}\n{}\n{}",
        challenge.proxy, challenge.asker, challenge.realm
    )
}

/// The prompt's title and what stands under it. The title names the site
/// that asks; under it stand the asker in full (its scheme and port too),
/// the realm as the server's own words, and that the password crosses the
/// network unprotected when it does.
fn words(challenge: &HttpAuthChallenge) -> (String, String) {
    let named = |key: &str, name: &str, value: &str| {
        CATALOG.t_args(key, &[(name, ui::i18n::ArgValue::Text(value.to_owned()))])
    };
    let site = challenge.site();
    let title = match (challenge.proxy, site.chars().count() <= MAX_TITLED_SITE) {
        (false, true) => named("browser-page-sign-in-title", "site", site),
        (false, false) => CATALOG.t("browser-page-sign-in-title-long"),
        (true, true) => named("browser-page-sign-in-title-proxy", "site", site),
        (true, false) => CATALOG.t("browser-page-sign-in-title-proxy-long"),
    };
    let mut detail = named(
        if challenge.proxy {
            "browser-page-sign-in-asks-proxy"
        } else {
            "browser-page-sign-in-asks"
        },
        "asker",
        &challenge.asker,
    );
    let realm = shown_realm(&challenge.realm);
    if !realm.is_empty() {
        detail.push(' ');
        detail.push_str(&named("browser-page-sign-in-realm", "realm", &realm));
    }
    if challenge.sends_in_the_clear() {
        detail.push(' ');
        detail.push_str(&CATALOG.t("browser-page-sign-in-clear"));
    }
    (title, detail)
}

/// The realm as it is shown: without the characters that change the
/// direction text is read in, so the server's words cannot reorder the ones
/// beside them.
fn shown_realm(realm: &str) -> String {
    realm
        .chars()
        .filter(|character| {
            !matches!(character,
                '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::webview::test_engine::TestEngine;
    use ui::widget::FieldRole;

    const PASSWORD: &str = "correct-horse-9";

    fn challenge(id: u32, asker: &str, realm: &str) -> HttpAuthChallenge {
        HttpAuthChallenge {
            id,
            asker: asker.into(),
            proxy: false,
            realm: realm.into(),
            scheme: "basic".into(),
            retry: false,
        }
    }

    fn asks(id: u32, asker: &str, realm: &str) -> WebViewEvent {
        WebViewEvent::HttpAuth(challenge(id, asker, realm))
    }

    fn answers(engine: &TestEngine) -> Vec<(u32, String, Option<HttpAuthCredentials>)> {
        engine
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::AnswerHttpAuth {
                    id,
                    asker,
                    credentials,
                } => Some((id, asker, credentials)),
                _ => None,
            })
            .collect()
    }

    fn preview(sign_in: &PageSignIn) -> String {
        sign_in
            .resource()
            .and_then(|row| row.presentation)
            .and_then(|presentation| presentation.preview)
            .expect("the sign-in says who asks")
    }

    #[test]
    fn a_sign_in_names_who_asks_and_is_answered_once() {
        let engine = TestEngine::new();
        let page: SharedEngine = engine.clone();
        let mut sign_in = PageSignIn::default();
        assert!(!sign_in.sync(&page));
        assert!(engine.accepts_http_auth(), "the page's sign-ins come here");
        assert!(sign_in.apply(&asks(3, "https://staging.example.org", "Staff wiki"), &page));

        let row = sign_in.resource().expect("the sign-in is a row");
        assert_eq!(row.label, "Sign in to staging.example.org");
        assert_eq!(
            preview(&sign_in),
            "https://staging.example.org asks for a user name and password. In its own words: \
             “Staff wiki”."
        );
        let ids: Vec<_> = row.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["answer-page-sign-in", "cancel-page-sign-in"]);
        let labels: Vec<_> = row.actions.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, ["Sign in", "Cancel"]);
        let asked: Vec<_> = row.actions[0]
            .params()
            .iter()
            .map(|param| (&*param.name, param.ty))
            .collect();
        assert_eq!(
            asked,
            [
                ("username", ui::tilo_actions::ParamType::Text),
                ("password", ui::tilo_actions::ParamType::Secret),
            ],
            "both are asked of whoever answers, the password as a secret; the sign-in is bound"
        );
        assert!(row.actions[1].params().is_empty());

        let given = || Some(HttpAuthCredentials::new("ada", PASSWORD));
        assert!(
            sign_in.answer(&page, 2, given()).is_err(),
            "an answer meant for another sign-in"
        );
        assert!(answers(&engine).is_empty(), "which sends nothing");
        let answered = sign_in.answer(&page, 3, given()).unwrap();
        assert!(answered.credentials);
        assert!(
            answered.said().contains("https://staging.example.org")
                && !answered.said().contains(PASSWORD)
                && !answered.said().contains("ada")
        );
        assert_eq!(
            answers(&engine),
            [(3, "https://staging.example.org".to_owned(), given())],
            "the answer names the asker that was shown"
        );
        assert!(sign_in.widget().is_none() && sign_in.resource().is_none());
        assert!(sign_in.answer(&page, 3, None).is_err(), "answered once");

        // The engine stops waiting by itself: the prompt goes.
        sign_in.apply(&asks(4, "https://staging.example.org", "Staff wiki"), &page);
        assert!(!sign_in.apply(&WebViewEvent::HttpAuthClosed { id: 3 }, &page));
        assert!(sign_in.apply(&WebViewEvent::HttpAuthClosed { id: 4 }, &page));
        assert!(sign_in.current().is_none());

        // Cancelling sends no credentials, and says so.
        sign_in.apply(&asks(5, "https://staging.example.org", "Staff wiki"), &page);
        let cancelled = sign_in.answer(&page, 5, None).unwrap();
        assert!(!cancelled.credentials && cancelled.said().starts_with("Cancelled"));
        assert_eq!(
            answers(&engine)[1],
            (5, "https://staging.example.org".to_owned(), None)
        );

        // A restarted engine is asked again; the old one's sign-in goes.
        sign_in.apply(&asks(6, "https://staging.example.org", "Staff wiki"), &page);
        let restarted = TestEngine::new();
        let page: SharedEngine = restarted.clone();
        assert!(sign_in.sync(&page));
        assert!(restarted.accepts_http_auth() && sign_in.current().is_none());
        assert!(answers(&restarted).is_empty());
    }

    #[test]
    fn the_realm_is_the_servers_own_words_and_never_who_asks() {
        let page: SharedEngine = TestEngine::new();
        let mut sign_in = PageSignIn::default();
        sign_in.sync(&page);

        // A server that calls itself somebody else is still named by its
        // own origin, and its words stand in quotes after it.
        sign_in.apply(
            &asks(
                1,
                "https://evil.example",
                "bank.example \u{202e}needs your password",
            ),
            &page,
        );
        assert_eq!(sign_in.resource().unwrap().label, "Sign in to evil.example");
        assert_eq!(
            preview(&sign_in),
            "https://evil.example asks for a user name and password. In its own words: \
             “bank.example needs your password”.",
            "without what would turn the text around"
        );

        // No realm, no words of the server's.
        sign_in.apply(&asks(2, "https://example.org:8443", ""), &page);
        assert_eq!(
            sign_in.resource().unwrap().label,
            "Sign in to example.org:8443"
        );
        assert_eq!(
            preview(&sign_in),
            "https://example.org:8443 asks for a user name and password."
        );

        // A password that crosses a network unprotected is said to.
        sign_in.apply(&asks(3, "http://192.168.1.1", "Router"), &page);
        assert_eq!(
            preview(&sign_in),
            "http://192.168.1.1 asks for a user name and password. In its own words: “Router”. \
             This connection is not private: the password can be read on its way."
        );
        sign_in.apply(&asks(4, "http://127.0.0.1:8080", ""), &page);
        assert!(!preview(&sign_in).contains("not private"), "this device");

        // A proxy is named as one, by its host and port.
        let mut proxy = challenge(5, "proxy.example:3128", "Office");
        proxy.proxy = true;
        sign_in.apply(&WebViewEvent::HttpAuth(proxy), &page);
        assert_eq!(
            sign_in.resource().unwrap().label,
            "Sign in to the proxy proxy.example:3128"
        );
        assert_eq!(
            preview(&sign_in),
            "The proxy proxy.example:3128 asks for a user name and password. In its own words: \
             “Office”."
        );

        // A name too long for a title is never cut: it stands in full below.
        let long = format!("https://{}.example.org", "a".repeat(80));
        sign_in.apply(&asks(6, &long, ""), &page);
        assert_eq!(sign_in.resource().unwrap().label, "Sign in to this site");
        assert!(preview(&sign_in).starts_with(&long));
    }

    #[test]
    fn a_challenge_that_does_not_name_an_origin_is_shown_to_nobody() {
        let engine = TestEngine::new();
        let page: SharedEngine = engine.clone();
        let mut sign_in = PageSignIn::default();
        sign_in.sync(&page);
        for (id, asker) in [
            (1, "https://bank.example/@evil.example"),
            (2, "https://ada@bank.example"),
            (3, "bank.example"),
            (4, ""),
        ] {
            assert!(!sign_in.apply(&asks(id, asker, "Staff"), &page), "{asker}");
            assert!(sign_in.widget().is_none() && sign_in.resource().is_none());
            assert_eq!(
                answers(&engine).last(),
                Some(&(id, asker.to_owned(), None)),
                "it is cancelled, not left waiting"
            );
        }
    }

    #[test]
    fn a_newer_sign_in_takes_the_place_of_one_still_open_and_a_retry_says_so() {
        let engine = TestEngine::new();
        let page: SharedEngine = engine.clone();
        let mut sign_in = PageSignIn::default();
        sign_in.sync(&page);
        sign_in.apply(&asks(1, "https://one.example", "First"), &page);
        sign_in.apply(&asks(2, "https://two.example", "Second"), &page);
        assert_eq!(sign_in.current().map(|challenge| challenge.id), Some(2));
        assert_eq!(sign_in.resource().unwrap().label, "Sign in to two.example");
        assert!(
            sign_in
                .answer(&page, 1, Some(HttpAuthCredentials::new("ada", PASSWORD)))
                .is_err(),
            "an answer for the first reaches nobody"
        );
        assert!(answers(&engine).is_empty());
        let prompt = sign_in.prompt().unwrap();
        assert!(prompt.error().is_none());
        assert_eq!(prompt.focused_field(), Some(FieldRole::Username));
        assert!(
            prompt.fields().all(|(_, field)| field.value().is_empty()),
            "a prompt starts empty"
        );

        // Asked again because the credentials were not accepted: it says so.
        let mut again = challenge(3, "https://two.example", "Second");
        again.retry = true;
        sign_in.apply(&WebViewEvent::HttpAuth(again), &page);
        let prompt = sign_in.prompt().unwrap();
        assert_eq!(
            prompt.error(),
            Some("That user name or password was not accepted.")
        );
        assert!(
            prompt
                .field(FieldRole::Secret)
                .is_some_and(|field| field.is_secret() && field.value().is_empty())
        );
    }
}
