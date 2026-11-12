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
//! A hold also ends with nobody's answer: the engine drops it when the tab
//! shows another page, since what was held has no page to leave from, and at
//! a deadline. Whoever waited is answered. A published hold leaves the page.
//! The action whose run reached a hold that ended without leaving, so or
//! because somebody kept the page as it is, is remembered: an allow that
//! comes after all is told that nothing left ([`HOLD_ENDED`]), and the
//! action is not run a second time on whatever page the tab shows by then.
//!
//! What the page publishes is what waits. A hold somebody answered, to let it
//! leave or to keep the page as it is, is offered for an answer no longer from
//! that moment, though the window keeps it until the engine says the request
//! is over.
//!
//! The engine counts the requests it holds and the downloads its pages offer
//! apart, so a request and a download may carry one number. A hold is named
//! here by which of the two it is with that number ([`HeldKey`]), and what the
//! page publishes for an answer is a number of this window's own, one per
//! hold: an answer or an ending meant for one hold never reaches another.
//!
//! A published hold states what leaves beside its number: the page it leaves
//! from, what the page does, the method and where it goes ([`Held::facts`]),
//! and the filled page as a picture kept in the workspace's content store
//! ([`Held::picture`]), so the person a host asks sees what leaves and not
//! only a name for it. The picture is kept only for a hold that waits: one
//! turned away as one too many is asked about by nobody.
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
use ui::webview::{AnsweredBy, HeldKind, HeldRequest, PageActor};

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
/// What an invocation answers when the person allows it after the hold its
/// run reached ended without leaving.
pub(crate) const HOLD_ENDED: &str = "browser.page.hold_ended";
/// The most of a download's file name the person's question shows.
const MAX_SHOWN_FILE: usize = 120;
/// What a page action answers when what its run reached waits for the person:
/// the code [`DoorError::ConfirmationRequired`] carries, so the caller's
/// invocation is refused for want of their answer and their allow runs it
/// again.
///
/// [`DoorError::ConfirmationRequired`]: ui::tilo_actions::DoorError::ConfirmationRequired
pub(crate) const NEEDS_PERSON: &str = ui::tilo_actions::remote::CONFIRMATION_REQUIRED_CODE;

/// Where a hold stands with whoever may answer it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stand {
    /// Nobody answered yet: it is asked about.
    Waiting,
    /// It was let leave, and the engine has not said yet that it left.
    Allowed,
    /// The page was kept as it is, and the engine has not said yet that the
    /// request is over.
    Kept,
}

/// What the engine names one hold by. It counts the requests it holds and the
/// downloads its pages offer apart, so the number alone names nothing: which
/// of the two a hold is is part of its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeldKey {
    /// A form submission, navigation or request the engine holds, by the id
    /// its report carried and its answer names.
    Request(u32),
    /// A download the page offered, by the engine's id of that download.
    Download(u32),
}

impl HeldKey {
    /// What the engine names `request` by.
    fn of(request: &HeldRequest) -> Self {
        if request.kind == HeldKind::Download {
            Self::Download(request.id)
        } else {
            Self::Request(request.id)
        }
    }
}

/// One request the page is not allowed to send yet.
#[derive(Clone)]
pub(crate) struct Held {
    pub(crate) request: HeldRequest,
    /// What the engine names it by.
    key: HeldKey,
    /// What this window names it by: the number its published answers state.
    /// One number is one hold, whatever the engine calls it.
    number: u32,
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
    /// Where it stands: waiting for an answer, or how it was answered.
    stand: Stand,
}

impl Held {
    /// What a person is told is waiting: what leaves, where, and which file.
    pub(crate) fn describing(&self) -> String {
        match &self.file {
            Some(file) => format!("{} ({file})", self.request.describing()),
            None => self.request.describing(),
        }
    }

    /// The number this window names it by, which its answers state.
    pub(crate) const fn number(&self) -> u32 {
        self.number
    }

    /// The departure as the person's question carries it, by the names a
    /// reader reads it by: the page it leaves from, what it does, the method
    /// and where it goes, and — when this window kept one — the picture of the
    /// filled page, by its content address.
    pub(crate) fn facts(&self) -> Vec<(&'static str, String)> {
        facts(&self.page, &self.request, self.picture.as_deref())
    }

    /// Whether somebody answered it, to let it leave or to keep the page as
    /// it is, rather than it waiting.
    pub(crate) const fn answered(&self) -> bool {
        !matches!(self.stand, Stand::Waiting)
    }

    /// Whether it was let leave.
    const fn allowed(&self) -> bool {
        matches!(self.stand, Stand::Allowed)
    }
}

/// What the window did with a request the engine reported holding.
pub(crate) enum Taken {
    /// It waits for the person, under this number of the window's.
    Held(u32),
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
    /// The number the window gave the hold it took last.
    last: u32,
    /// The invocations whose run reached a hold that ended without leaving,
    /// each with what was held: an allow that comes after all is answered
    /// from here. As many are remembered as holds may wait.
    ended: Vec<(InvocationId, String)>,
    /// What the invocations that released a hold are answered, once the
    /// engine said their departure is over, until each is read. They are not
    /// the tab's one answer: another actor's action may hold the tab by then.
    answers: Vec<ui::tilo_actions::InvocationUpdate>,
}

impl Holds {
    /// Why the window would not hold `request`, when it would not: it holds
    /// it already or it is no request the engine could have reported, or as
    /// many as it asks the person about already wait. `None` for a request it
    /// would take. Asked before anything is kept for a hold, so nothing is
    /// kept for one that will never be asked about.
    pub(crate) fn refuses(&self, request: &HeldRequest) -> Option<Taken> {
        let key = HeldKey::of(request);
        if !request.valid() || self.held.iter().any(|held| held.key == key) {
            return Some(Taken::Known);
        }
        let waiting = self.held.iter().filter(|held| !held.answered()).count();
        (waiting >= MAX_HELD).then_some(Taken::TooMany)
    }

    /// Hold `request`, or say why not: the engine reports one request once,
    /// and the window asks the person about [`MAX_HELD`] at once. `page` is
    /// the origin the page it leaves from is on, and `picture` the filled
    /// page kept as content, when the window kept one.
    pub(crate) fn hold(
        &mut self,
        request: HeldRequest,
        page: String,
        picture: Option<String>,
        file: Option<String>,
    ) -> Taken {
        if let Some(refused) = self.refuses(&request) {
            return refused;
        }
        let key = HeldKey::of(&request);
        // The next number no hold of this window still has.
        let mut number = self.last.checked_add(1).unwrap_or(1);
        while self.held.iter().any(|held| held.number == number) {
            number = number.checked_add(1).unwrap_or(1);
        }
        self.last = number;
        self.held.push(Held {
            request,
            key,
            number,
            page,
            picture,
            file,
            asked: None,
            releasing: None,
            until: std::time::Instant::now() + HELD_WAIT,
            stand: Stand::Waiting,
        });
        Taken::Held(number)
    }

    /// The hold `invocation`'s run reached: its answer to its caller names it,
    /// and its allow releases it.
    pub(crate) fn asked_by(&mut self, key: HeldKey, invocation: InvocationId) {
        if let Some(held) = self.held.iter_mut().find(|held| held.key == key) {
            held.asked = Some(invocation);
        }
    }

    /// Take the hold `invocation`'s run reached, to release it: the same
    /// invocation is run again because the person allowed it, and the page
    /// action it named is not run a second time.
    pub(crate) fn release_for(&mut self, invocation: InvocationId) -> Option<HeldKey> {
        let held = self
            .held
            .iter_mut()
            .find(|held| held.asked == Some(invocation) && held.releasing.is_none())?;
        if held.answered() {
            return None;
        }
        held.releasing = Some(invocation);
        held.stand = Stand::Allowed;
        Some(held.key)
    }

    /// The hold the window numbers `number`, which somebody answered
    /// themselves. `None` when it is not waiting.
    /// It stays until the engine says it is over, so the answer names what
    /// it was for; a download's outcome is the download's own, and nothing
    /// closes it here.
    pub(crate) fn answered(&mut self, number: u32, allow: bool) -> Option<Held> {
        let found = self
            .held
            .iter()
            .position(|held| held.number == number && !held.answered())?;
        let held = self.held.get_mut(found)?;
        held.stand = if allow { Stand::Allowed } else { Stand::Kept };
        let answer = held.clone();
        if matches!(answer.key, HeldKey::Download(_)) {
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

    /// The engine says `key` is over: released, dropped, or its page went.
    pub(crate) fn closed(&mut self, key: HeldKey) -> Option<Held> {
        let found = self.held.iter().position(|held| held.key == key)?;
        let held = self.held.remove(found);
        self.note_ended(&held);
        Some(held)
    }

    /// `held` is over. Unless it was let leave, the invocation whose run
    /// reached it, when one did, is remembered with what was held.
    fn note_ended(&mut self, held: &Held) {
        let Some(invocation) = held.asked.filter(|_| !held.allowed()) else {
            return;
        };
        if self.ended.len() >= MAX_HELD {
            self.ended.remove(0);
        }
        self.ended.push((invocation, held.describing()));
    }

    /// What the hold `invocation`'s run reached was, when that hold ended
    /// without leaving: said once.
    pub(crate) fn take_ended(&mut self, invocation: InvocationId) -> Option<String> {
        let found = self
            .ended
            .iter()
            .position(|(ended, _)| *ended == invocation)?;
        Some(self.ended.remove(found).1)
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
        for held in &dropped {
            self.note_ended(held);
        }
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
        let page = self.held_page();
        // The filled page is kept as content only for a hold that will wait
        // for an answer. One the window holds already, or turns away as one
        // too many, is asked about by nobody: a page that posts in a loop
        // would otherwise fill the workspace's store with pictures no
        // question shows.
        let picture = match self.holds.refuses(request) {
            None => self.kept_hold_picture(request),
            Some(_) => None,
        };
        let mut stated = departure_facets(&facts(&page, request, picture.as_deref()));
        let number = match self
            .holds
            .hold(request.clone(), page.clone(), picture, None)
        {
            Taken::Held(number) => number,
            Taken::Known => return,
            Taken::TooMany => return self.turn_away(request, &page),
        };
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
        self.holds
            .asked_by(HeldKey::of(request), operation.invocation);
        // The number the window holds it under stands beside the departure,
        // not among its parts: it is what answers the hold, and no part of
        // what leaves, so a person asked what leaves is not shown it.
        stated.push(ui::tilo_actions::ActionFacet::new(
            HELD_FACET,
            number.to_string(),
        ));
        self.page_operation_result = Some(action_failed(
            operation.invocation,
            NEEDS_PERSON,
            format!("{}: the person has to allow this", request.describing()),
            serde_json::to_value(stated).ok(),
        ));
        self.structure_dirty = true;
    }

    /// Turn `request`, which the page at `page` was about to send, away at
    /// once: as many requests as the window asks the person about already
    /// wait. The engine drops it, so nothing of it leaves and nothing stays
    /// paused, and the action whose run reached it is told why, instead of
    /// being left to think the page took it.
    fn turn_away(&mut self, request: &HeldRequest, page: &str) {
        self.release_held(HeldKey::of(request), false, AnsweredBy::TooMany);
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
                "page": page,
                "waiting": MAX_HELD,
            })),
        ));
        self.structure_dirty = true;
    }

    /// `invocation` is run again: the person allowed the departure its first
    /// run reached. That departure leaves now, and what reached it, a page
    /// action or a form's own action, is not run a second time. A hold that
    /// ended meanwhile with nobody's answer has nothing left to release: the
    /// invocation is answered that the request was dropped, and the action
    /// is not run again on whatever page the tab shows by now. `None` for an
    /// invocation whose run reached no hold.
    pub(crate) fn release_reached(
        &mut self,
        invocation: InvocationId,
    ) -> Option<ui::framework::Effected> {
        if let Some(held) = self.holds.release_for(invocation) {
            self.release_held(held, true, AnsweredBy::Person);
            return Some(ui::framework::Effected::Pending);
        }
        let what = self.holds.take_ended(invocation)?;
        self.holds.answer(action_failed(
            invocation,
            HOLD_ENDED,
            format!(
                "{what}: it is no longer held and nothing left: its page went away, nobody \
                 answered in time, or the page was kept as it is. Read the page and act again \
                 if it is still wanted"
            ),
            None,
        ));
        Some(ui::framework::Effected::Pending)
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

    /// The page this window shows, by its origin: what a hold's question
    /// names as the page it leaves from.
    pub(crate) fn held_page(&self) -> String {
        origin_of(&self.current_uri)
    }

    /// Send one held request on its way (`allow`) or drop it, as `invoker`
    /// (nobody: the person at the window), and answer the invocation that
    /// waited for the release once the engine says it is over. `id` is the
    /// number the window published the hold under.
    pub(crate) fn answer_hold(
        &mut self,
        id: &str,
        allow: bool,
        invoker: Option<&ActionInvoker>,
    ) -> Result<serde_json::Value, String> {
        let number: u32 = id.parse().map_err(|_| {
            "a held request is named by the number the page publishes it under".to_string()
        })?;
        let Some(held) = self.holds.answered(number, allow) else {
            return Err("the page is not waiting on that any more".into());
        };
        let by = self.answering(invoker);
        self.release_held(held.key, allow, by);
        self.structure_dirty = true;
        Ok(serde_json::json!({
            "kind": held.request.kind.as_str(),
            "method": held.request.method,
            "origin": held.request.origin,
            "allowed": allow,
        }))
    }

    /// Let `key` leave, or cancel it: the engine resumes it exactly once, or
    /// drops it and leaves the page as it was. `by` is who answered, which
    /// the engine sets down in the tab's record.
    pub(crate) fn release_held(&self, key: HeldKey, allow: bool, by: AnsweredBy) {
        self.engine.send(match key {
            // A download the page offers is the engine's own question, and
            // its answer is the one a person's click on the offer gives.
            HeldKey::Download(id) => WebViewCommand::DownloadDecision { id, allow, by },
            HeldKey::Request(id) => WebViewCommand::AnswerHeldRequest { id, allow, by },
        });
    }

    /// The engine says the request `id` is over: what the person allowed
    /// leaves, and the invocation that waited for it is answered with what
    /// left.
    pub(crate) fn held_closed(&mut self, id: u32) {
        let Some(held) = self.holds.closed(HeldKey::Request(id)) else {
            return;
        };
        self.structure_dirty = true;
        let Some(invocation) = held.releasing else {
            return;
        };
        let answer = if held.allowed() {
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
            self.release_held(held.key, false, AnsweredBy::Nobody);
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
        let mut request = HeldRequest::new(id, HeldKind::Download);
        request.method = "GET".into();
        request.origin = origin_of(url);
        match self
            .holds
            .hold(request, self.held_page(), None, shown_file(name))
        {
            Taken::Held(_) => {
                self.structure_dirty = true;
                true
            }
            Taken::Known => false,
            Taken::TooMany => {
                // One more than the person is asked about: discarded at once.
                self.release_held(HeldKey::Download(id), false, AnsweredBy::TooMany);
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

/// The facet a hold's number is stated under in the answer of the page
/// action whose run reached it: the number the window holds the departure
/// under, which is what answers it. It stands beside the departure's facets,
/// outside their prefix, so whoever reads what leaves does not read it.
pub(crate) const HELD_FACET: &str = "browser.held";

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
