//! "Waiting on you", the launcher's second section: what someone asked of
//! the person and has not had an answer to.
//!
//! Read from the notifications the desktop already holds, those addressed to
//! the person ([`Addressed`]): a call ringing now, a message someone wrote to
//! them or a mention, and a question a person or an agent is waiting on.
//! News, reminders and the desktop's own notices are not waiting on anyone.
//! Each row says who and when, and Enter does what its notification's own
//! default action does (answer, open), as a click on its banner would.
//! Scoped to the current workspace, only what came from there.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop::notifications::{Addressed, HistoryActivation, Notification};
use ui::widget::command_palette::RowFace;

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::launcher::LauncherSection;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "waiting-on-you";

/// Row ids: `notification:<id>`.
const ROW_PREFIX: &str = "notification:";

/// A row's icon and the faces at its end.
type Look = (Option<ui::icons::Icon>, Vec<RowFace>);

pub struct WaitingOnYouProvider {
    kinds: Vec<ResourceKind>,
    /// How the rows last listed look, by row id.
    looks: RefCell<HashMap<String, Look>>,
}

impl Default for WaitingOnYouProvider {
    fn default() -> Self {
        Self {
            kinds: vec![ResourceKind::Custom(t("search-launcher-waiting-on-you"))],
            looks: RefCell::default(),
        }
    }
}

impl WaitingOnYouProvider {
    pub fn new() -> Self {
        Self::default()
    }

    fn kind(&self) -> ResourceKind {
        self.kinds.first().cloned().unwrap_or(ResourceKind::Action)
    }
}

/// Whether `notification` waits on the person, and whether it is a call.
/// A call waits only while it rings, on screen; once it has gone to the
/// history it was missed, not waiting.
fn waits(notification: &Notification, ringing: bool) -> bool {
    match notification.addressed() {
        Some(Addressed::IncomingCall) => ringing,
        Some(Addressed::Message | Addressed::Ask) => true,
        Some(Addressed::Reminder) | None => false,
    }
}

/// The notifications waiting on the person in `scope`, newest first, each
/// with whether it is still on screen. One sent from no workspace (a call
/// to the person, not to a room) waits on them in every scope.
fn waiting(state: &ChromeState, scope: SearchScope) -> Vec<(&Notification, bool)> {
    let here = state.tree().desktop.active.0;
    let log = state.notification_log();
    let mut out: Vec<(&Notification, bool)> = log
        .active()
        .iter()
        .map(|notification| (notification, true))
        .chain(
            log.history()
                .iter()
                .map(|notification| (notification, false)),
        )
        .filter(|(notification, ringing)| waits(notification, *ringing))
        .filter(|(notification, _)| {
            scope == SearchScope::All
                || notification
                    .source_workspace
                    .is_none_or(|from| from == here)
        })
        .collect();
    out.sort_by_key(|(notification, _)| std::cmp::Reverse(notification.created_at));
    let mut seen = std::collections::HashSet::new();
    out.retain(|(notification, _)| seen.insert(notification.id));
    out
}

fn row(kind: ResourceKind, notification: &Notification, on_screen: bool) -> SearchResult {
    let who = notification
        .from
        .as_ref()
        .map(|person| person.name.clone())
        .or_else(|| notification.app_name.clone());
    let when = if notification.addressed() == Some(Addressed::IncomingCall) {
        t("search-launcher-calling-now")
    } else {
        tilo_desktop::notifications::relative_time(notification, ui::time::now())
    };
    let id = notification.id;
    let title = if notification.title.trim().is_empty() {
        notification.body.clone()
    } else {
        notification.title.clone()
    };
    SearchResult {
        facts: Vec::new(),
        kind,
        operation_kind: tilo_actions::OperationKind::Effect,
        provider_id: PROVIDER_ID.into(),
        id: format!("{ROW_PREFIX}{id}"),
        title,
        subtitle: None,
        badge: None,
        shortcut: None,
        score: 0,
        params: Vec::new(),
        contract: None,
        on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
            answer(chrome, id, on_screen)
        }),
        on_invoke: None,
        on_begin_invoke: None,
    }
    .with_facts(who.into_iter().chain(std::iter::once(when)))
}

/// How a row looks beside its words: its icon by what it is, and the face
/// of the person it is from.
fn look(notification: &Notification) -> Look {
    let icon = match notification.addressed() {
        Some(Addressed::IncomingCall) => Some(ui::icons::Icon::Phone),
        Some(Addressed::Message) => Some(ui::icons::Icon::MessageCircle),
        Some(Addressed::Ask | Addressed::Reminder) | None => Some(ui::icons::Icon::Bell),
    };
    let faces = notification
        .from
        .iter()
        .map(|person| RowFace {
            name: person.name.clone(),
            identity: person.identity.clone(),
            colour: ui::presence::identity_colour(&person.identity),
            is_agent: false,
            dimmed: false,
        })
        .collect();
    (icon, faces)
}

/// Do what the notification's banner does when clicked: its default action,
/// sent to the app or device it came from; for one restored after a restart,
/// open the app it came from.
fn answer(chrome: &Chrome, id: u64, on_screen: bool) -> Result<(), ActionError> {
    if on_screen && chrome.borrow_mut().activate_notification(id) {
        return Ok(());
    }
    let outcome = chrome.borrow_mut().activate_history_notification(id);
    match outcome {
        HistoryActivation::Invoked => Ok(()),
        HistoryActivation::OpenApp { app_id, label } => {
            super::applications::launch_installed_app(chrome, &app_id, &label)
        }
        HistoryActivation::Unavailable => Err(ActionError::Explained(t(
            "search-launcher-waiting-unavailable",
        ))),
        HistoryActivation::Missing => Err(ActionError::Refused(
            "the notification was answered or dismissed already".into(),
        )),
    }
}

impl SearchProvider for WaitingOnYouProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Waiting on you"
    }

    fn display_name(&self) -> String {
        t("search-launcher-waiting-on-you")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    fn launcher_section(&self) -> Option<LauncherSection> {
        Some(LauncherSection::WaitingOnYou)
    }

    fn launcher_rows(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let kind = self.kind();
        let mut looks = self.looks.borrow_mut();
        looks.clear();
        waiting(ctx.chrome, ctx.effective_scope())
            .into_iter()
            .map(|(notification, on_screen)| {
                let row = row(kind.clone(), notification, on_screen);
                looks.insert(row.id.clone(), look(notification));
                row
            })
            .collect()
    }

    /// Found only before anything is typed: a notification is answered from
    /// the launcher or its banner, not searched for.
    fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
        Vec::new()
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for (notification, on_screen) in waiting(ctx.chrome, SearchScope::All) {
            (notification.id, on_screen, notification.source_workspace).hash(&mut hasher);
        }
        ctx.chrome.tree().desktop.active.0.hash(&mut hasher);
        format!("waiting:{:016x}", hasher.finish())
    }

    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        let wanted: u64 = id.strip_prefix(ROW_PREFIX)?.parse().ok()?;
        let kind = self.kind();
        waiting(ctx.chrome, SearchScope::All)
            .into_iter()
            .find(|(notification, _)| notification.id == wanted)
            .map(|(notification, on_screen)| row(kind, notification, on_screen))
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        self.looks
            .borrow()
            .get(&result.id)
            .and_then(|(icon, _)| *icon)
    }

    fn row_outcome(&self, result: &SearchResult) -> Option<String> {
        let calling = self
            .looks
            .borrow()
            .get(&result.id)
            .is_some_and(|(icon, _)| *icon == Some(ui::icons::Icon::Phone));
        Some(t(if calling {
            "search-launcher-outcome-answer"
        } else {
            "search-launcher-outcome-open"
        }))
    }

    fn row_faces(&self, result: &SearchResult) -> Vec<RowFace> {
        self.looks
            .borrow()
            .get(&result.id)
            .map(|(_, faces)| faces.clone())
            .unwrap_or_default()
    }
}
