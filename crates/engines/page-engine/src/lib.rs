//! The page a window shows, one request at a time, with no UI of its own.
//!
//! A window holds one engine and one [`PageSession`]: the session asks the engine to do what the
//! page's actions say (`PageRequest`), keeps the answers until the frame that carries them back
//! to whoever asked, and takes every other event the engine reports into a queue the view
//! reads. The engine's events have one reader — this session — so a page's navigation and its
//! answers never take each other's.

pub mod codec;

mod asks;
mod prompts;

pub use asks::{Asks, Download, ExtensionButton, ExtensionNotice, Menu};
pub use prompts::{Prompt, PromptChange, PromptKey, PromptKind, Prompts};

use std::collections::{BTreeSet, HashMap, VecDeque};

use tilo_webview::{
    CredentialAction, CredentialRequest, FileChooser, PageActor, PageFailure, PageReply,
    PageRequest, SharedEngine, WebViewCommand, WebViewEvent,
};

/// One app window's page: its engine, the actions waiting for an answer, and the events its
/// view shows.
pub struct PageSession {
    engine: SharedEngine,
    /// The id of the last page action asked about.
    next: u32,
    /// The actions whose answers have not come.
    waiting: BTreeSet<u32>,
    /// The answers that came and wait to be carried to whoever asked.
    answers: Vec<(u32, Result<PageReply, PageFailure>)>,
    /// The engine's other events, in the order they came, for the view to show.
    events: VecDeque<WebViewEvent>,
    /// What the page waits on and last reported of itself, as its overlays and the app's records
    /// read it: a dialog, a sign-in, a permission, a held request, its context menu.
    asks: Asks,
    /// What the page waits on a person's yes for, as the window raises and withdraws it.
    prompts: Prompts,
    /// The page's file choosers a person's click opened, waiting for the host's picker: the
    /// window answers each and the page is never left waiting on one.
    choosers: Vec<FileChooser>,
    /// What the page says of itself that the app's own record carries (a download moving, the
    /// pages it can go to), for the window to hand the app as its calls: the record moves while
    /// the person looks.
    notes: Vec<String>,
    /// What each download last said, so a moving one is told at each tenth it reaches rather
    /// than at every event.
    download_steps: HashMap<u32, (String, u64)>,
    /// The engine's number for the next credential action.
    credential_next: u32,
    /// A credential action waiting for the page's current document: the engine's store only acts
    /// on the document the person is looking at, so the session asks for its generation first.
    credential_waiting: Option<CredentialRequest>,
    /// A private page's disposable root, which this session's engine started on and which goes
    /// when the session does: nothing a private window read or signed in to is kept.
    private_root: Option<std::path::PathBuf>,
}

impl PageSession {
    #[must_use]
    pub fn new(engine: SharedEngine) -> Self {
        Self {
            engine,
            next: 0,
            waiting: BTreeSet::new(),
            answers: Vec::new(),
            events: VecDeque::new(),
            asks: Asks::default(),
            prompts: Prompts::default(),
            choosers: Vec::new(),
            notes: Vec::new(),
            download_steps: HashMap::new(),
            credential_next: 0,
            credential_waiting: None,
            private_root: None,
        }
    }

    /// The page of a window opened private: `engine` is already started on `root`, a fresh
    /// profile that holds nothing the person signed in to, and this session removes it when the
    /// window closes.
    #[must_use]
    pub fn new_private(engine: SharedEngine, root: std::path::PathBuf) -> Self {
        let mut session = Self::new(engine);
        session.private_root = Some(root);
        session
    }

    /// Whether this page is private: nothing it reads or signs in to is kept, and its history is
    /// kept nowhere.
    #[must_use]
    pub fn is_private(&self) -> bool {
        self.private_root.is_some()
    }

    /// The engine this window shows its page with.
    #[must_use]
    pub fn engine(&self) -> &SharedEngine {
        &self.engine
    }

    /// Ask the page to do something as `actor` (`None` for the one unnamed actor a window has
    /// always had), and answer its id: the answer comes in [`Self::poll`] and [`Self::answers`].
    pub fn ask(&mut self, actor: Option<PageActor>, request: PageRequest) -> u32 {
        let request_id = self.next.wrapping_add(1);
        self.next = request_id;
        self.waiting.insert(request_id);
        self.engine.send(WebViewCommand::Page {
            request_id,
            actor,
            request,
        });
        request_id
    }

    /// Stop waiting for `action`: whoever asked gave up, and the page must not hold the next
    /// action behind this one.
    pub fn cancel(&mut self, action: u32) {
        if self.waiting.remove(&action) {
            self.engine
                .send(WebViewCommand::CancelPage { request_id: action });
        }
    }

    /// Take what the engine reported: the answers to the actions asked, and the page's other
    /// events for the view. A window calls this once a frame, before it draws.
    pub fn poll(&mut self) {
        for event in self.engine.drain_events() {
            match &event {
                WebViewEvent::PageResult { request_id, result } => {
                    // An answer to an action cancelled meanwhile is dropped: nobody waits for
                    // it, and the page settled it as asked.
                    if self.waiting.remove(request_id) {
                        self.answers.push((*request_id, result.clone()));
                    }
                    continue;
                }
                // A page's file chooser is answered by the host's picker, never shown as the
                // page's own event: the window takes it from here and asks the person.
                WebViewEvent::FileChooser(chooser) => {
                    self.choosers.push(chooser.clone());
                    continue;
                }
                WebViewEvent::FileChooserClosed { id } => {
                    self.choosers.retain(|chooser| chooser.id != *id);
                    continue;
                }
                // A download moving, and the pages the page can go to, are the app's record's:
                // the window hands them to the app as its calls.
                WebViewEvent::DownloadUpdated {
                    id,
                    state,
                    name,
                    url,
                    path,
                    retryable,
                    received,
                    total,
                    error,
                } => {
                    let step = received.saturating_mul(10).checked_div(*total).unwrap_or(0);
                    let said = (state.clone(), step);
                    if self.download_steps.insert(*id, said.clone()) != Some(said) {
                        self.notes.push(codec::download_note(codec::DownloadNote {
                            id: *id,
                            state: state.as_str(),
                            name: name.as_str(),
                            url: url.as_str(),
                            path: path.as_str(),
                            retryable: *retryable,
                            received: *received,
                            total: *total,
                            error: error.as_str(),
                        }));
                    }
                }
                WebViewEvent::NavigationHistory {
                    direction, entries, ..
                } => {
                    self.notes.push(codec::history_note(*direction, entries));
                }
                // What is installed and what installing, turning on or removing came to: the
                // app's record's, never the page's own event.
                WebViewEvent::ExtensionList {
                    installed, message, ..
                } => {
                    self.notes.push(codec::extensions_note(installed, message));
                    continue;
                }
                // A press on an extension's toolbar button that opens a page of the extension's
                // own is a popup: the app keeps it as a page of the browser.
                WebViewEvent::ExtensionOpenPopup { url, .. } => {
                    self.notes.push(codec::open_note(url, true));
                    continue;
                }
                WebViewEvent::ExtensionOutcome {
                    ok,
                    kind,
                    message,
                    id,
                    ..
                } => {
                    self.notes
                        .push(codec::extension_outcome_note(*ok, kind, message, id));
                    continue;
                }
                // How the engine's credential store answered: the forms and accounts it lists,
                // the capture it took, and what the action came to. A status the session asked
                // only to learn the page's document is not the person's to see: their own action
                // follows with that document.
                WebViewEvent::CredentialResult(reply) => {
                    if reply.action == "status" && self.credential_waiting.is_some() {
                        if let Some(mut waiting) = self.credential_waiting.take() {
                            if reply.document.is_empty() {
                                self.notes.push(codec::credential_note(
                                    waiting.action.name(),
                                    false,
                                    &reply.message,
                                    &[],
                                    &[],
                                    None,
                                    None,
                                    reply.late,
                                ));
                            } else {
                                waiting.document.clone_from(&reply.document);
                                self.send_credential(waiting);
                            }
                        }
                    } else {
                        self.notes.push(codec::credential_note(
                            &reply.action,
                            reply.success,
                            &reply.message,
                            &reply.forms,
                            &reply.accounts,
                            reply.capture.as_deref(),
                            reply.username.as_deref(),
                            reply.late,
                        ));
                    }
                }
                _ => {}
            }
            self.asks.apply(&event);
            self.prompts.apply(&event);
            self.events.push_back(event);
        }
        // Pages a script opened in a window of their own: a popup the site sized, or a plain new
        // tab. The app keeps them as pages; the window that holds one is the system's.
        for (url, _foreground) in self.engine.take_new_tab_requests() {
            self.notes.push(codec::open_note(&url, false));
        }
        self.engine.accept_popups();
        for popup in self.engine.take_popups() {
            self.notes.push(codec::open_note(&popup.url, true));
        }
    }

    /// The answers that came since the last call, oldest first.
    pub fn answers(&mut self) -> Vec<(u32, Result<PageReply, PageFailure>)> {
        std::mem::take(&mut self.answers)
    }

    /// The next event of the page for its view, in the order it came.
    pub fn next_event(&mut self) -> Option<WebViewEvent> {
        self.events.pop_front()
    }

    /// Whether `action` still waits for its answer.
    #[must_use]
    pub fn waiting_for(&self, action: u32) -> bool {
        self.waiting.contains(&action)
    }

    /// What the page waits on and last reported of itself.
    #[must_use]
    pub fn asks(&self) -> &Asks {
        &self.asks
    }

    /// What the page started and stopped waiting on a person's yes for since the window last
    /// took it, in the order it changed.
    pub fn prompts(&mut self) -> Vec<PromptChange> {
        self.prompts.take()
    }

    /// Whether a prompt started or stopped waiting since the window last took them.
    #[must_use]
    pub fn has_prompts(&self) -> bool {
        self.prompts.changed()
    }

    /// The page's file choosers that started waiting since the window last took them, oldest
    /// first: the window asks the person with the host's picker and answers each.
    pub fn choosers(&mut self) -> Vec<FileChooser> {
        std::mem::take(&mut self.choosers)
    }

    /// Whether a file chooser started waiting since the window last took them.
    #[must_use]
    pub fn has_choosers(&self) -> bool {
        !self.choosers.is_empty()
    }

    /// What the page said of itself since the window last took it, oldest first, each encoded
    /// for the app (`ToApp::PagePrompt`): a download moving, the pages it can go to.
    pub fn notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Whether the page said anything of itself since the window last took it.
    #[must_use]
    pub fn has_notes(&self) -> bool {
        !self.notes.is_empty()
    }

    /// Ask the engine's credential store to do `request`: the logins it keeps for the page's
    /// site, the forms it reads them from, or the capture of one the person typed. The store acts
    /// on the document the person is looking at and refuses one from a page they left, so the
    /// session asks which document that is and names it on the action. A password never crosses
    /// the action or its answer.
    pub fn credential(&mut self, request: CredentialRequest) {
        if matches!(
            request.action,
            CredentialAction::Status | CredentialAction::Cancel
        ) {
            if matches!(request.action, CredentialAction::Cancel) {
                self.credential_waiting = None;
            }
            self.send_credential(request);
            return;
        }
        let status = CredentialRequest {
            request_id: 0,
            origin: request.origin.clone(),
            document: String::new(),
            action: CredentialAction::Status,
        };
        self.credential_waiting = Some(request);
        self.send_credential(status);
    }

    /// Send one credential action, named by the session's own number for it.
    fn send_credential(&mut self, mut request: CredentialRequest) {
        self.credential_next = self.credential_next.wrapping_add(1);
        request.request_id = self.credential_next;
        self.engine.send(WebViewCommand::Credentials(request));
    }

    /// Answer the page's file chooser `id` with exactly the paths the person picked, or with
    /// none to cancel it.
    pub fn answer_chooser(&self, id: u32, paths: Vec<String>) {
        self.engine
            .send(WebViewCommand::AnswerFileChooser { id, paths });
    }

    /// Whether the page has something for the window: an answer to carry, or an event to show.
    #[must_use]
    pub fn busy(&self) -> bool {
        !self.waiting.is_empty()
            || !self.answers.is_empty()
            || !self.events.is_empty()
            || self.prompts.changed()
            || !self.choosers.is_empty()
            || self.credential_waiting.is_some()
            || !self.notes.is_empty()
    }
}

impl Drop for PageSession {
    fn drop(&mut self) {
        // The engine's own supervisor removes a private root once its child has exited; a
        // window that closed before one started leaves the root to this.
        if let Some(root) = self.private_root.take() {
            tilo_webview::chromium::release_private_root(&root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_webview::test_engine::TestEngine;

    /// A page's outline is asked once and answered once, and the page's own events stay theirs.
    #[test]
    fn an_answer_comes_once_and_the_pages_events_reach_its_view() {
        let test = TestEngine::new();
        let engine: SharedEngine = test.clone();
        let mut session = PageSession::new(engine);
        let action = session.ask(
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        );
        assert!(session.waiting_for(action));
        session.poll();
        let answers = session.answers();
        assert_eq!(answers.len(), 1, "one answer, for one action");
        assert_eq!(answers[0].0, action);
        assert!(
            answers[0].1.is_ok(),
            "the outline answers: {:?}",
            answers[0].1
        );
        assert!(!session.waiting_for(action), "the action is answered");

        // What the page reports of itself is the view's, not an answer's: it waits in order.
        test.push_event(WebViewEvent::UriChanged("https://example.org/".to_owned()));
        session.poll();
        assert!(matches!(
            session.next_event(),
            Some(WebViewEvent::UriChanged(uri)) if uri == "https://example.org/"
        ));
        assert!(session.next_event().is_none());
        assert!(session.answers().is_empty(), "events are not answers");
    }

    /// A file input's chooser is the window's to ask with the host's picker, never the page's
    /// own event, and the answer goes back with exactly the paths the person chose.
    #[test]
    fn a_file_chooser_is_the_windows_and_its_answer_is_the_persons_paths() {
        let test = TestEngine::new();
        let engine: SharedEngine = test.clone();
        let mut session = PageSession::new(engine);
        test.push_event(WebViewEvent::FileChooser(tilo_webview::FileChooser {
            id: 7,
            mode: tilo_webview::FileChooserMode::One,
            title: "Upload".to_owned(),
            accept: vec!["image/png".to_owned()],
        }));
        session.poll();
        assert!(session.has_choosers());
        let choosers = session.choosers();
        assert_eq!(choosers.len(), 1, "one chooser waits: {choosers:?}");
        assert_eq!(choosers[0].id, 7);
        assert!(
            session.next_event().is_none(),
            "the chooser is not an event the page's view draws"
        );

        session.answer_chooser(7, vec!["/home/ada/picture.png".to_owned()]);
        assert!(
            test.commands().iter().any(|command| matches!(
                command,
                WebViewCommand::AnswerFileChooser { id, paths }
                    if *id == 7 && paths == &["/home/ada/picture.png".to_owned()]
            )),
            "the page is answered with the person's path: {:?}",
            test.commands()
        );
    }

    /// The engine's credential store acts on the document the person is looking at: the session
    /// asks the page which one that is and names it on the person's action, and the question it
    /// asked is not the person's to see.
    #[test]
    fn a_credential_action_names_the_pages_own_document() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        session.credential(CredentialRequest {
            request_id: 0,
            origin: "https://example.org".to_owned(),
            document: String::new(),
            action: CredentialAction::Discover,
        });
        match test.commands().last() {
            Some(WebViewCommand::Credentials(request)) => {
                assert!(
                    matches!(request.action, CredentialAction::Status),
                    "the document is asked for first, naming no document itself: {request:?}"
                );
            }
            other => panic!("a status is asked for: {other:?}"),
        }

        test.push_event(WebViewEvent::CredentialResult(
            tilo_webview::CredentialReply {
                request_id: 1,
                action: "status".to_owned(),
                origin: "https://example.org".to_owned(),
                document: "4".to_owned(),
                success: true,
                message: "Ready".to_owned(),
                forms: Vec::new(),
                accounts: Vec::new(),
                capture: None,
                username: None,
                late: false,
            },
        ));
        session.poll();
        match test.commands().last() {
            Some(WebViewCommand::Credentials(request)) => {
                assert!(
                    matches!(request.action, CredentialAction::Discover),
                    "the person's action follows: {request:?}"
                );
                assert_eq!(
                    request.document, "4",
                    "named by the document the page is at"
                );
                assert_eq!(request.origin, "https://example.org");
            }
            other => panic!("the action follows the status: {other:?}"),
        }
        assert!(
            session.notes().is_empty(),
            "asking the page for its document is not the person's to see"
        );
    }

    /// A page whose logins the engine will not reach (it is not secure, or it has moved on) says
    /// so in the person's action's own answer, and no action is sent for a document unknown.
    #[test]
    fn a_credential_action_the_engine_refuses_is_answered_in_its_own_words() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        session.credential(CredentialRequest {
            request_id: 0,
            origin: "http://example.org".to_owned(),
            document: String::new(),
            action: CredentialAction::List,
        });
        let before = test.commands().len();
        test.push_event(WebViewEvent::CredentialResult(
            tilo_webview::CredentialReply {
                request_id: 1,
                action: "status".to_owned(),
                origin: "http://example.org".to_owned(),
                document: String::new(),
                success: false,
                message: "Page changed or secure form access is unavailable".to_owned(),
                forms: Vec::new(),
                accounts: Vec::new(),
                capture: None,
                username: None,
                late: false,
            },
        ));
        session.poll();
        assert_eq!(
            test.commands().len(),
            before,
            "nothing is asked of a page whose document the store will not name"
        );
        let notes = session.notes();
        let note: serde_json::Value = serde_json::from_str(&notes[0]).expect("the note is JSON");
        assert_eq!(note["did"], "list");
        assert_eq!(note["success"], false);
        assert_eq!(
            note["message"],
            "Page changed or secure form access is unavailable"
        );
    }

    /// A private page's profile is its window's alone and goes when the page does: nothing it
    /// read or signed in to is kept for the next window.
    #[test]
    fn a_private_page_owns_the_profile_it_goes_with() {
        let root = tilo_webview::chromium::allocate_private_root().expect("a private root");
        assert!(
            tilo_webview::chromium::is_owned_private_root(&root),
            "a root this browser made is one it may remove"
        );
        let session = PageSession::new_private(TestEngine::new(), root.clone());
        assert!(session.is_private());
        drop(session);
        assert!(!root.exists(), "the private profile goes with the page");
    }

    /// A page that is not private keeps this device's profile.
    #[test]
    fn an_ordinary_page_is_not_private() {
        assert!(!PageSession::new(TestEngine::new()).is_private());
    }

    /// An extension's toolbar button is what the engine last said of it, and a reset takes them
    /// all away; the button a press opens a page from is a popup the app keeps as a page.
    #[test]
    fn an_extensions_toolbar_button_is_what_the_engine_last_said() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        test.push_event(WebViewEvent::ExtensionAction {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            global: false,
            change: tilo_webview::ExtensionActionChange {
                badge: Some(Some("3".to_owned())),
                title: Some(Some("Read later".to_owned())),
                popup: None,
                enabled: None,
                icon: None,
            },
        });
        test.push_event(WebViewEvent::ExtensionAction {
            id: "ponmlkjihgfedcbaponmlkjihgfedcba".to_owned(),
            global: false,
            change: tilo_webview::ExtensionActionChange {
                enabled: Some(Some(false)),
                ..Default::default()
            },
        });
        session.poll();
        let buttons = &session.asks().extension_buttons;
        assert_eq!(buttons.len(), 2, "two buttons: {buttons:?}");
        assert!(buttons[0].enabled, "a button nobody dimmed takes a press");
        assert!(
            !buttons[1].enabled,
            "a button its extension dimmed does not"
        );
        assert_eq!(buttons[0].badge, "3");
        assert_eq!(buttons[0].title, "Read later");

        test.push_event(WebViewEvent::ExtensionActionReset);
        session.poll();
        assert!(
            session.asks().extension_buttons.is_empty(),
            "a reset takes them all away"
        );

        test.push_event(WebViewEvent::ExtensionOpenPopup {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            url: "https://example.org/popup".to_owned(),
        });
        session.poll();
        let note: serde_json::Value = serde_json::from_str(&session.notes()[0]).expect("a note");
        assert_eq!(note["note"], "open");
        assert_eq!(note["url"], "https://example.org/popup");
        assert_eq!(note["popup"], true, "a popup is shown as a page of its own");
    }

    /// An extension's notification is kept while it shows and gone when the extension takes it
    /// back, and the person's press on one of its buttons reaches the engine by number.
    #[test]
    fn an_extensions_notification_is_kept_until_the_extension_takes_it_back() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        test.push_event(WebViewEvent::ExtensionNotification {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            notification: "n1".to_owned(),
            notice: Some(tilo_webview::ExtensionNotice {
                title: "Saved".to_owned(),
                message: "Three pages kept".to_owned(),
                context: String::new(),
                items: Vec::new(),
                buttons: vec!["Open".to_owned(), "Later".to_owned()],
                progress: None,
                silent: false,
            }),
        });
        session.poll();
        let notices = &session.asks().extension_notices;
        assert_eq!(notices.len(), 1, "one notification: {notices:?}");
        assert_eq!(notices[0].title, "Saved");
        assert_eq!(notices[0].buttons, ["Open", "Later"]);

        test.push_event(WebViewEvent::ExtensionNotification {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            notification: "n1".to_owned(),
            notice: None,
        });
        session.poll();
        assert!(
            session.asks().extension_notices.is_empty(),
            "the extension took it back"
        );
    }

    /// A page a script opened (a popup the site sized, a plain new tab) is a note the window
    /// hands the app, never the page's own event.
    #[test]
    fn pages_a_script_opens_are_notes_for_the_app() {
        let test = TestEngine::new();
        let engine: SharedEngine = test.clone();
        let mut session = PageSession::new(engine.clone());
        test.push_new_tab_request("https://example.org/new");
        // The page holds a popup for a window that asked for them, which is what the session
        // says at each poll.
        engine.accept_popups();
        test.push_popup(9, "https://example.org/sign-in", 420, 640);
        session.poll();
        let notes = session.notes();
        assert_eq!(notes.len(), 2, "a new tab and a popup: {notes:?}");
        let parsed: Vec<serde_json::Value> = notes
            .iter()
            .map(|note| serde_json::from_str(note).expect("a note is JSON"))
            .collect();
        assert_eq!(parsed[0]["note"], "open");
        assert_eq!(parsed[0]["url"], "https://example.org/new");
        assert_eq!(parsed[0]["popup"], false);
        assert_eq!(parsed[1]["url"], "https://example.org/sign-in");
        assert_eq!(parsed[1]["popup"], true);
    }

    /// An action nobody waits for any more is dropped, and the page is told to stop it.
    #[test]
    fn a_cancelled_action_holds_neither_the_page_nor_an_answer() {
        let engine: SharedEngine = TestEngine::new();
        let mut session = PageSession::new(engine.clone());
        let action = session.ask(
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        );
        session.cancel(action);
        assert!(!session.waiting_for(action));
        session.poll();
        assert!(
            session.answers().is_empty(),
            "a cancelled action has no answer"
        );
    }
}
