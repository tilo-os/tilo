//! The page this window shows: one engine, one reader of its events, and the answers to the
//! app's page calls.
//!
//! An app's actions run in another process from the window that holds the engine. A page call
//! arrives as `ToUi::Page` carrying the request as JSON, the window asks its page
//! ([`tilo_page_engine`]), and the answer goes back as `ToApp::PageAnswer`. The same session
//! feeds the view its page's events, so the engine's one event stream is read exactly once.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_page_engine::{PageSession, codec};
use tilo_webview::SharedEngine;
use ui::webview;

/// The page of one window, made when the window first needs it.
pub type Page = Rc<RefCell<Option<PageSession>>>;

/// The engine of `page`, made now when the window has none: the window's one page.
#[must_use]
pub fn engine_of(page: &Page) -> SharedEngine {
    let mut held = page.borrow_mut();
    if let Some(session) = held.as_ref() {
        session.engine().clone()
    } else {
        let engine = webview::new_page_engine();
        held.replace(PageSession::new(engine.clone()));
        engine
    }
}

/// What asking the page left to do.
pub enum Asked {
    /// The operation answered at once (a navigation, a zoom): what the app reads.
    Answered(String),
    /// The page was asked; its answer comes with the action `action`.
    Waiting(u32),
}

/// Ask the page `request` (JSON an app's action wrote) to do what it says.
///
/// # Errors
/// The request is no JSON the page reads, or the window keeps no page.
pub fn ask(page: &Page, request: &str) -> Result<Asked, String> {
    let engine = engine_of(page);
    match codec::decode(request)? {
        codec::Ask::Command(command) => {
            engine.send(command);
            Ok(Asked::Answered(codec::accepted()))
        }
        codec::Ask::Asks => {
            let mut held = page.borrow_mut();
            let session = held
                .as_mut()
                .ok_or_else(|| "this window has no page".to_owned())?;
            session.poll();
            Ok(Asked::Answered(codec::asks_json(session.asks())))
        }
        codec::Ask::Page(request) => {
            let mut held = page.borrow_mut();
            let session = held
                .as_mut()
                .ok_or_else(|| "this window has no page".to_owned())?;
            Ok(Asked::Waiting(session.ask(None, request)))
        }
    }
}

/// Take the answers the page has given since the window last looked, oldest first.
#[must_use]
pub fn answers(page: &Page) -> Vec<(u32, String)> {
    let mut held = page.borrow_mut();
    let Some(session) = held.as_mut() else {
        return Vec::new();
    };
    session.poll();
    session
        .answers()
        .into_iter()
        .map(|(action, answer)| (action, codec::encode(&answer)))
        .collect()
}
