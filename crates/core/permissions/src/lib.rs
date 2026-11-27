//! What a window shows of its person's permissions.
//!
//! A window decides no permission. Its device decides every action the window publishes or
//! shows, for the person at it ([`tilo_actions::shown`]): one read when the window opens, and
//! again whenever a grant, a role or a declaration changes. [`Verdicts`] is that projection as the
//! window keeps it. The window acts at once on an allowed action, shows why a refused one is
//! refused, and sends one that asks first through the device, which keeps it as its asked record.
//! A window with no verdicts yet shows its controls as not yet decided and acts on nothing.
//! Wherever anything lands (a write, an effect's call, a grant given once) the device admits it
//! again, so a verdict gone stale never lets a refused change through.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

pub use tilo_action_policy as policy;
pub use tilo_action_policy::{Answer, Grantee, MemberAccess, TreePath};
pub use tilo_action_types::{ActionKind, KindSet};
use tilo_actions::shown::{ActionDecision, ActionVerdict, REFUSED, ShownAction, ShownVerdicts};
pub use tilo_member_types::{Acting, MemberId};

pub mod caps;

pub use caps::{Access, CapDecision, CapOverride, Effect, ResourceRef, Scope, decide_capability};
pub use tilo_action_policy::Role;

/// The id the person of a device's own local workspace acts as, when no host names one.
pub const LOCAL_USER: &str = "local";

/// The workspace a window is in when no host names one.
pub const LOCAL_WORKSPACE: &str = "local";

/// Which system app a presence key or a sandbox capability refers to.
pub use tilo_presence::AppKind;

/// What the device says the person at a window may do: its verdict of each question the window
/// asked, refused while a verdict is on its way. Only presentation: the device admits whatever
/// lands.
#[derive(Clone, Debug, Default)]
pub struct Verdicts {
    shown: Arc<ShownVerdicts>,
    /// The actions the window publishes and shows, as it last named them ([`Self::ask`]).
    published: Arc<BTreeSet<ShownAction>>,
    /// The questions the window asked about its app, a record or the workspace itself: kept for
    /// as long as the window is open.
    questions: Arc<BTreeSet<ShownAction>>,
    /// Questions read here that the device was not asked yet: asked next
    /// ([`Self::take_missed`]). Shared by every copy, so a read through any of them is heard.
    missed: Arc<Mutex<BTreeSet<ShownAction>>>,
    /// Whether the window only shows what it keeps (a past version, a retired document): every
    /// change is refused whatever the device says.
    reading: bool,
}

impl PartialEq for Verdicts {
    fn eq(&self, other: &Self) -> bool {
        self.shown == other.shown && self.reading == other.reading
    }
}

impl Eq for Verdicts {}

impl Verdicts {
    /// The same verdicts, for a window that only shows what it keeps: every change refused.
    #[must_use]
    pub fn reading(&self) -> Self {
        Self {
            reading: true,
            ..self.clone()
        }
    }

    /// Whether every change is refused here ([`Self::reading`]).
    #[must_use]
    pub const fn is_reading(&self) -> bool {
        self.reading
    }

    /// The window names every action it publishes and shows now: what it asks its device about.
    /// Each keeps the verdict it had when it was asked about before.
    pub fn ask(&mut self, published: impl IntoIterator<Item = ShownAction>) {
        let published: BTreeSet<ShownAction> = published.into_iter().collect();
        if *self.published != published {
            self.published = Arc::new(published);
            self.rebuild();
        }
    }

    /// Move the questions read here and not asked yet into what is asked: whether there were any.
    pub fn take_missed(&mut self) -> bool {
        let missed =
            std::mem::take(&mut *self.missed.lock().unwrap_or_else(PoisonError::into_inner));
        if missed.is_empty() {
            return false;
        }
        Arc::make_mut(&mut self.questions).extend(missed);
        self.rebuild();
        true
    }

    fn rebuild(&mut self) {
        let asked = self
            .shown
            .asking(self.published.iter().chain(self.questions.iter()).cloned());
        if *self.shown != asked {
            self.shown = Arc::new(asked);
        }
    }

    /// Whether the window waits for its device to say something it read or asked: what it acts
    /// on only once it said.
    #[must_use]
    pub fn undecided(&self) -> bool {
        self.shown.waits()
            || !self
                .missed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
    }

    /// Whether the window says what `decisions` say of the actions they decide.
    #[must_use]
    pub fn agrees(&self, decisions: &[ActionDecision]) -> bool {
        decisions
            .iter()
            .all(|decision| self.shown.verdict(&decision.action) == Some(Some(&decision.verdict)))
    }

    /// What the window asks its device about.
    #[must_use]
    pub fn asked(&self) -> Vec<ShownAction> {
        self.shown.actions().cloned().collect()
    }

    /// Ask the device about `questions` too, from now on, as the window's own questions are: what
    /// a scenario staged on the window runs, which the window shows no control of.
    pub fn ask_also(&mut self, questions: impl IntoIterator<Item = ShownAction>) {
        let before = self.questions.len();
        Arc::make_mut(&mut self.questions).extend(questions);
        if self.questions.len() != before {
            self.rebuild();
        }
    }

    /// Take what the device said with the window's launch: each of those actions is asked from
    /// now on, as the window's own questions are, and answered as the device said it.
    pub fn handed(&mut self, decisions: &[ActionDecision]) {
        if decisions.is_empty() {
            return;
        }
        Arc::make_mut(&mut self.questions)
            .extend(decisions.iter().map(|decision| decision.action.clone()));
        self.rebuild();
        self.decide(decisions);
    }

    /// Take the verdicts the device sent.
    pub fn decide(&mut self, decisions: &[ActionDecision]) {
        Arc::make_mut(&mut self.shown).decide(decisions);
    }

    /// What the device said of `question`: `None` while it is on its way, and while the window
    /// has not asked it, which it then asks next.
    fn verdict(&self, question: &ShownAction) -> Option<&ActionVerdict> {
        if let Some(verdict) = self.shown.verdict(question) {
            return verdict;
        }
        self.missed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(question.clone());
        None
    }

    /// Whether `question` changes anything: what a window that only reads refuses.
    const fn changes(question: &ShownAction) -> bool {
        !matches!(question.kind_needed, ActionKind::Read)
    }

    /// The device's answer to `question`: refused until it arrives.
    #[must_use]
    pub fn answer(&self, question: &ShownAction) -> Answer {
        self.said(question).unwrap_or(Answer::Deny)
    }

    /// What the device said of `question`: `None` while it has not, which asks it.
    #[must_use]
    pub fn said(&self, question: &ShownAction) -> Option<Answer> {
        if self.reading && Self::changes(question) {
            return Some(Answer::Deny);
        }
        self.verdict(question).map(|verdict| match verdict {
            ActionVerdict::Allowed => Answer::Allow,
            ActionVerdict::AsksFirst => Answer::Ask,
            ActionVerdict::Refused(_) => Answer::Deny,
        })
    }

    /// Whether these withhold from what a window publishes exactly what `earlier` withheld: what
    /// a filter by `earlier` kept of it, a filter by these keeps. Verdicts for actions published
    /// since leave it so unless they refuse.
    #[must_use]
    pub fn withhold_as(&self, earlier: &Self) -> bool {
        self.reading == earlier.reading && self.shown.refuse_as(&earlier.shown)
    }

    /// Whether the person may take `question`: allowed, or once they confirm it.
    #[must_use]
    pub fn permits(&self, question: &ShownAction) -> bool {
        self.answer(question) != Answer::Deny
    }

    /// Why `question` is refused here, as the person reads it: the device's reason, or that it is
    /// still being checked. `None` when it is not refused.
    #[must_use]
    pub fn refusal(&self, question: &ShownAction) -> Option<String> {
        if self.reading && Self::changes(question) {
            return Some(REFUSED.to_owned());
        }
        match self.verdict(question) {
            Some(ActionVerdict::Allowed | ActionVerdict::AsksFirst) => None,
            Some(ActionVerdict::Refused(why)) => Some(why.clone()),
            None => Some(tilo_actions::shown::CHECKING.to_owned()),
        }
    }

    /// Admit the person's own call of `action` on `resource`, published by `app`, with the typed
    /// `json` or text `argument` it carries, as the device said of it.
    ///
    /// # Errors
    /// Refused, with why; or [`tilo_actions::ApiError::ConfirmationRequired`] where it asks first:
    /// the call then goes through the device's dispatch, which keeps it as its asked record until
    /// the person answers, and spends a grant given once as it admits it.
    pub fn admit(
        &self,
        app: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
        (json, argument): (
            Option<&serde_json::Map<String, serde_json::Value>>,
            Option<&str>,
        ),
    ) -> Result<(), tilo_actions::ApiError> {
        // Any member may ask the workspace's host to run a member-own action for themselves, as
        // them: the host decides each call by their grants there.
        if action.contract.is_member_own() {
            return Ok(());
        }
        // A generic edit or a group is decided by the value it carries: the field it writes and
        // what that needs, not its own least need.
        let question = if action.carries_actions() {
            ShownAction::new(
                resource.workspace.as_deref(),
                app,
                (&resource.kind, resource.id.as_str(), &action.id),
                &action.need_for(json, argument),
            )
        } else {
            question(app, resource, action)
        };
        match self.answer(&question) {
            Answer::Allow => Ok(()),
            Answer::Ask => Err(tilo_actions::ApiError::ConfirmationRequired),
            Answer::Deny => Err(tilo_actions::ApiError::Unavailable {
                code: "permission_denied".into(),
                reason: self
                    .refusal(&question)
                    .unwrap_or_else(|| REFUSED.to_owned()),
            }),
        }
    }

    /// Whether the person may take `action` on `resource`, published by `app`.
    #[must_use]
    pub fn permits_published(
        &self,
        app: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
    ) -> bool {
        self.permits(&question(app, resource, action))
    }

    /// Whether a control of `action` on `resource`, published by `app`, shows: unless the device
    /// refuses it. One still being checked shows, as not yet decided.
    #[must_use]
    pub fn shows_published(
        &self,
        app: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
    ) -> bool {
        // A member-own action is offered to every member: the workspace's host runs it for
        // whoever asked, as them, and admits each call.
        if action.contract.is_member_own() {
            return true;
        }
        let question = question(app, resource, action);
        (!self.reading || !Self::changes(&question))
            && !matches!(self.verdict(&question), Some(ActionVerdict::Refused(_)))
    }

    /// `resource`, published by `app`, as the person may reach it: every action but those the
    /// device refuses (one still being checked stays, shown as not yet decided), and the nested
    /// resources that keep any. `None` when nothing is left.
    #[must_use]
    pub fn permitted_resource(
        &self,
        app: &str,
        resource: tilo_actions::ResourceSpec,
    ) -> Option<tilo_actions::ResourceSpec> {
        self.sifted(app, resource, false)
    }

    /// [`Self::permitted_resource`] for a caller that addresses actions rather than seeing
    /// controls (an agent through a view with no window): where it may take any action of a
    /// resource, one the device refuses stays listed, marked not allowed in the device's words,
    /// so it hears why rather than that there is no such action. A resource it may take nothing
    /// on discloses none of its actions.
    #[must_use]
    pub fn listed_resource(
        &self,
        app: &str,
        resource: tilo_actions::ResourceSpec,
    ) -> Option<tilo_actions::ResourceSpec> {
        self.sifted(app, resource, true)
    }

    fn sifted(
        &self,
        app: &str,
        mut resource: tilo_actions::ResourceSpec,
        lists_refused: bool,
    ) -> Option<tilo_actions::ResourceSpec> {
        let mut shown: Vec<bool> = resource
            .actions
            .iter()
            .map(|action| self.shows_published(app, &resource, action))
            .collect();
        // A generic edit or a group is offered under the least of the edits it carries: wherever
        // any edit of the resource is, it is too, and each value it carries is decided where it
        // lands.
        let any_change = resource.actions.iter().zip(&shown).any(|(action, shown)| {
            *shown && !action.carries_actions() && action.contract.need().kind != ActionKind::Read
        });
        if any_change {
            for (action, shown) in resource.actions.iter().zip(shown.iter_mut()) {
                *shown |= action.carries_actions();
            }
        }
        let refused: Vec<String> = resource
            .actions
            .iter()
            .zip(&shown)
            .filter(|(_, shown)| !**shown)
            .map(|(action, _)| action.id.clone())
            .collect();
        if lists_refused && shown.iter().any(|shown| *shown) {
            let reasons: Vec<Option<String>> = resource
                .actions
                .iter()
                .zip(&shown)
                .map(|(action, shown)| {
                    (!*shown).then(|| {
                        self.refusal(&question(app, &resource, action))
                            .unwrap_or_else(|| REFUSED.to_owned())
                    })
                })
                .collect();
            for (action, reason) in resource.actions.iter_mut().zip(reasons) {
                if let Some(reason) = reason {
                    action.type_mut().contract.availability =
                        tilo_actions::Availability::Unavailable {
                            code: tilo_actions::Availability::NOT_ALLOWED.to_owned(),
                            reason,
                        };
                }
            }
        } else {
            let mut kept = shown.into_iter();
            resource.retain_actions(|_| kept.next().unwrap_or(false));
        }
        // What a typed edit or a group beside them names is cut by these same verdicts: a call of
        // a refused action through one is refused too, so neither names it. Where the refused
        // stay listed, the edit or group keeps naming them, and a call of one through it is
        // refused saying why, as a call of the action itself is.
        if !refused.is_empty() && !lists_refused {
            let refused: Vec<&str> = refused.iter().map(String::as_str).collect();
            for action in &mut resource.actions {
                action.without_carried(&refused);
            }
        }
        resource.children = resource
            .children
            .into_iter()
            .filter_map(|child| self.sifted(app, child, lists_refused))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }
}

/// The question of `action` on `resource`, published by `app`.
fn question(
    app: &str,
    resource: &tilo_actions::ResourceSpec,
    action: &tilo_actions::ActionSpec,
) -> ShownAction {
    ShownAction::published(app, resource, action)
}

/// The verdicts a host shares with what it hosts (an app, its components, a worker), which the
/// device changes while they run. Every change goes through [`Self::update`] or [`Self::set`],
/// which move [`shared_revision`] on, so whatever derives something from them (which controls may
/// run) derives it again only after a change, not on every frame.
#[derive(Clone, Default)]
pub struct SharedPermissions(std::rc::Rc<std::cell::RefCell<Verdicts>>);

thread_local! {
    static SHARED_REVISION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Moves on with every change from outside the thread that shared verdicts follow
/// ([`outside_changed`]).
static OUTSIDE_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What to call when something outside changes: each answers whether it still wants to hear.
type OutsideWaker = Box<dyn Fn() -> bool + Send>;

/// The loops to wake when something outside changes.
static OUTSIDE_WAKES: Mutex<Vec<OutsideWaker>> = Mutex::new(Vec::new());

/// Moves on with every change of any [`SharedPermissions`] on this thread, and with every change
/// announced from outside it.
#[must_use]
pub fn shared_revision() -> u64 {
    SHARED_REVISION
        .get()
        .wrapping_add(OUTSIDE_REVISION.load(std::sync::atomic::Ordering::Acquire))
}

/// Something the window's verdicts follow changed, on any thread: moves [`shared_revision`] on and
/// wakes every loop that asked ([`wake_on_outside_change`]), so what it derived from them is
/// derived again.
pub fn outside_changed() {
    OUTSIDE_REVISION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    OUTSIDE_WAKES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|wake| wake());
}

/// Call `wake` whenever something outside the verdicts follow changes, until it answers that it
/// no longer wants to hear (a loop whose scope closed).
pub fn wake_on_outside_change(wake: impl Fn() -> bool + Send + 'static) {
    OUTSIDE_WAKES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Box::new(wake));
}

impl SharedPermissions {
    /// Share `verdicts`.
    #[must_use]
    pub fn new(verdicts: Verdicts) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(verdicts)))
    }

    /// The verdicts in force now, to read.
    ///
    /// # Panics
    /// While a change of the same verdicts runs.
    pub fn borrow(&self) -> std::cell::Ref<'_, Verdicts> {
        self.0.borrow()
    }

    /// [`Self::borrow`], or an error while a change of them runs.
    ///
    /// # Errors
    /// While a [`Self::set`] or [`Self::update`] of the same verdicts runs.
    pub fn try_borrow(&self) -> Result<std::cell::Ref<'_, Verdicts>, std::cell::BorrowError> {
        self.0.try_borrow()
    }

    /// Change the verdicts in force in place (the device's arrived).
    pub fn update<R>(&self, change: impl FnOnce(&mut Verdicts) -> R) -> R {
        let (result, moved) = {
            let mut live = self.0.borrow_mut();
            let before = live.clone();
            let result = change(&mut live);
            (result, *live != before)
        };
        // A change that changed nothing is no change: nothing drawn from these verdicts is stale.
        if moved {
            SHARED_REVISION.set(SHARED_REVISION.get().wrapping_add(1));
        }
        result
    }

    /// The verdicts in force now.
    #[must_use]
    pub fn get(&self) -> Verdicts {
        self.0.borrow().clone()
    }

    /// Put `verdicts` in force for everyone sharing these.
    pub fn set(&self, verdicts: Verdicts) {
        {
            let mut live = self.0.borrow_mut();
            if *live == verdicts {
                return;
            }
            *live = verdicts;
        }
        SHARED_REVISION.set(SHARED_REVISION.get().wrapping_add(1));
    }

    /// Whether `self` and `other` are the same shared verdicts.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }

    /// A handle that does not keep the verdicts shared.
    #[must_use]
    pub fn downgrade(&self) -> WeakPermissions {
        WeakPermissions(std::rc::Rc::downgrade(&self.0))
    }
}

impl From<Verdicts> for SharedPermissions {
    fn from(verdicts: Verdicts) -> Self {
        Self::new(verdicts)
    }
}

impl std::fmt::Debug for SharedPermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedPermissions")
            .field(&*self.0.borrow())
            .finish()
    }
}

/// [`SharedPermissions`] held without keeping them shared.
#[derive(Clone)]
pub struct WeakPermissions(std::rc::Weak<std::cell::RefCell<Verdicts>>);

impl WeakPermissions {
    /// The verdicts, while anyone still shares them.
    #[must_use]
    pub fn upgrade(&self) -> Option<SharedPermissions> {
        self.0.upgrade().map(SharedPermissions)
    }

    /// Whether anyone still shares the verdicts.
    #[must_use]
    pub fn is_shared(&self) -> bool {
        self.0.strong_count() > 0
    }
}

/// The verdicts as they stand *now*, following every change the device sends while the app runs.
/// Read-only here: only the device's verdicts change them.
#[derive(Clone)]
pub struct LivePermissions(SharedPermissions);

impl LivePermissions {
    /// Follow the verdicts the host shares with its components.
    #[must_use]
    pub fn shared(verdicts: SharedPermissions) -> Self {
        Self(verdicts)
    }

    /// Verdicts that never change: a preview of a past version, a capture.
    #[must_use]
    pub fn fixed(verdicts: Verdicts) -> Self {
        Self(SharedPermissions::new(verdicts))
    }

    /// Whether the person may take `question`, as of now.
    #[must_use]
    pub fn permits(&self, question: &ShownAction) -> bool {
        self.0.borrow().permits(question)
    }

    /// The verdicts in force now.
    #[must_use]
    pub fn current(&self) -> Verdicts {
        self.0.get()
    }

    /// A call refused as `why` says, or, while the device has not answered what the window read
    /// ([`Verdicts::undecided`]), as not yet decided: asked again once it has, the call is decided.
    #[must_use]
    pub fn refused(&self, why: String) -> String {
        if self.0.borrow().undecided() {
            tilo_actions::shown::CHECKING.to_owned()
        } else {
            why
        }
    }

    /// The window names what it publishes and shows ([`Verdicts::ask`]).
    pub fn ask(&self, published: impl IntoIterator<Item = ShownAction>) {
        self.0.update(|verdicts| verdicts.ask(published));
    }

    /// The questions read and not asked yet move into what is asked ([`Verdicts::take_missed`]).
    pub fn take_missed(&self) -> bool {
        self.0.update(Verdicts::take_missed)
    }

    /// The device's verdicts arrived ([`Verdicts::decide`]).
    pub fn decide(&self, decisions: &[ActionDecision]) {
        self.0.update(|verdicts| verdicts.decide(decisions));
    }
}

impl std::fmt::Debug for LivePermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LivePermissions")
            .field(&*self.0.borrow())
            .finish()
    }
}

/// The refusal of a call the window may not make.
#[must_use]
pub fn denied(action: &tilo_actions::ActionSpec) -> tilo_actions::ApiError {
    tilo_actions::ApiError::Unavailable {
        code: "permission_denied".into(),
        reason: format!("you may not {} here", action.label.to_lowercase()),
    }
}

#[cfg(test)]
mod tests;
