//! The page a window shows, one request at a time, with no UI of its own.
//!
//! A window holds one engine and one [`PageSession`]: the session asks the engine to do what the
//! page's actions say (`PageRequest`), keeps the answers until the frame that carries them back
//! to whoever asked, and takes every other event the engine reports into a queue the view
//! reads. The engine's events have one reader — this session — so a page's navigation and its
//! answers never take each other's.

use std::collections::{BTreeMap, VecDeque};

use tilo_webview::{
    PageActor, PageFailure, PageReply, PageRequest, SharedEngine, WebViewCommand, WebViewEvent,
};

/// One app window's page: its engine, the actions waiting for an answer, and the events its
/// view shows.
pub struct PageSession {
    engine: SharedEngine,
    /// The id of the last page action asked about.
    next: u32,
    /// The actions whose answers have not come, by their id.
    waiting: BTreeMap<u32, ()>,
    /// The answers that came and wait to be carried to whoever asked.
    answers: Vec<(u32, Result<PageReply, PageFailure>)>,
    /// The engine's other events, in the order they came, for the view to show.
    events: VecDeque<WebViewEvent>,
}

impl PageSession {
    #[must_use]
    pub fn new(engine: SharedEngine) -> Self {
        Self {
            engine,
            next: 0,
            waiting: BTreeMap::new(),
            answers: Vec::new(),
            events: VecDeque::new(),
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
        self.waiting.insert(request_id, ());
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
        if self.waiting.remove(&action).is_some() {
            self.engine.send(WebViewCommand::CancelPage {
                request_id: action,
            });
        }
    }

    /// Take what the engine reported: the answers to the actions asked, and the page's other
    /// events for the view. A window calls this once a frame, before it draws.
    pub fn poll(&mut self) {
        for event in self.engine.drain_events() {
            match event {
                WebViewEvent::PageResult { request_id, result } => {
                    // An answer to an action cancelled meanwhile is dropped: nobody waits for
                    // it, and the page settled it as asked.
                    if self.waiting.remove(&request_id).is_some() {
                        self.answers.push((request_id, result));
                    }
                }
                other => self.events.push_back(other),
            }
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
        self.waiting.contains_key(&action)
    }

    /// Whether the page has something for the window: an answer to carry, or an event to show.
    #[must_use]
    pub fn busy(&self) -> bool {
        !self.waiting.is_empty() || !self.answers.is_empty() || !self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_webview::test_engine::TestEngine;

    /// A page's outline is asked once and answered once, and the page's own events stay theirs.
    #[test]
    fn an_answer_comes_once_and_the_pages_events_reach_its_view() {
        let engine: SharedEngine = TestEngine::new();
        let mut session = PageSession::new(engine.clone());
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
        assert!(answers[0].1.is_ok(), "the outline answers: {:?}", answers[0].1);
        assert!(!session.waiting_for(action), "the action is answered");

        // What the page reports of itself is the view's, not an answer's: it waits in order.
        engine.push_event(WebViewEvent::UriChanged("https://example.org/".to_owned()));
        session.poll();
        assert!(matches!(
            session.next_event(),
            Some(WebViewEvent::UriChanged(uri)) if uri == "https://example.org/"
        ));
        assert!(session.next_event().is_none());
        assert!(session.answers().is_empty(), "events are not answers");
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
        assert!(session.answers().is_empty(), "a cancelled action has no answer");
    }
}
