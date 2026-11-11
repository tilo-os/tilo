//! What this window holds before the page sends it.
//!
//! A window an agent operates holds every departure: a form submission
//! however it was triggered, a navigation or request with a method of its
//! own, and a download. The page stays as it was filled — nothing leaves —
//! until the person allows that one request:
//!
//! - The page action whose run reached the departure answers that it needs
//!   the person, naming what is held ([`HeldRequest::describing`]). Their
//!   allow runs the same invocation again, which releases the held request
//!   instead of running the page action a second time; denied or unanswered,
//!   the window drops it and the page stays.
//! - A departure nothing called for (a page posting on its own) is a held
//!   request the window publishes: it is on `browser.page` for anyone who may
//!   ask the page, and answered there with `send-held-request` or
//!   `keep-held-request`.
//!
//! A hold nobody answers is dropped after [`HELD_WAIT`]: the page is left as
//! it was, and the window is never left waiting on a person who did not come.

use ui::tilo_actions::InvocationId;
use ui::webview::HeldRequest;

use super::*;

/// How long a held request waits for the person before the window drops it.
pub(crate) const HELD_WAIT: std::time::Duration = std::time::Duration::from_secs(2 * 60);
/// The most held requests one window waits on at once: a page that posts in a
/// loop meets the same bound as one that opens tabs.
const MAX_HELD: usize = 8;
/// What a page action answers when what its run reached waits for the person:
/// the code [`DoorError::ConfirmationRequired`] carries, so the caller's
/// invocation is refused for want of their answer and their allow runs it
/// again.
///
/// [`DoorError::ConfirmationRequired`]: ui::tilo_actions::DoorError::ConfirmationRequired
pub(crate) const NEEDS_PERSON: &str = ui::tilo_actions::remote::CONFIRMATION_REQUIRED_CODE;

/// One request the page is not allowed to send yet.
pub(crate) struct Held {
    pub(crate) request: HeldRequest,
    /// The page action whose run reached it, if one was in flight: that same
    /// invocation, run again with the person's answer, releases it.
    asked: Option<InvocationId>,
    /// The invocation releasing it, answered once the engine says it is over.
    releasing: Option<InvocationId>,
    /// When the window drops it if nobody answers.
    until: std::time::Instant,
    /// Whether the person allowed it, rather than the hold being dropped.
    pub(crate) allowed: bool,
}

impl Held {
    /// What a person is told is waiting: what leaves, and where.
    pub(crate) fn describing(&self) -> String {
        self.request.describing()
    }

    /// Whether the person answered, rather than it waiting.
    pub(crate) const fn answered(&self) -> bool {
        self.allowed || self.releasing.is_some()
    }
}

/// The requests this window holds, oldest first.
#[derive(Default)]
pub(crate) struct Holds {
    held: Vec<Held>,
}

impl Holds {
    /// Hold `request`, or say it is already held: the engine reports one
    /// request once.
    pub(crate) fn hold(&mut self, request: HeldRequest) -> Option<u32> {
        if !request.valid() || self.held.iter().any(|held| held.request.id == request.id) {
            return None;
        }
        if self.held.len() >= MAX_HELD {
            return None;
        }
        let id = request.id;
        self.held.push(Held {
            request,
            asked: None,
            releasing: None,
            until: std::time::Instant::now() + HELD_WAIT,
            allowed: false,
        });
        Some(id)
    }

    /// The hold `invocation`'s run reached: its answer to its caller names it,
    /// and its allow releases it.
    pub(crate) fn asked_by(&mut self, id: u32, invocation: InvocationId) {
        if let Some(held) = self.held.iter_mut().find(|held| held.request.id == id) {
            held.asked = Some(invocation);
        }
    }

    /// Take the hold `invocation`'s run reached, to release it: the same
    /// invocation is run again because the person allowed it, and the page
    /// action it named is not run a second time.
    pub(crate) fn release_for(
        &mut self,
        invocation: InvocationId,
    ) -> Option<(u32, ui::webview::HeldKind)> {
        let held = self
            .held
            .iter_mut()
            .find(|held| held.asked == Some(invocation) && held.releasing.is_none())?;
        if held.allowed {
            return None;
        }
        held.releasing = Some(invocation);
        held.allowed = true;
        Some((held.request.id, held.request.kind))
    }

    /// The hold the person answered themselves. `None` when it is not waiting.
    /// It stays until the engine says it is over, so the answer names what
    /// it was for; a download's outcome is the download's own, and nothing
    /// closes it here.
    pub(crate) fn answered(&mut self, id: u32, allow: bool) -> Option<Held> {
        let found = self
            .held
            .iter()
            .position(|held| held.request.id == id && !held.answered())?;
        self.held[found].allowed = allow;
        if self.held[found].request.kind == ui::webview::HeldKind::Download {
            return Some(self.held.remove(found));
        }
        let held = &self.held[found];
        Some(Held {
            request: held.request.clone(),
            asked: held.asked,
            releasing: held.releasing,
            until: held.until,
            allowed: allow,
        })
    }

    /// The engine says `id` is over: released, dropped, or its page went.
    pub(crate) fn closed(&mut self, id: u32) -> Option<Held> {
        let found = self.held.iter().position(|held| held.request.id == id)?;
        Some(self.held.remove(found))
    }

    /// Drop the holds nobody answered by `now`: what the window lets go at
    /// its own deadline.
    pub(crate) fn expire(&mut self, now: std::time::Instant) -> Vec<Held> {
        let mut dropped = Vec::new();
        let mut kept = Vec::with_capacity(self.held.len());
        for held in self.held.drain(..) {
            if held.answered() || now < held.until {
                kept.push(held);
            } else {
                dropped.push(held);
            }
        }
        self.held = kept;
        dropped
    }

    /// The requests waiting for the person, oldest first.
    pub(crate) fn waiting(&self) -> impl Iterator<Item = &Held> {
        self.held.iter().filter(|held| !held.answered())
    }
}

impl BrowserWidget {
    /// What the person is asked about `request`, whose run reached it: the
    /// action answers that it needs them, and their allow releases it.
    pub(crate) fn hold_asked(&mut self, request: HeldRequest) {
        if self.holds.hold(request.clone()).is_none() {
            return;
        }
        self.structure_dirty = true;
        let Some(operation) = self.page_operation.take() else {
            // Nothing called for it: the page posted on its own, and the
            // window publishes the hold for whoever may ask it.
            return;
        };
        self.note_text_entered(&operation.acted);
        self.reader_request = None;
        self.holds.asked_by(request.id, operation.invocation);
        self.page_operation_result = Some(action_failed(
            operation.invocation,
            NEEDS_PERSON,
            format!("{}: the person has to allow this", request.describing()),
            Some(serde_json::json!({
                "held": request.id,
                "kind": request.kind.as_str(),
                "method": request.method,
                "origin": request.origin,
            })),
        ));
        self.structure_dirty = true;
    }

    /// Send one held request on its way (`allow`) or drop it, and answer the
    /// invocation that waited for the release once the engine says it is over.
    pub(crate) fn answer_hold(
        &mut self,
        id: &str,
        allow: bool,
    ) -> Result<serde_json::Value, String> {
        let id: u32 = id.parse().map_err(|_| {
            "a held request is named by the id the page's request carries".to_string()
        })?;
        let Some(held) = self.holds.answered(id, allow) else {
            return Err("the page is not waiting on that any more".into());
        };
        self.release_held(held.request.id, held.request.kind, allow);
        self.structure_dirty = true;
        Ok(serde_json::json!({
            "kind": held.request.kind.as_str(),
            "method": held.request.method,
            "origin": held.request.origin,
            "allowed": allow,
        }))
    }

    /// Let `id` leave, or cancel it: the engine resumes it exactly once, or
    /// drops it and leaves the page as it was.
    pub(crate) fn release_held(&mut self, id: u32, kind: ui::webview::HeldKind, allow: bool) {
        if kind == ui::webview::HeldKind::Download {
            // A download the page offers is the engine's own question, and
            // its answer is the one a person's click on the offer gives.
            self.engine
                .send(WebViewCommand::DownloadDecision { id, allow });
        } else {
            self.engine
                .send(WebViewCommand::AnswerHeldRequest { id, allow });
        }
    }

    /// The engine says `id` is over: what the person allowed leaves, and the
    /// invocation that waited for it is answered with what left.
    pub(crate) fn held_closed(&mut self, id: u32) {
        let Some(held) = self.holds.closed(id) else {
            return;
        };
        self.structure_dirty = true;
        let Some(invocation) = held.releasing else {
            return;
        };
        self.page_operation_result = Some(if held.allowed {
            ui::tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: ui::tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "kind": held.request.kind.as_str(),
                        "method": held.request.method,
                        "origin": held.request.origin,
                        "released": true,
                    })),
                    resources: Vec::new(),
                },
            }
        } else {
            action_failed(
                invocation,
                "browser.page.kept",
                format!("{}: the person kept the page as it is", held.describing()),
                None,
            )
        });
    }

    /// Drop the holds nobody answered: the page stays as it was filled.
    pub(crate) fn expire_holds(&mut self) {
        for held in self.holds.expire(std::time::Instant::now()) {
            self.release_held(held.request.id, held.request.kind, false);
            self.structure_dirty = true;
        }
    }

    /// A download the page started in a window an agent operates waits for the
    /// person like every other departure: the engine holds it, and its answer
    /// is the download's own.
    pub(crate) fn hold_download(&mut self, id: u32, url: &str, name: &str) -> bool {
        let mut request = HeldRequest::new(id, ui::webview::HeldKind::Download);
        request.method = "GET".into();
        request.origin = origin_of(url);
        if self.holds.hold(request.clone()).is_none() {
            return false;
        }
        let _ = name;
        self.structure_dirty = true;
        true
    }
}

/// The origin of `url`, as a person reads it: its scheme and host, and the
/// address itself when it is no web address.
fn origin_of(url: &str) -> String {
    crate::site_controls::origin(url).unwrap_or_else(|| url.to_owned())
}
