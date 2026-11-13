//! What this window holds before the page sends it.
//!
//! A window an agent operates holds every departure: a form submission however it was
//! triggered, a navigation or request with a method of its own, and a download. The
//! page stays as it was filled until the person allows that one request.
//!
//! A page action whose run reached the departure answers that it needs the person; their
//! allow runs the same invocation again, which releases the hold instead of running the
//! action twice. A departure nothing called for is published on `browser.page` and
//! answered there. A hold nobody answers is dropped after [`HELD_WAIT`].

use ui::tilo_actions::InvocationId;
use ui::webview::HeldRequest;

use super::{BrowserWidget, WebViewCommand, action_failed};

pub(crate) const HELD_WAIT: std::time::Duration = std::time::Duration::from_secs(2 * 60);
/// A page that posts in a loop meets the same bound as one that opens tabs.
const MAX_HELD: usize = 8;
const MAX_SHOWN_FILE: usize = 120;
/// What a page action answers when its run reached something that waits for the person:
/// the code [`DoorError::ConfirmationRequired`] carries, so their allow runs it again.
///
/// [`DoorError::ConfirmationRequired`]: ui::tilo_actions::DoorError::ConfirmationRequired
pub(crate) const NEEDS_PERSON: &str = ui::tilo_actions::remote::CONFIRMATION_REQUIRED_CODE;

#[derive(Clone)]
pub(crate) struct Held {
    pub(crate) request: HeldRequest,
    /// The page it leaves from, by its origin.
    page: String,
    /// The filled page as a picture, by its content address, when the window kept one.
    picture: Option<String>,
    /// The file a download would save, when the engine named it.
    file: Option<String>,
    /// The page action whose run reached it, so the person's allow runs it again.
    asked: Option<InvocationId>,
    releasing: Option<InvocationId>,
    until: std::time::Instant,
    pub(crate) allowed: bool,
}

impl Held {
    pub(crate) fn describing(&self) -> String {
        match &self.file {
            Some(file) => format!("{} ({file})", self.request.describing()),
            None => self.request.describing(),
        }
    }

    pub(crate) fn facts(&self) -> Vec<(&'static str, String)> {
        facts(&self.page, &self.request, self.picture.as_deref())
    }

    pub(crate) const fn answered(&self) -> bool {
        self.allowed || self.releasing.is_some()
    }
}

#[derive(Default)]
pub(crate) struct Holds {
    held: Vec<Held>,
}

impl Holds {
    /// Hold `request`, or say it is already held: the engine reports one request once.
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

    pub(crate) fn asked_by(&mut self, id: u32, invocation: InvocationId) {
        if let Some(held) = self.held.iter_mut().find(|held| held.request.id == id) {
            held.asked = Some(invocation);
        }
    }

    /// Take the hold `invocation`'s run reached, to release it; the page action it named
    /// is not run a second time.
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

    /// The hold the person answered themselves; `None` when it is not waiting. It stays
    /// until the engine says it is over, so the answer names what it was for.
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

    pub(crate) fn closed(&mut self, id: u32) -> Option<Held> {
        let found = self.held.iter().position(|held| held.request.id == id)?;
        Some(self.held.remove(found))
    }

    /// Drop the holds nobody answered by `now`, at their own deadline.
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

    /// The requests no page action's run reached, oldest first: the ones the window
    /// publishes and answers from there. A hold a run reached is not among them, so
    /// nobody is asked about one departure twice.
    pub(crate) fn waiting(&self) -> impl Iterator<Item = &Held> {
        self.held
            .iter()
            .filter(|held| !held.answered() && held.asked.is_none())
    }
}

impl BrowserWidget {
    /// What the person is asked about `request`, whose run reached it: the answer states
    /// the departure it holds, so the host asks them what leaves rather than a label for
    /// it.
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
            // Nothing called for it: the page posted on its own, and the window
            // publishes the hold for whoever may ask it.
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

    pub(crate) fn held_page(&self) -> String {
        origin_of(&self.current_uri)
    }

    /// Send one held request on its way (`allow`) or drop it, and answer the invocation
    /// that waited once the engine says it is over.
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

    /// Let `id` leave, or drop it and leave the page as it was.
    pub(crate) fn release_held(&self, id: u32, kind: ui::webview::HeldKind, allow: bool) {
        if kind == ui::webview::HeldKind::Download {
            // A download the page offers is the engine's own question, answered as a
            // click on the offer is.
            self.engine
                .send(WebViewCommand::DownloadDecision { id, allow });
        } else {
            self.engine
                .send(WebViewCommand::AnswerHeldRequest { id, allow });
        }
    }

    /// The engine says `id` is over: the invocation that waited is answered with what
    /// left.
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

    fn kept_hold_picture(&self, request: &HeldRequest) -> Option<String> {
        let picture = request.capture.as_deref()?;
        Self::kept_picture(picture, self.content.as_ref()?).ok()
    }

    /// A download the page started in a window an agent operates waits for the person;
    /// its answer is the download's own.
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

/// The engine's name for what a download would save as the question shows it: `None` when
/// it named none, and cut to what the question can show.
fn shown_file(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.chars().take(MAX_SHOWN_FILE).collect())
}

/// Its scheme and host, or the address itself when it is no web address.
fn origin_of(url: &str) -> String {
    crate::site_controls::origin(url).unwrap_or_else(|| url.to_owned())
}

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

/// [`facts`] as the facets a departure is stated under wherever it leaves this window.
fn departure_facets(facts: &[(&'static str, String)]) -> Vec<ui::tilo_actions::ActionFacet> {
    facts
        .iter()
        .map(|(name, value)| {
            ui::tilo_actions::remote::browser::departure_facet(name, value.clone())
        })
        .collect()
}
