//! The window reads its page's forms itself.
//!
//! A page's forms are the page's actions once they are known
//! ([`super::form_actions`]). The window does not wait for an actor to read
//! them: when its page has finished loading it reads them once, so a first
//! visit has its actions before any agent looks at the page, and a later
//! visit has what was remembered checked against the page as it is.
//!
//! It reads again only when something says the forms may differ:
//!
//! - the page shows another document: it finished loading, a reload at the
//!   same address included, or it moved to another address without loading;
//! - a form action's answer says the form asks more now, or that the page
//!   does not hold the form the action was made for.
//!
//! Each of those asks for one read and several at once for one, so a page
//! without forms costs one read and nothing reads in a loop. A read nobody
//! answered is not asked again until one of them happens.
//!
//! The read is the one `read-page-forms` makes, sent as the tab's one
//! unnamed actor, as the page's review reads ([`super::page_review_view`]):
//! it takes no place in the tab's record and tells no agent anything. It
//! holds no turn of the tab. The engine runs it in the order it came, like
//! any read, so an action admitted meanwhile goes to the engine at once and
//! waits there no longer than the read takes. An answer for a page the
//! window has left is dropped.
//!
//! What is kept here is which read is with the engine. What the page asks
//! is [`crate::page_forms::PageForms`]'s.

use ui::webview::{PageFailure, PageReply, PageRequest, WebViewCommand};

use super::BrowserWidget;
use crate::site_memory::Place;

/// How long a page that moved to another address without loading is left to
/// draw what it shows there, before its forms are read.
const ROUTE_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);
/// The engine's answer to a read never stays pending longer.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// A read the page is due.
struct Wanted {
    /// Since when.
    at: ui::time::Instant,
    /// The page said it is ready to be read: it finished loading, or one of
    /// its forms changed under an action. Otherwise it only moved, and is
    /// given [`ROUTE_SETTLE`] first.
    ready: bool,
}

/// A read that is with the engine.
struct Asked {
    request_id: u32,
    /// The page it was asked of.
    place: Place,
    deadline: ui::time::Instant,
}

/// The window's own reads of its page's forms.
#[derive(Default)]
pub(super) struct FormsRead {
    /// The page the window last saw in front.
    place: Option<Place>,
    wanted: Option<Wanted>,
    asked: Option<Asked>,
}

impl BrowserWidget {
    /// Whether this window reads its page's forms itself: a window its app
    /// host shows, on a page of the web. A capture's deterministic page and
    /// a window nothing hosts publish to nobody.
    fn reads_own_forms(&self) -> bool {
        !self.headless && self.dispatch.is_some()
    }

    /// The page's forms may differ from what the window knows: it finished
    /// loading, or a form action's answer said so. It is read once more.
    pub(super) fn read_forms_again(&mut self) {
        self.forms_read.wanted = Some(Wanted {
            at: ui::time::now(),
            ready: true,
        });
    }

    /// Read the page's forms when the page is due a read and can answer one.
    pub(super) fn sync_forms_read(&mut self) {
        if !self.reads_own_forms() {
            return;
        }
        let now = ui::time::now();
        let place = if self.engine_failed || self.load_error.is_some() {
            None
        } else {
            Place::of(&self.current_uri)
        };
        if place != self.forms_read.place {
            // The page shows another document: the read of the one it left
            // is dropped, and this one is due a read of its own.
            self.withdraw_forms_read();
            let ready = self
                .forms_read
                .wanted
                .take()
                .is_some_and(|wanted| wanted.ready);
            self.forms_read.wanted = place.is_some().then_some(Wanted { at: now, ready });
            self.forms_read.place.clone_from(&place);
        }
        if self
            .forms_read
            .asked
            .as_ref()
            .is_some_and(|asked| now >= asked.deadline)
        {
            // The engine did not answer. What the page publishes stays as
            // it was, and nothing asks again until the page has a reason.
            self.forms_read.asked = None;
        }
        let Some(place) = place else {
            self.forms_read.wanted = None;
            return;
        };
        let Some(wanted) = &self.forms_read.wanted else {
            return;
        };
        // The page runs nothing while it waits on its question.
        if self.page_dialogs.current().is_some() {
            return;
        }
        // A page that is loading is read once it has loaded, or as it
        // stands when it loads for too long.
        let settled = wanted.ready || now >= wanted.at + ROUTE_SETTLE;
        let loaded = settled && !self.page_loading();
        if !loaded && now < wanted.at + super::PAGE_LOAD_WAIT {
            return;
        }
        // One read at a time: a newer one replaces the one still pending.
        self.withdraw_forms_read();
        self.forms_read.wanted = None;
        self.page_request_seq = self.page_request_seq % i32::MAX.unsigned_abs() + 1;
        self.engine.send(WebViewCommand::Page {
            request_id: self.page_request_seq,
            actor: None,
            request: PageRequest::Forms { frame: None },
        });
        self.forms_read.asked = Some(Asked {
            request_id: self.page_request_seq,
            place,
            deadline: now + READ_TIMEOUT,
        });
    }

    /// Give up on the read that is with the engine: the engine is told
    /// nobody waits for it, and its answer is no longer the window's.
    fn withdraw_forms_read(&mut self) {
        if let Some(asked) = self.forms_read.asked.take() {
            self.engine.send(WebViewCommand::CancelPage {
                request_id: asked.request_id,
            });
        }
    }

    /// Take the engine's answer when `request_id` is the window's own read
    /// of its page's forms. Any other answer is handed back.
    pub(super) fn take_forms_read(
        &mut self,
        request_id: u32,
        result: Result<PageReply, PageFailure>,
    ) -> Option<Result<PageReply, PageFailure>> {
        let asked = self
            .forms_read
            .asked
            .take_if(|asked| asked.request_id == request_id);
        let Some(asked) = asked else {
            return Some(result);
        };
        // A read whose page has gone says nothing of the page in front.
        if Place::of(&self.current_uri).as_ref() != Some(&asked.place) {
            return None;
        }
        // A page that did not answer keeps what it publishes.
        if let Ok(PageReply {
            data: Some(data), ..
        }) = result
        {
            self.heard_own_read(&asked.place, data);
        }
        None
    }

    /// When the window next needs to be awake for its own read: to send the
    /// one the page is due, or to give up on the one the engine holds.
    pub(super) fn forms_read_deadline(&self) -> Option<ui::time::Instant> {
        if !self.reads_own_forms() {
            return None;
        }
        // A page waiting on its question is read once it is answered, which
        // wakes the window by itself.
        let asking = self.page_dialogs.current().is_some();
        let wanted = self.forms_read.wanted.as_ref().filter(|_| !asking);
        let due = wanted.map(|wanted| {
            if wanted.ready || self.page_loading() {
                wanted.at + super::PAGE_LOAD_WAIT
            } else {
                wanted.at + ROUTE_SETTLE
            }
        });
        let pending = self.forms_read.asked.as_ref().map(|asked| asked.deadline);
        due.into_iter().chain(pending).min()
    }
}
