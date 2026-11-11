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
//!
//! Every departure the engine reports is answered. The window asks the person
//! about [`MAX_HELD`] at once and no more: one beyond them is turned away at
//! once, the engine drops it, and the action whose run reached it is told why.
//! Refusing it at once, and not making it wait its turn behind the others,
//! keeps what is paused bounded for a page that posts in a loop, never has a
//! request wait on questions the person has not even seen, and tells the
//! caller the truth while it can still act on it. The engine ends a hold no
//! app answers at a deadline of its own, a little after this window's.
//!
//! The tab's record says all of it ([`crate::page_record`]). The engine sets
//! down that a departure was held, under the actor whose action reached it,
//! and how it ended: let leave, kept, or dropped unanswered. It cannot know
//! who answered, so each answer the window sends says ([`AnsweredBy`]):
//!
//! - the person, when their allow ran again the invocation that reached the
//!   hold. That invocation is its actor's, whom the held entry already names,
//!   and the action door runs it a second time only on the person's own
//!   answer, so the one who let it leave is the person;
//! - whoever the action door admitted `send-held-request` or
//!   `keep-held-request` for, by the name the tab keeps their page actions
//!   under, and the person at the window when it names nobody;
//! - nobody, when the window dropped it at its own deadline.

use ui::tilo_actions::{ActionInvoker, InvocationId};
use ui::webview::{AnsweredBy, HeldRequest, PageActor};

use super::{BrowserWidget, WebViewCommand, action_failed};

/// How long a held request waits for the person before the window drops it.
pub(crate) const HELD_WAIT: std::time::Duration = std::time::Duration::from_secs(2 * 60);
/// The most held requests one window asks the person about at once: a page
/// that posts in a loop meets the same bound as one that opens tabs. One
/// more is turned away at once.
const MAX_HELD: usize = 8;
/// What a page action answers when what its run reached was turned away as
/// one request too many.
pub(crate) const TOO_MANY: &str = "browser.page.too_many_held";
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

    /// Whether the person answered, rather than it waiting.
    pub(crate) const fn answered(&self) -> bool {
        self.allowed || self.releasing.is_some()
    }
}

/// What the window did with a request the engine reported holding.
pub(crate) enum Taken {
    /// It waits for the person.
    Held,
    /// Nothing: the window holds it already, or it is no request the engine
    /// could have reported.
    Known,
    /// It turns it away: as many as it asks the person about already wait.
    TooMany,
}

/// The requests this window holds, oldest first.
#[derive(Default)]
pub(crate) struct Holds {
    held: Vec<Held>,
    /// What the invocations that released a hold are answered, once the
    /// engine said their departure is over, until each is read. They are not
    /// the tab's one answer: another actor's action may hold the tab by then.
    answers: Vec<ui::tilo_actions::InvocationUpdate>,
}

impl Holds {
    /// Hold `request`, or say why not: the engine reports one request once,
    /// and the window asks the person about [`MAX_HELD`] at once.
    pub(crate) fn hold(&mut self, request: HeldRequest, file: Option<String>) -> Taken {
        if !request.valid() || self.held.iter().any(|held| held.request.id == request.id) {
            return Taken::Known;
        }
        let waiting = self.held.iter().filter(|held| !held.answered()).count();
        if waiting >= MAX_HELD {
            return Taken::TooMany;
        }
        self.held.push(Held {
            request,
            file,
            asked: None,
            releasing: None,
            until: std::time::Instant::now() + HELD_WAIT,
            allowed: false,
        });
        Taken::Held
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

    /// Keep `answer` for the invocation that released a hold, until it is
    /// read. At most as many wait as holds do: the oldest goes first.
    pub(crate) fn answer(&mut self, answer: ui::tilo_actions::InvocationUpdate) {
        if self.answers.len() >= MAX_HELD {
            self.answers.remove(0);
        }
        self.answers.push(answer);
    }

    /// What `invocation` is answered for the hold it released, once.
    pub(crate) fn take_answer(
        &mut self,
        invocation: InvocationId,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        let found = self
            .answers
            .iter()
            .position(|answer| answer.invocation() == invocation)?;
        Some(self.answers.remove(found))
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
    /// action answers that it needs them, and their allow releases it.
    pub(crate) fn hold_asked(&mut self, request: &HeldRequest) {
        match self.holds.hold(request.clone(), None) {
            Taken::Held => {}
            Taken::Known => return,
            Taken::TooMany => return self.turn_away(request),
        }
        self.structure_dirty = true;
        let Some(operation) = self.page_operation.take() else {
            // Nothing called for it: the page posted on its own, and the
            // window publishes the hold for whoever may ask it.
            return;
        };
        self.note_text_entered(&operation.acted);
        // A form's action that reached it answered nothing of its form: the
        // person's allow releases the departure, and no answer is to come.
        self.forms.take_run(operation.invocation);
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

    /// Turn `request` away at once: as many requests as the window asks the
    /// person about already wait. The engine drops it, so nothing of it
    /// leaves and nothing stays paused, and the action whose run reached it
    /// is told why, instead of being left to think the page took it.
    fn turn_away(&mut self, request: &HeldRequest) {
        self.release_held(request.id, request.kind, false, AnsweredBy::TooMany);
        let Some(operation) = self.page_operation.take() else {
            return;
        };
        self.note_text_entered(&operation.acted);
        self.forms.take_run(operation.invocation);
        self.page_operation_result = Some(action_failed(
            operation.invocation,
            TOO_MANY,
            format!(
                "{}: {MAX_HELD} requests of this page already wait for the person, so this one \
                 was dropped and nothing left; answer those first",
                request.describing()
            ),
            Some(serde_json::json!({
                "kind": request.kind.as_str(),
                "method": request.method,
                "origin": request.origin,
                "waiting": MAX_HELD,
            })),
        ));
        self.structure_dirty = true;
    }

    /// Who answers a hold when the action door admitted the answer for
    /// `invoker`: that actor, by the name the tab keeps their page actions
    /// under, and the person at the window for nobody. The engine sets the
    /// answer down under that name, so the tab's record is told whose name
    /// it is.
    pub(crate) fn answering(&mut self, invoker: Option<&ActionInvoker>) -> AnsweredBy {
        let actor = invoker.and_then(|invoker| PageActor::named(&invoker.actor));
        self.page_record.acts_as(actor.as_ref(), invoker);
        AnsweredBy::admitted(actor)
    }

    /// Send one held request on its way (`allow`) or drop it, as `invoker`
    /// (nobody: the person at the window), and answer the invocation that
    /// waited for the release once the engine says it is over.
    pub(crate) fn answer_hold(
        &mut self,
        id: &str,
        allow: bool,
        invoker: Option<&ActionInvoker>,
    ) -> Result<serde_json::Value, String> {
        let id: u32 = id.parse().map_err(|_| {
            "a held request is named by the id the page's request carries".to_string()
        })?;
        let Some(held) = self.holds.answered(id, allow) else {
            return Err("the page is not waiting on that any more".into());
        };
        let by = self.answering(invoker);
        self.release_held(held.request.id, held.request.kind, allow, by);
        self.structure_dirty = true;
        Ok(serde_json::json!({
            "kind": held.request.kind.as_str(),
            "method": held.request.method,
            "origin": held.request.origin,
            "allowed": allow,
        }))
    }

    /// Let `id` leave, or cancel it: the engine resumes it exactly once, or
    /// drops it and leaves the page as it was. `by` is who answered, which
    /// the engine sets down in the tab's record.
    pub(crate) fn release_held(
        &self,
        id: u32,
        kind: ui::webview::HeldKind,
        allow: bool,
        by: AnsweredBy,
    ) {
        if kind == ui::webview::HeldKind::Download {
            // A download the page offers is the engine's own question, and
            // its answer is the one a person's click on the offer gives.
            self.engine
                .send(WebViewCommand::DownloadDecision { id, allow, by });
        } else {
            self.engine
                .send(WebViewCommand::AnswerHeldRequest { id, allow, by });
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
        let answer = if held.allowed {
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
        };
        // Several actors work in one tab: the tab's own answer may be
        // another's by now, so this one waits beside it.
        self.holds.answer(answer);
    }

    /// Drop the holds nobody answered: the page stays as it was filled.
    pub(crate) fn expire_holds(&mut self) {
        self.expire_holds_at(std::time::Instant::now());
    }

    /// Drop the holds nobody answered by `now`, and say that nobody did.
    pub(crate) fn expire_holds_at(&mut self, now: std::time::Instant) {
        for held in self.holds.expire(now) {
            self.release_held(
                held.request.id,
                held.request.kind,
                false,
                AnsweredBy::Nobody,
            );
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
        match self.holds.hold(request, shown_file(name)) {
            Taken::Held => {
                self.structure_dirty = true;
                true
            }
            Taken::Known => false,
            Taken::TooMany => {
                // One more than the person is asked about: discarded at once.
                let kind = ui::webview::HeldKind::Download;
                self.release_held(id, kind, false, AnsweredBy::TooMany);
                false
            }
        }
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
