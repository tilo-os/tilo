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

pub use asks::{Asks, Download, Menu};
pub use prompts::{Prompt, PromptChange, PromptKey, PromptKind, Prompts};

use std::collections::{BTreeSet, HashMap, VecDeque};

use tilo_webview::{
    FileChooser, PageActor, PageFailure, PageReply, PageRequest, SharedEngine, WebViewCommand,
    WebViewEvent,
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
        }
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
                    let step = if *total > 0 {
                        received.saturating_mul(10) / total
                    } else {
                        0
                    };
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
            || !self.notes.is_empty()
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

    /// A page a script opened (a popup the site sized, a plain new tab) is a note the window
    /// hands the app, never the page's own event.
    #[test]
    fn pages_a_script_opens_are_notes_for_the_app() {
        let test = TestEngine::new();
        let engine: SharedEngine = test.clone();
        let mut session = PageSession::new(engine);
        test.push_new_tab_request("https://example.org/new");
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
