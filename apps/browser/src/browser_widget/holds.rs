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

use super::{BrowserWidget, WebViewCommand, action_failed};

/// How long a held request waits for the person before the window drops it.
pub(crate) const HELD_WAIT: std::time::Duration = std::time::Duration::from_secs(2 * 60);
/// The most held requests one window waits on at once: a page that posts in a
/// loop meets the same bound as one that opens tabs.
const MAX_HELD: usize = 8;
/// The most of a download's file name the person's question shows.
const MAX_SHOWN_FILE: usize = 120;
/// What a page action answers when what its run reached waits for the person:
/// the code [`DoorError::ConfirmationRequired`] carries, so the caller's
/// invocation is refused for want of their answer and their allow runs it
/// again.
///
/// [`DoorError::ConfirmationRequired`]: ui::tilo_actions::DoorError::ConfirmationRequired
pub(crate) const NEEDS_PERSON: &str = ui::tilo_actions::remote::CONFIRMATION_REQUIRED_CODE;

/// One request the page is not allowed to send yet.
#[derive(Clone)]
pub(crate) struct Held {
    pub(crate) request: HeldRequest,
    /// The page it leaves from, by its origin: what the person is told they
    /// are on, where the page itself is not the destination.
    page: String,
    /// The filled page as a picture, by its content address, when this window
    /// kept one: what the person is shown before they allow it.
    picture: Option<String>,
    /// The file a download would save, when the engine named it: the person
    /// is told which one they allow.
    file: Option<String>,
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
    /// What a person is told is waiting: what leaves, where, and which file.
    pub(crate) fn describing(&self) -> String {
        match &self.file {
            Some(file) => format!("{} ({file})", self.request.describing()),
            None => self.request.describing(),
        }
    }

    /// The departure as the person's question carries it, by the names a
    /// reader reads it by: the page it leaves from, what it does, the method
    /// and where it goes, and — when this window kept one — the picture of the
    /// filled page, by its content address.
    pub(crate) fn facts(&self) -> Vec<(&'static str, String)> {
        facts(&self.page, &self.request, self.picture.as_deref())
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
    /// request once. `page` is the origin the page it leaves from is on, and
    /// `picture` the filled page kept as content, when the window kept one.
    pub(crate) fn hold(
        &mut self,
        request: HeldRequest,
        page: String,
        picture: Option<String>,
        file: Option<String>,
    ) -> Option<u32> {
        if !request.valid() || self.held.iter().any(|held| held.request.id == request.id) {
            return None;
        }
        if self.held.len() >= MAX_HELD {
            return None;
        }
        let id = request.id;
        self.held.push(Held {
            request,
            page,
            picture,
            file,
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
        let held = self.held.get_mut(found)?;
        held.allowed = allow;
        let answer = held.clone();
        if answer.request.kind == ui::webview::HeldKind::Download {
            self.held.remove(found);
        }
        Some(answer)
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

    /// The requests nobody's page action reached, oldest first: the ones the
    /// window publishes, and answers from there. A hold a page action's run
    /// reached is not among them — its caller is told that it needs the
    /// person, and their allow runs that same invocation again — so nobody is
    /// asked about one departure twice.
    pub(crate) fn waiting(&self) -> impl Iterator<Item = &Held> {
        self.held
            .iter()
            .filter(|held| !held.answered() && held.asked.is_none())
    }
}

impl BrowserWidget {
    /// What the person is asked about `request`, whose run reached it: the
    /// action answers that it needs them — stating the departure it holds, so
    /// the host holding their window asks them what leaves rather than a label
    /// for it — and their allow releases it.
    pub(crate) fn hold_asked(&mut self, request: &HeldRequest) {
        let picture = self.kept_hold_picture(request);
        let page = self.held_page();
        let departure = departure_facets(&facts(&page, request, picture.as_deref()));
        if self
            .holds
            .hold(request.clone(), page, picture, None)
            .is_none()
        {
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
            serde_json::to_value(departure).ok(),
        ));
        self.structure_dirty = true;
    }

    /// The page this window shows, by its origin: what a hold's question
    /// names as the page it leaves from.
    pub(crate) fn held_page(&self) -> String {
        origin_of(&self.current_uri)
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
    pub(crate) fn release_held(&self, id: u32, kind: ui::webview::HeldKind, allow: bool) {
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

    /// The picture `request` carries of the filled page, kept as this
    /// window's content so the person's question can name it: `None` when the
    /// engine painted none, or this window has nowhere to keep one.
    fn kept_hold_picture(&self, request: &HeldRequest) -> Option<String> {
        let picture = request.capture.as_deref()?;
        Self::kept_picture(picture, self.content.as_ref()?).ok()
    }

    /// A download the page started in a window an agent operates waits for the
    /// person like every other departure: the engine holds it, and its answer
    /// is the download's own.
    pub(crate) fn hold_download(&mut self, id: u32, url: &str, name: &str) -> bool {
        let mut request = HeldRequest::new(id, ui::webview::HeldKind::Download);
        request.method = "GET".into();
        request.origin = origin_of(url);
        if self
            .holds
            .hold(request, self.held_page(), None, shown_file(name))
            .is_none()
        {
            return false;
        }
        self.structure_dirty = true;
        true
    }
}

/// The engine's name for what a download would save, as the question shows
/// it: `None` when it named none, and cut to what the question can show.
fn shown_file(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.chars().take(MAX_SHOWN_FILE).collect())
}

/// The origin of `url`, as a person reads it: its scheme and host, and the
/// address itself when it is no web address.
fn origin_of(url: &str) -> String {
    crate::site_controls::origin(url).unwrap_or_else(|| url.to_owned())
}

/// The parts of `request`'s departure from `page`, by the names a person's
/// question reads them by: the page it leaves from, what it does, the method
/// and where it goes, and — when this window kept one — the picture of the
/// filled page.
fn facts(page: &str, request: &HeldRequest, picture: Option<&str>) -> Vec<(&'static str, String)> {
    let mut facts = vec![
        ("page", page.to_owned()),
        ("kind", request.kind.as_str().to_owned()),
        ("method", request.method.clone()),
        ("destination", request.origin.clone()),
    ];
    if let Some(picture) = picture {
        facts.push(("picture", picture.to_owned()));
    }
    facts
}

/// [`facts`] as the departure facets they are stated under wherever a
/// departure leaves this window: on the hold it publishes, and in the answer a
/// page action's run gives when what it reached waits for the person.
fn departure_facets(facts: &[(&'static str, String)]) -> Vec<ui::tilo_actions::ActionFacet> {
    facts
        .iter()
        .map(|(name, value)| {
            ui::tilo_actions::remote::browser::departure_facet(name, value.clone())
        })
        .collect()
}
