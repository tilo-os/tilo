//! Notifications over the strip: the shared banner stack in the top-right corner and, on request,
//! the shared notification centre under it, both reading a [`DesktopNotifications`] log the host
//! posts into.
//! Every banner that leaves the stack (closed, timed out, chosen, pushed off by newer ones) is
//! retained in the centre until the person dismisses it there, as on the native desktop. What the
//! person chose is handed back to the host as [`NotificationOutcome`]s: only the host can answer
//! the application that sent a notification.
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use ui::a11y::AccessNode;
use ui::kurbo::{Affine, Point, Rect};
use ui::taffy::{NodeId, TaffyTree};
use ui::time::Instant;
use ui::vello::Scene;
use ui::widget::Widget;
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

use crate::notifications::{
    BannerFeed, CenterLayout, CenterMetrics, CenterOutcome, CentreApi, ContinuityNotificationId,
    HistoryActivation, NOTIFICATION_CENTRE, Notification, NotificationHost, NotificationLog,
    NotificationSeverity, NotificationStack,
};
use tilo_lock::MutexRecover as _;

/// How tall the centre's panel stands while its code is on its way.
const LOADING_HEIGHT: f64 = 96.0;

/// The open centre, or its panel waiting on the centre's code.
type Centre = ui::lazy_code::LazySurface<Box<dyn CentreApi>, ()>;

/// What the person did with a notification that its sender should hear.
#[derive(Clone, Debug)]
pub enum NotificationOutcome {
    /// The person chose the notification: its default action, or opening its application.
    Activated {
        notification: Box<Notification>,
        action: Option<String>,
    },
    /// The person removed a retained notification from the centre.
    Dismissed { id: u64 },
}

#[derive(Default)]
struct NoticeState {
    log: RefCell<NotificationLog>,
    /// Bumped by every change, so the layer repaints only when one happened.
    revision: Cell<u64>,
    outcomes: RefCell<Vec<NotificationOutcome>>,
    /// The person asked the centre open or closed (the bell, a chord, the banner stack's "more"
    /// card): whoever draws it over the windows takes the ask on its next pass.
    centre_asked: Cell<Option<bool>>,
    /// An app a row of the centre asked to open: the desk focuses its window.
    open_app: RefCell<Option<String>>,
    /// Where the shown windows' content starts: banners stand below it.
    clear_top: Cell<f64>,
    /// Notifications posted since the person last looked at the centre, by id: what the bell
    /// counts.
    unread: RefCell<std::collections::BTreeSet<u64>>,
    /// The host shows the banners elsewhere (an app of their own); the surface shows only the
    /// centre.
    banners_elsewhere: Cell<bool>,
    /// The notification centre is open: no banner stands over it, and what arrives while it is open
    /// goes into it rather than onto the desk.
    centre_open: Cell<bool>,
    /// Told of each outcome as it happens.
    taps: RefCell<Vec<OutcomeTap>>,
    /// Why fresh banners are held right now (Do Not Disturb, a focus schedule, presenting), as the
    /// bar and the centre's header say it.
    quiet: RefCell<Option<String>>,
    /// How many unread notifications each app says it has, by the id the launcher attributed: what
    /// its icon badges.
    badges: RefCell<std::collections::BTreeMap<String, u32>>,
    /// The shell's own centre stands over the desk, as its reading says. Its own state: the shell
    /// knows when it closes, the desk's layer when its own does.
    shell_centre: Cell<bool>,
}

type OutcomeTap = Rc<dyn Fn(&NotificationOutcome)>;

impl NoticeState {
    fn touch(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    fn retire_active(&self, id: u64) -> Option<Notification> {
        let taken = self.log.borrow_mut().take_active(id)?;
        self.log.borrow_mut().retain(taken.clone());
        self.touch();
        Some(taken)
    }

    fn activated(&self, notification: Notification) {
        let action = notification.default_action_key();
        self.outcome(NotificationOutcome::Activated {
            notification: Box::new(notification),
            action,
        });
    }

    /// Hand `outcome` to the host: its taps now, its queue for later.
    fn outcome(&self, outcome: NotificationOutcome) {
        let taps = self.taps.borrow().clone();
        for tap in &taps {
            tap(&outcome);
        }
        self.outcomes.borrow_mut().push(outcome);
    }
}

impl NotificationHost for NoticeState {
    fn log(&self) -> Ref<'_, NotificationLog> {
        self.log.borrow()
    }
    fn present(&self, now: Instant, on_screen: &[u64], held: bool) {
        self.log.borrow_mut().present(now, on_screen, held);
    }
    fn open_center(&self) {
        self.centre_asked.set(Some(true));
        self.touch();
    }
    fn quiet_status(&self) -> Option<String> {
        self.quiet.borrow().clone()
    }
    fn dismiss(&self, id: u64) {
        self.retire_active(id);
    }
    fn activate(&self, id: u64) {
        if let Some(notification) = self.retire_active(id) {
            self.activated(notification);
        }
    }
    fn activate_history(&self, id: u64) -> HistoryActivation {
        let Some(notification) = self.log.borrow().history_entry(id).cloned() else {
            return HistoryActivation::Missing;
        };
        if !notification.actions.is_empty() {
            self.activated(notification);
            return HistoryActivation::Invoked;
        }
        match (&notification.app_id, &notification.app_name) {
            (Some(app_id), label) => HistoryActivation::OpenApp {
                app_id: app_id.clone(),
                label: label.clone().unwrap_or_else(|| app_id.clone()),
            },
            (None, _) => HistoryActivation::Unavailable,
        }
    }
    fn dismiss_history(&self, id: u64) {
        // A notice still up as a banner goes off the stack with it; one retired into the centre
        // goes from the centre. Either way it is dismissed.
        let (from_banner, from_history) = {
            let mut log = self.log.borrow_mut();
            (
                log.take_active(id).is_some(),
                log.take_history(id).is_some(),
            )
        };
        if from_banner || from_history {
            self.outcome(NotificationOutcome::Dismissed { id });
            self.touch();
        }
    }
    fn clear_history(&self) {
        if self.log.borrow_mut().clear_history(ui::time::now()) {
            self.touch();
        }
    }
    fn undo_clear(&self) {
        if self.log.borrow_mut().undo_clear() {
            self.touch();
        }
    }
    fn changed(&self) {
        self.touch();
    }
    /// An ask the host posted shows its choices as buttons: "Not now" and the action itself. An
    /// application's notification shows the action it named ("Try again"); one without opens on a
    /// click.
    fn actions_for(&self, notification: &Notification) -> Vec<(String, String)> {
        if super::search::is_host_offer(notification) {
            // Painted secondary first, so the answer ends the row.
            notification.actions.iter().rev().cloned().collect()
        } else {
            notification.actions.clone()
        }
    }
    fn invoke_action(&self, id: u64, key: &str) {
        if key == crate::notifications::NOT_NOW_ACTION {
            // Put off, not refused: the ask waits in the centre.
            self.retire_active(id);
            return;
        }
        if let Some(notification) = self.retire_active(id) {
            self.outcome(NotificationOutcome::Activated {
                notification: Box::new(notification),
                action: Some(key.to_owned()),
            });
            return;
        }
        // A retained row's action answers its sender the same way, and the row leaves the centre:
        // the person has dealt with it.
        let retained = self.log.borrow_mut().take_history(id);
        if let Some(notification) = retained {
            self.outcome(NotificationOutcome::Activated {
                notification: Box::new(notification),
                action: Some(key.to_owned()),
            });
            self.touch();
        }
    }
}

/// A desktop's notifications: the live banners and the retained history the surface shows. The host
/// posts into it and reads back what the person did. Cheap to clone; single-threaded.
#[derive(Clone, Default)]
pub struct DesktopNotifications {
    log: Rc<NoticeState>,
}

impl DesktopNotifications {
    /// Show `notification` as a banner; any it pushes off the stack retire to the centre. Returns
    /// its id. One that arrives while the centre is open goes straight into the centre: the person
    /// is reading it, so a banner there would cover a row and take the press meant for it.
    pub fn post(&self, notification: Notification) -> u64 {
        let id = notification.id;
        self.log.unread.borrow_mut().insert(id);
        if self.log.centre_open.get() {
            self.log.log.borrow_mut().retain(notification);
        } else {
            let evicted = self.log.log.borrow_mut().show(notification);
            for retired in evicted {
                self.log.log.borrow_mut().retain(retired);
            }
        }
        self.log.touch();
        id
    }

    /// Withdraw notification `id` , live or retained: its sender took it back.
    pub fn withdraw(&self, id: u64) -> bool {
        let mut log = self.log.log.borrow_mut();
        let taken = log.take_active(id).is_some() | log.take_history(id).is_some();
        drop(log);
        if taken {
            self.log.touch();
        }
        taken
    }

    /// Withdraw every ask for host action `id` , live or retained: the host no longer offers it (it
    /// was done, perhaps from search).
    pub fn withdraw_host_action(&self, id: &str) -> bool {
        let mut log = self.log.log.borrow_mut();
        let asks: Vec<u64> = log
            .active()
            .iter()
            .chain(log.history())
            .filter(|notification| super::search::offers(notification, id))
            .map(|notification| notification.id)
            .collect();
        for ask in &asks {
            let _ = log.take_active(*ask).is_some() || log.take_history(*ask).is_some();
        }
        drop(log);
        if !asks.is_empty() {
            self.log.touch();
        }
        !asks.is_empty()
    }

    /// Show `notification` in the centre without a banner: a quiet mode (Do Not Disturb, a focus
    /// schedule) holds it back, and the person reads it there. Returns its id.
    pub fn post_quiet(&self, notification: Notification) -> u64 {
        let id = notification.id;
        self.log.unread.borrow_mut().insert(id);
        self.log.log.borrow_mut().retain(notification);
        self.log.touch();
        id
    }

    /// Replace the banner or centre row the same origin as `notification` showed before, keeping
    /// its local id so hit targets and centre selection survive: what another device's notification
    /// update is. Returns the id it shows under; `None` when the desk showed none from that origin
    /// yet.
    pub fn replace_remote(&self, notification: &Notification) -> Option<u64> {
        let route = notification.remote_route.as_ref()?;
        let existing = {
            let log = self.log.log.borrow();
            log.active()
                .iter()
                .chain(log.history().iter())
                .find(|shown| shown.remote_route.as_ref() == Some(route))
                .map(|shown| shown.id)?
        };
        self.log.log.borrow_mut().replace_remote(notification);
        self.log.unread.borrow_mut().insert(existing);
        self.log.touch();
        Some(existing)
    }

    /// Take every copy of the notification `id` another device projected,
    /// live or retained: its origin is done with it. Returns whether the desk
    /// showed any.
    pub fn retire_remote(&self, source_device: &str, id: ContinuityNotificationId) -> bool {
        let gone: Vec<u64> = {
            let log = self.log.log.borrow();
            log.active()
                .iter()
                .chain(log.history().iter())
                .filter(|shown| {
                    shown.remote_route.as_ref().is_some_and(|route| {
                        route.source_device == source_device && route.notification_id == id
                    })
                })
                .map(|shown| shown.id)
                .collect()
        };
        let retired = self.log.log.borrow_mut().retire_remote(source_device, id);
        if retired {
            let mut unread = self.log.unread.borrow_mut();
            for id in gone {
                unread.remove(&id);
            }
            drop(unread);
            self.log.touch();
        }
        retired
    }

    /// Why fresh banners are held now, for the bar's silenced bell and the centre's header; `None`
    /// while they show.
    pub fn set_quiet(&self, quiet: Option<String>) {
        let changed = *self.log.quiet.borrow() != quiet;
        if changed {
            *self.log.quiet.borrow_mut() = quiet;
            self.log.touch();
        }
    }

    /// Whether fresh banners are held now.
    #[must_use]
    pub fn quiet(&self) -> bool {
        self.log.quiet.borrow().is_some()
    }

    /// The centre stands open on the desk: what arrives joins its list, so no banner covers it. A
    /// banner up when it opens goes into it, since a stack over an open centre would take the
    /// presses meant for its rows.
    pub fn set_centre_open(&self, open: bool) {
        if open && !self.log.centre_open.get() {
            self.retire_banners();
        }
        self.log.centre_open.set(open);
    }

    /// The shell's centre stands over the desk, or has closed: the shell says which, every pass,
    /// since it knows every way the person leaves it. What arrives joins the list it shows while it
    /// is up, and none covers it.
    pub fn set_shell_centre(&self, open: bool) {
        if open && !self.log.shell_centre.get() {
            self.retire_banners();
        }
        self.log.shell_centre.set(open);
    }

    /// Whether the centre is open, on the desk or over it.
    #[must_use]
    pub fn centre_open(&self) -> bool {
        self.log.centre_open.get() || self.log.shell_centre.get()
    }

    /// Retire every banner still up into the centre: what a surface opening over them does, so none
    /// covers it.
    pub fn retire_banners(&self) {
        let banners: Vec<u64> = self.log().active().iter().map(|n| n.id).collect();
        for id in banners {
            self.log.retire_active(id);
        }
    }

    /// Whether the retained history changed since the host last wrote it out: a host persisting the
    /// centre writes it when this says so.
    #[must_use]
    pub fn take_history_dirty(&self) -> bool {
        self.log.log.borrow_mut().take_history_dirty()
    }

    /// How many unread notifications the app with verified id `app_id` owns, as its icon badges.
    #[must_use]
    pub fn app_badge(&self, app_id: &str) -> u32 {
        self.log.badges.borrow().get(app_id).copied().unwrap_or(0)
    }

    /// When the desk's notifications next change with nobody asking: a banner's dwell running out,
    /// or the Clear all undo closing.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.log.log.borrow().next_deadline()
    }

    /// Retire banners whose dwell ran out and forget a lapsed Clear all. Returns whether anything
    /// changed.
    pub fn expire(&self, now: Instant) -> bool {
        let expired = self.log.log.borrow_mut().take_expired(now);
        let mut changed = !expired.is_empty();
        for retired in expired {
            self.log.log.borrow_mut().retain(retired);
        }
        changed |= self.log.log.borrow_mut().expire_clear(now);
        if changed {
            self.log.touch();
        }
        changed
    }

    /// The banner stack laid out `on_screen` at `now` , `held` or not; see
    /// [`NotificationLog::present`].
    pub fn present(&self, now: Instant, on_screen: &[u64], held: bool) {
        self.log.present(now, on_screen, held);
    }

    /// The centre's history as a host keeps it between sessions: each row's words, sender and age,
    /// and whether the person has seen it. What a row could act on, and a row that is only an
    /// offer of this host's, do not outlive the session.
    pub fn kept(&self, wall_now: ui::time::SystemTime) -> Result<Vec<u8>, serde_json::Error> {
        let history: Vec<Notification> = self
            .log
            .log
            .borrow()
            .history()
            .iter()
            .filter(|notification| !crate::is_host_offer(notification))
            .cloned()
            .collect();
        crate::notifications::encode_history(
            &history,
            |id| self.is_unread(id),
            wall_now,
            ui::time::now(),
        )
    }

    /// Put the history [`Self::kept`] wrote back into the centre, without showing it as new
    /// banners: what the person had not seen is still unread, so the bell counts it again.
    pub fn restore_kept(
        &self,
        bytes: &[u8],
        wall_now: ui::time::SystemTime,
    ) -> Result<(), serde_json::Error> {
        let kept = crate::notifications::decode_history(bytes, wall_now, ui::time::now())?;
        if kept.is_empty() {
            return Ok(());
        }
        let mut history = Vec::with_capacity(kept.len());
        {
            let mut unread = self.log.unread.borrow_mut();
            for (notification, was_unread) in kept {
                if was_unread {
                    unread.insert(notification.id);
                }
                history.push(notification);
            }
        }
        self.log.log.borrow_mut().restore_history(history);
        self.log.touch();
        Ok(())
    }

    /// Also tell `tap` of each outcome as it happens: for a host that answers a notification's
    /// sender the moment the person chose, while the surface carries out its own part (opening the
    /// app).
    pub fn on_outcome(&self, tap: impl Fn(&NotificationOutcome) + 'static) {
        self.log.taps.borrow_mut().push(Rc::new(tap));
    }

    /// The host shows the banners in a surface of their own ([`Self::feed`], [`Self::answer`]); the
    /// desktop surface shows only the centre.
    pub fn show_banners_elsewhere(&self) {
        self.log.banners_elsewhere.set(true);
        self.log.touch();
    }

    /// The banners shown now, for a banner surface of the host's own. Nothing while the centre is
    /// open: a banner there would stand over the rows the person is reading.
    #[must_use]
    pub fn feed(&self) -> BannerFeed {
        if self.log.centre_open.get() {
            BannerFeed::default()
        } else {
            BannerFeed::of(self.host())
        }
    }

    /// Carry out what the person did on a banner surface of the host's own.
    pub fn answer(&self, ask: crate::notifications::BannerAsk) {
        ask.answer(self.host());
    }

    /// Ask for the notification centre open or closed: whoever draws it takes the ask
    /// ([`Self::take_centre_ask`]).
    pub fn ask_centre(&self, open: bool) {
        self.log.centre_asked.set(Some(open));
        self.log.touch();
    }

    /// Whether the person last asked the centre open or closed since this was last called.
    pub fn take_centre_ask(&self) -> Option<bool> {
        self.log.centre_asked.take()
    }

    /// The app a row of the centre asked to open since this was last called.
    pub fn take_open_app(&self) -> Option<String> {
        self.log.open_app.borrow_mut().take()
    }

    /// Banners begin below `y` , where the shown windows' content starts.
    pub fn set_clear_top(&self, y: f64) {
        if self.log.clear_top.get().to_bits() != y.to_bits() {
            self.log.clear_top.set(y);
            self.log.touch();
        }
    }

    /// What the person chose since the last call, oldest first.
    pub fn take_outcomes(&self) -> Vec<NotificationOutcome> {
        std::mem::take(&mut *self.log.outcomes.borrow_mut())
    }

    pub fn revision(&self) -> u64 {
        self.log.revision.get()
    }

    /// Notifications posted since the centre was last open that are still shown, as a banner or in
    /// the centre: the bell's count. One withdrawn or cleared is no longer news.
    pub fn unread(&self) -> usize {
        let log = self.log.log.borrow();
        let unread = self.log.unread.borrow();
        log.active()
            .iter()
            .chain(log.history())
            .filter(|notification| unread.contains(&notification.id))
            .count()
    }

    /// The person saw every notification: the centre opened or closed.
    pub fn mark_read(&self) {
        let had = !std::mem::take(&mut *self.log.unread.borrow_mut()).is_empty();
        if had {
            // What is kept between sessions says which rows were read.
            self.log.log.borrow_mut().history_changed();
            self.log.touch();
        }
    }

    /// Whether the notification `id` is one the person has not seen: what the centre's unread mark
    /// on its row says, and the same set the bell counts.
    pub fn is_unread(&self, id: u64) -> bool {
        self.log.unread.borrow().contains(&id)
    }

    /// Banners on screen and notifications retained in the centre.
    pub fn counts(&self) -> (usize, usize) {
        let log = self.log.log.borrow();
        (log.active().len(), log.history().len())
    }

    pub fn log(&self) -> Ref<'_, NotificationLog> {
        self.log.log.borrow()
    }

    /// Choose the retained notification `id` in the centre: its action runs,
    /// its app opens, or nothing can act on it any more.
    pub fn activate_history(&self, id: u64) -> HistoryActivation {
        self.host().activate_history(id)
    }

    /// Dismiss the retained notification `id` from the centre.
    pub fn dismiss_history(&self, id: u64) {
        self.host().dismiss_history(id);
    }

    /// Run the action `key` of the retained notification `id` .
    pub fn invoke_action(&self, id: u64, key: &str) {
        self.host().invoke_action(id, key);
    }

    /// Clear the centre, keeping the clear undoable for a while.
    pub fn clear_history(&self) {
        self.host().clear_history();
    }

    /// Clear every notice the desk lists, the banners still up with the ones retired into the
    /// centre: the banners retire into the centre first, so one Undo brings all of them back.
    pub fn clear_all(&self) {
        self.retire_banners();
        self.host().clear_history();
    }

    /// Bring back what the last Clear all took.
    pub fn undo_clear(&self) {
        self.host().undo_clear();
    }

    /// Whether the last Clear all can still be undone.
    pub fn can_undo_clear(&self) -> bool {
        self.log.log.borrow().can_undo_clear()
    }

    fn host(&self) -> &dyn NotificationHost {
        &*self.log
    }
}

/// What an application asked of the desktop's notifications, queued until a surface's next update:
/// applications post from wherever their code runs, the surface attributes and shows on its own
/// thread.
#[derive(Debug)]
enum AppRequest {
    Show {
        /// The window whose application code was running.
        tab: Option<u64>,
        notification: ui::notify::Notification,
    },
    Withdraw {
        tab: Option<u64>,
        tag: String,
    },
}

/// Requests and how many surfaces passed them over: a window another surface shows is that
/// surface's to attribute.
type Queue = std::sync::Arc<std::sync::Mutex<Vec<(AppRequest, u8)>>>;

/// Surfaces that pass a request over this many times show it unattributed: the window that posted
/// it has closed.
const UNCLAIMED_PASSES: u8 = 4;

/// Applications' notifications for desktops that show them themselves: the
/// [`Notifier`](ui::notify::Notifier) a host installs as its services' notifications. Each surface
/// showing them takes an [`AppInbox`]; each request is attributed to the window whose application
/// code made it.
/// With no surface attached (an application mounted on its own), requests go straight to `forward`
/// , the platform's notifications. While the page is in the background they also go there, where
/// the person can see them.
pub struct AppNotifier {
    queue: Queue,
    inboxes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    forward: std::sync::Arc<dyn ui::notify::Notifier>,
    background: std::sync::Arc<std::sync::atomic::AtomicBool>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
}

/// One surface's side of an [`AppNotifier`].
pub struct AppInbox {
    queue: Queue,
    inboxes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// The banner each application's tag shows, so showing a tag again replaces it.
    shown: HashMap<(String, String), u64>,
    /// The activation URI each shown notification carries.
    activations: HashMap<u64, String>,
}

impl Drop for AppInbox {
    fn drop(&mut self) {
        self.inboxes
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

impl AppNotifier {
    /// `background` is set by the host while the page cannot be seen and the person allows
    /// `forward` 's notifications; `wake` schedules every surface's next update.
    pub fn new(
        forward: std::sync::Arc<dyn ui::notify::Notifier>,
        background: std::sync::Arc<std::sync::atomic::AtomicBool>,
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            queue: Queue::default(),
            inboxes: std::sync::Arc::default(),
            forward,
            background,
            wake,
        }
    }

    /// A surface's inbox of this notifier's requests.
    pub fn inbox(&self) -> AppInbox {
        self.inboxes
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        AppInbox {
            queue: std::sync::Arc::clone(&self.queue),
            inboxes: std::sync::Arc::clone(&self.inboxes),
            shown: HashMap::new(),
            activations: HashMap::new(),
        }
    }

    fn attached(&self) -> bool {
        self.inboxes.load(std::sync::atomic::Ordering::Acquire) > 0
    }

    fn push(&self, request: AppRequest) {
        self.queue.lock_recover().push((request, 0));
        (self.wake)();
    }
}

impl ui::notify::Notifier for AppNotifier {
    fn show(&self, notification: &ui::notify::Notification) -> Result<(), ui::notify::NotifyError> {
        if !self.attached() {
            return self.forward.show(notification);
        }
        if notification.tag.is_empty() {
            return Err(ui::notify::NotifyError::Invalid(
                "a notification needs a tag",
            ));
        }
        if notification.title.trim().is_empty() {
            return Err(ui::notify::NotifyError::Invalid(
                "a notification needs a title",
            ));
        }
        if self.background.load(std::sync::atomic::Ordering::Acquire) {
            // The desktop keeps it either way; the platform copy is for a page nobody is looking
            // at.
            let _ = self.forward.show(notification);
        }
        self.push(AppRequest::Show {
            tab: ui::widget::panic_scope::current(),
            notification: notification.clone(),
        });
        Ok(())
    }

    fn withdraw(&self, tag: &str) -> Result<(), ui::notify::NotifyError> {
        if !self.attached() {
            return self.forward.withdraw(tag);
        }
        if tag.is_empty() {
            return Err(ui::notify::NotifyError::Invalid(
                "a notification needs a tag",
            ));
        }
        let _ = self.forward.withdraw(tag);
        self.push(AppRequest::Withdraw {
            tab: ui::widget::panic_scope::current(),
            tag: tag.to_owned(),
        });
        Ok(())
    }
}

/// The application a window runs: its id, display name and workspace.
pub(super) type AppOf<'a> = &'a dyn Fn(u64) -> Option<(String, String, Option<u64>)>;

impl AppInbox {
    /// Show and withdraw what applications asked since the last call. `app_of` names the
    /// application a window of this surface runs; a request from a window it does not show is left
    /// for the surface that does. Returns whether anything changed.
    pub(super) fn drain(
        &mut self,
        notifications: &DesktopNotifications,
        app_of: AppOf<'_>,
    ) -> bool {
        let requests = std::mem::take(&mut *self.queue.lock_recover());
        let mut changed = false;
        let mut passed = Vec::new();
        for (request, passes) in requests {
            let tab = match &request {
                AppRequest::Show { tab, .. } | AppRequest::Withdraw { tab, .. } => *tab,
            };
            let app = tab.and_then(app_of);
            if let (Some(_), None) = (tab, &app)
                && passes < UNCLAIMED_PASSES
            {
                passed.push((request, passes.saturating_add(1)));
                continue;
            }
            changed = true;
            let app_id = app.as_ref().map(|(id, ..)| id.clone()).unwrap_or_default();
            match request {
                AppRequest::Show { notification, .. } => {
                    let key = (app_id, notification.tag.clone());
                    if let Some(earlier) = self.shown.remove(&key) {
                        notifications.withdraw(earlier);
                        self.activations.remove(&earlier);
                    }
                    let mut shown = Notification::new(notification.title, notification.body);
                    if let Some((id, name, workspace)) = app {
                        shown.app_id = Some(id);
                        shown.app_name = Some(name);
                        shown.source_workspace = workspace;
                    }
                    shown.severity = match notification.severity {
                        ui::notify::Severity::Info => NotificationSeverity::Info,
                        ui::notify::Severity::Warning => NotificationSeverity::Warning,
                        ui::notify::Severity::Error => NotificationSeverity::Error,
                    };
                    if let Some(uri) = notification.activation {
                        // A click opens it; an activation the app named ("Try again") is a button
                        // too.
                        if let Some(label) = notification.action {
                            shown.actions = vec![("default".into(), label)];
                        }
                        self.activations.insert(shown.id, uri);
                    }
                    self.shown.insert(key, notifications.post(shown));
                }
                AppRequest::Withdraw { tag, .. } => {
                    if let Some(id) = self.shown.remove(&(app_id, tag)) {
                        notifications.withdraw(id);
                        self.activations.remove(&id);
                    }
                }
            }
        }
        if !passed.is_empty() {
            self.queue.lock_recover().extend(passed);
        }
        changed
    }

    /// The activation URI notification `id` carries, once.
    pub fn take_activation(&mut self, id: u64) -> Option<String> {
        self.shown.retain(|_, shown| *shown != id);
        self.activations.remove(&id)
    }
}

/// The banner stack and, while open, the centre panel: drawn over the windows, by the host that
/// stands above them.
pub struct NotificationLayer {
    notifications: DesktopNotifications,
    stack: NotificationStack,
    center: Option<Centre>,
    /// The person asked for the centre: it takes input and its arrivals join it. A centre that
    /// closes stays here until its leaving is drawn.
    centre_open: bool,
    /// The centre's coming and going, on the standard appear motion: from the corner under the
    /// bar's bell and back to it.
    appear: ui::motion::Appearing,
    /// Where the centre stands while it is open, as the stack of surfaces over the windows places
    /// it: the panel takes its width and top, and as much of its height as its rows ask.
    place: Option<Rect>,
    rect: Rect,
    cursor: Option<Point>,
    /// The caption and label line boxes the centre lays out with, captured from the theme each
    /// paint so input between paints measures the same type.
    type_lines: CenterMetrics,
    drawn_revision: Cell<Option<u64>>,
    /// Input changed what the layer shows; its layout (one full-size leaf) never changes, so this
    /// only asks for a repaint.
    dirty: bool,
}

impl NotificationLayer {
    pub fn new(notifications: DesktopNotifications) -> Self {
        Self {
            notifications,
            stack: NotificationStack::new(),
            center: None,
            centre_open: false,
            appear: ui::motion::Appearing::closed(),
            place: None,
            rect: Rect::ZERO,
            cursor: None,
            type_lines: CenterLayout::metrics(&ui::Theme::dark(), &mut ui::TextRenderer::new()),
            drawn_revision: Cell::new(None),
            dirty: true,
        }
    }

    pub fn notifications(&self) -> &DesktopNotifications {
        &self.notifications
    }

    /// Hand the stack the banners shown now: none while the host shows them elsewhere, and none
    /// while the centre is open — the person is reading the centre, so no banner stands over it and
    /// takes the press meant for a row.
    fn feed_stack(&mut self) {
        let feed =
            if self.notifications.centre_open() || self.notifications.log.banners_elsewhere.get() {
                BannerFeed::default()
            } else {
                BannerFeed::of(self.notifications.host())
            };
        self.stack.set_feed(feed);
    }

    /// Carry out what the person did with the banners.
    fn answer_stack(&mut self) {
        let asks = self.stack.take_asks();
        if asks.is_empty() {
            return;
        }
        let host = self.notifications.host();
        for ask in asks {
            ask.answer(host);
        }
        self.feed_stack();
    }

    /// Stand the centre at `place` , where the stack of surfaces over the windows puts it.
    pub fn set_place(&mut self, place: Option<Rect>) {
        if self.place != place {
            self.place = place;
            self.dirty = true;
        }
    }

    /// Where the centre's panel is drawn now, while it is open.
    #[must_use]
    pub fn panel(&self) -> Option<Rect> {
        self.centre_open
            .then(|| self.layout().map(|layout| layout.panel()))
            .flatten()
    }

    /// Open or close the centre. Either way the person has seen what it holds: nothing is unread
    /// after. Every close — Escape, a press outside, an app opened from a row — comes through here,
    /// since the arrivals read its state: a centre that only appeared to close would keep every
    /// later banner off the desk.
    pub fn set_center_open(&mut self, open: bool) {
        if open == self.centre_open {
            return;
        }
        self.centre_open = open;
        self.notifications.mark_read();
        self.notifications.set_centre_open(open);
        if open {
            self.center.get_or_insert_with(|| {
                Centre::new(&NOTIFICATION_CENTRE, (), |make, ()| make.make())
            });
        }
        // Closed, it is drawn until its leaving is over and dropped then.
        self.appear.show(open);
        self.dirty = true;
        self.feed_stack();
    }

    /// Open the centre waiting on its code once the code is in. Whether it opened just now.
    fn adopt_centre(&mut self) -> bool {
        let opened = Centre::poll(&mut self.center, &NOTIFICATION_CENTRE, |make, ()| {
            make.make()
        });
        self.dirty |= opened;
        opened
    }

    /// The open centre, once its code is in.
    fn centre(&self) -> Option<&dyn CentreApi> {
        Some(&**self.center.as_ref()?.ready()?)
    }

    /// The open centre, once its code is in.
    fn centre_mut(&mut self) -> Option<&mut (dyn CentreApi + 'static)> {
        Some(&mut **self.center.as_mut()?.ready_mut()?)
    }

    /// Whether the centre is open, as opposed to leaving: it takes input and what arrives joins it.
    pub fn center_open(&self) -> bool {
        self.centre_open
    }

    /// The centre panel: where the stack placed it, as tall as its rows ask within that place.
    fn layout(&self) -> Option<CenterLayout> {
        let center = self.center.as_ref()?;
        let place = self.place?;
        let width = place.width().max(0.0);
        let max = place.height().max(0.0);
        let height = center
            .ready()
            .map_or_else(
                || LOADING_HEIGHT.min(max),
                |center| {
                    center.fitted_height(self.notifications.host(), &self.type_lines, width, max)
                },
            )
            .max(0.0);
        Some(CenterLayout::new(
            Rect::new(place.x0, place.y0, place.x1, place.y0 + height),
            &self.type_lines,
        ))
    }

    pub(super) fn apply(&mut self, outcome: CenterOutcome) -> bool {
        match outcome {
            CenterOutcome::Ignored => return false,
            CenterOutcome::Handled => {}
            CenterOutcome::Back => self.set_center_open(false),
            CenterOutcome::OpenApp { app_id, .. } => {
                *self.notifications.log.open_app.borrow_mut() = Some(app_id);
                self.notifications.log.touch();
                self.set_center_open(false);
            }
        }
        self.dirty = true;
        true
    }

    /// Route `event` ; returns whether the layer consumed it.
    pub fn route(&mut self, event: &WindowEvent, scale: f64) -> bool {
        self.route_with_shift(event, scale, false)
    }

    fn route_with_shift(&mut self, event: &WindowEvent, scale: f64, backwards: bool) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(position.x / scale, position.y / scale));
            if self.center.is_some() {
                // Hover lights the centre's controls.
                self.dirty = true;
            }
        }
        let notifications = self.notifications.clone();
        let host = notifications.host();
        // A centre that is leaving takes no press: it goes to what is under it.
        if !self.centre_open {
            let mut handled = false;
            self.feed_stack();
            self.dirty |= self.stack.handle_pointer_event(event, scale, &mut handled);
            self.answer_stack();
            return handled;
        }
        self.stack.cancel_pointer_press();
        if self.stack.wants_pointer_capture()
            && !matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            let mut handled = false;
            self.feed_stack();
            self.dirty |= self.stack.handle_pointer_event(event, scale, &mut handled);
            self.answer_stack();
            if handled {
                return true;
            }
        }
        let layout = self.layout();
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(p) = self.cursor else {
                    return false;
                };
                if let (Some(center), Some(layout)) = (self.center.as_mut(), layout) {
                    if layout.panel().contains(p) {
                        if let Some(center) = center.ready_mut() {
                            let outcome = center.click(host, &layout, p);
                            self.apply(outcome);
                        }
                        return true;
                    }
                    // A press outside the open panel closes it and goes no further, as a popover's
                    // does.
                    self.set_center_open(false);
                    return true;
                }
                false
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let cursor = self.cursor;
                let (Some(center), Some(layout), Some(p)) = (self.centre_mut(), layout, cursor)
                else {
                    return false;
                };
                if !layout.panel().contains(p) {
                    return false;
                }
                center.wheel(host, &layout, *delta, scale);
                self.dirty = true;
                true
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let Some(layout) = layout else {
                    return false;
                };
                let Some(center) = self.centre_mut() else {
                    // Escape closes the centre whose code is still coming.
                    let escape = event.logical_key
                        == ui::winit::keyboard::Key::Named(ui::winit::keyboard::NamedKey::Escape);
                    return escape && self.apply(CenterOutcome::Back);
                };
                let outcome = center.key(host, &layout, &event.logical_key, backwards);
                self.apply(outcome)
            }
            _ => false,
        }
    }

    /// Whether the layer shows anything at all.
    fn visible(&self) -> bool {
        self.center.is_some()
            || self.appear.is_moving()
            || (!self.notifications.log.banners_elsewhere.get()
                && !self.notifications.log().active().is_empty())
    }
}

impl Widget for NotificationLayer {
    fn name(&self) -> &'static str {
        "DesktopNotifications"
    }
    fn needs_redraw(&self) -> bool {
        self.dirty || self.drawn_revision.get() != Some(self.notifications.revision())
    }
    fn is_layout_dirty(&self) -> bool {
        false
    }
    fn needs_animation_frame(&self) -> bool {
        self.stack.needs_animation_frame()
            || self.appear.is_moving()
            || self.centre().is_some_and(CentreApi::reading_needs_frame)
    }
    /// The reading page glides on after a flick, repainted as a wheel's scroll is; that changes no
    /// model.
    fn update(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) -> bool {
        self.feed_stack();
        self.adopt_centre();
        if let Some(center) = self.centre_mut() {
            self.dirty |= center.advance_reading();
        }
        false
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        let banner = self.stack.next_wake_deadline();
        let undo = self.notifications.log().clear_deadline();
        banner.into_iter().chain(undo).min()
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _theme: &ui::Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree
            .new_leaf(overlay_style())
            .unwrap_or_else(|_| NodeId::from(0u64));
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    ui::rect_geometry!();
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        self.type_lines = CenterLayout::metrics(theme, text);
        self.adopt_centre();
        let notifications = self.notifications.clone();
        let host = notifications.host();
        self.stack.set_viewport(self.rect);
        self.stack.set_clear_top(notifications.log.clear_top.get());
        self.feed_stack();
        self.stack.draw(scene, transform, text, theme);
        self.answer_stack();
        // The centre arrives from the corner under the bar's bell and leaves back to it; one whose
        // leaving is over is dropped here.
        self.appear.follow(self.centre_open, theme.motion());
        if !self.centre_open && !self.appear.is_drawn() {
            self.center = None;
        }
        let cursor = self.cursor;
        let posed = self.layout().map(|layout| {
            let panel = layout.panel();
            // Under the bell: the panel's own top-right corner.
            let anchor = Point::new(panel.x1, panel.y0);
            let pose = self.appear.pose(panel, anchor);
            (layout, panel, pose)
        });
        if let Some((layout, panel, pose)) = posed
            && let Some(center) = self.center.as_mut()
        {
            let elevation = &theme.tokens.elevation.floating;
            let reach = f64::from(elevation.blur) * 2.0 + f64::from(elevation.offset_y).abs() + 2.0;
            pose.paint(
                scene,
                transform,
                panel.inflate(reach, reach),
                |scene, at| {
                    ui::surface::panel(
                        scene,
                        at,
                        panel,
                        f64::from(theme.tokens.radii.control),
                        theme,
                    );
                    match center.ready_mut() {
                        Some(center) => {
                            center.draw(host, &layout, scene, at, text, theme, cursor);
                        }
                        None => {
                            ui::lazy_code::draw_loading(
                                scene,
                                at,
                                text,
                                theme,
                                panel,
                                "lazy-loading",
                            );
                        }
                    }
                },
            );
        }
        self.drawn_revision.set(Some(notifications.revision()));
        self.dirty = false;
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !*ctx.handled
            && let Some(center) = self.centre_mut()
        {
            self.dirty |= center.reader_event(event, ctx);
        }
        if !*ctx.handled
            && self.route_with_shift(event, ctx.scale_factor, ctx.modifiers.state().shift_key())
        {
            *ctx.handled = true;
        }
    }
    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.centre()?.reading_scroll()
    }
    /// A banner's control pressed by assistive technology, as a click does.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        self.feed_stack();
        let pressed = self.stack.perform_semantic(target, action);
        self.answer_stack();
        self.dirty |= pressed;
        pressed
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        let result = self
            .centre_mut()
            .map_or(ui::widget::PrimaryScrollResult::Unsupported, |center| {
                center.apply_reading_scroll(action)
            });
        self.dirty |= matches!(result, ui::widget::PrimaryScrollResult::Changed(_));
        result
    }
    /// The live region announcing banners, holding the centre's dialog while it is open. Absent
    /// while there is nothing to announce or show.
    fn accessibility(&self) -> Option<AccessNode> {
        let host = self.notifications.host();
        if !self.visible() {
            return None;
        }
        let mut node = self.stack.accessibility()?;
        if let (Some(center), Some(layout)) = (self.centre(), self.layout()) {
            // The dialog stays the first child: its synthetic rects are looked up by that path.
            node.children
                .insert(0, center.accessibility(host, &layout, ui::time::now()));
        }
        Some(node)
    }
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [0, dialog @ ..] = path else {
            return None;
        };
        let (center, layout) = (self.centre()?, self.layout()?);
        center.accessibility_rect_for_path(self.notifications.host(), &layout, dialog)
    }
    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [0, dialog @ ..] = path else {
            return false;
        };
        let notifications = self.notifications.clone();
        let Some(layout) = self.layout() else {
            return false;
        };
        let Some(center) = self.centre_mut() else {
            return false;
        };
        let focused = center.focus_accessibility_path(notifications.host(), &layout, dialog);
        self.dirty |= focused;
        focused
    }
}

/// A leaf over the whole surface, out of the strip's flow.
fn overlay_style() -> ui::taffy::Style {
    ui::taffy::Style {
        position: ui::taffy::Position::Absolute,
        inset: ui::taffy::Rect {
            left: ui::taffy::LengthPercentageAuto::length(0.0),
            top: ui::taffy::LengthPercentageAuto::length(0.0),
            right: ui::taffy::LengthPercentageAuto::auto(),
            bottom: ui::taffy::LengthPercentageAuto::auto(),
        },
        size: ui::taffy::Size {
            width: ui::taffy::Dimension::percent(1.0),
            height: ui::taffy::Dimension::percent(1.0),
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::DrawChild;

    use crate::notifications::{RemoteNotificationRoute, RowShape};

    /// Where the stack of surfaces over the windows stands the centre on a 1280 by 800 display.
    const PLACE: Rect = Rect::new(892.0, 32.0, 1272.0, 792.0);

    fn layer_with(count: usize) -> (NotificationLayer, DesktopNotifications) {
        let notifications = DesktopNotifications::default();
        for i in 0..count {
            notifications.post(
                Notification::new(format!("n{i}"), "body")
                    .with_app_name("Clocks")
                    .with_actions(i),
            );
        }
        let mut layer = NotificationLayer::new(notifications.clone());
        layer.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        layer.set_place(Some(PLACE));
        (layer, notifications)
    }

    trait WithActions {
        fn with_actions(self, i: usize) -> Self;
    }
    impl WithActions for Notification {
        fn with_actions(mut self, i: usize) -> Self {
            self.app_id = Some("so.tilo.clocks".into());
            if i.is_multiple_of(2) {
                self.actions = vec![("default".into(), "Open".into())];
            }
            self
        }
    }

    fn press(layer: &mut NotificationLayer, p: Point) -> bool {
        layer.cursor = Some(p);
        layer.route(&ui::synthetic_input::mouse_press(), 1.0)
    }

    fn paint(layer: &mut NotificationLayer, theme: &ui::Theme) {
        layer.draw_child(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut ui::TextRenderer::new(),
            theme,
            &ui::presence::PresenceCtx::default(),
        );
    }

    /// The bell counts what arrived since the person last looked: opening the centre reads it all,
    /// and one taken back is no longer news.
    #[test]
    fn notifications_stay_unread_until_the_centre_opens() {
        let (mut layer, notifications) = layer_with(2);
        assert_eq!(notifications.unread(), 2);
        // Each row says whether it is news, by the same set the bell counts: the centre's rows and
        // its header never disagree.
        let ids: Vec<u64> = notifications
            .log()
            .history()
            .iter()
            .chain(notifications.log().active())
            .map(|notification| notification.id)
            .collect();
        assert!(ids.iter().all(|id| notifications.is_unread(*id)));
        let revision = notifications.revision();
        layer.set_center_open(true);
        assert_eq!(notifications.unread(), 0, "opening the centre reads them");
        assert!(
            ids.iter().all(|id| !notifications.is_unread(*id)),
            "every row is read once the centre opened"
        );
        assert_ne!(notifications.revision(), revision, "the bell repaints");
        layer.set_center_open(false);
        let fresh = notifications.post(Notification::new("Grace renamed it", ""));
        assert_eq!(notifications.unread(), 1, "a new one is unread");
        assert!(notifications.withdraw(fresh));
        assert_eq!(notifications.unread(), 0, "a withdrawn one is not news");
    }

    /// A notification another of the person's devices projected updates the row it already shows,
    /// and its live and kept copies go together when that device is done with it.
    #[test]
    fn another_devices_projection_updates_in_place_and_retires_together() {
        let notifications = DesktopNotifications::default();
        let id = ContinuityNotificationId {
            epoch: [3; 16],
            token: [4; 16],
        };
        let route = RemoteNotificationRoute {
            source_device: "laptop".into(),
            source_label: "Laptop".into(),
            notification_id: id,
        };
        let mut first =
            Notification::new("New mail", "From Ada").with_app_name("Mail · From Laptop");
        first.remote_route = Some(route.clone());
        let shown = notifications.post(first);
        let mut update = Notification::new("Two new mails", "From Ada and Grace")
            .with_app_name("Mail · From Laptop");
        update.remote_route = Some(route);
        assert_eq!(
            notifications.replace_remote(&update),
            Some(shown),
            "the row it already shows takes the update"
        );
        assert_eq!(notifications.counts(), (1, 0), "one row, not two");
        assert_eq!(notifications.log().active()[0].title, "Two new mails");

        notifications.host().dismiss(shown);
        assert_eq!(notifications.counts(), (0, 1));
        assert!(notifications.retire_remote("laptop", id));
        assert_eq!(notifications.counts(), (0, 0), "the kept copy goes too");
        assert!(!notifications.is_unread(shown));
        assert!(
            !notifications.retire_remote("laptop", id),
            "nothing left to take away"
        );
        assert!(
            !notifications.retire_remote("desk", id),
            "another device's id is not this one's"
        );
    }

    /// Escape closes the centre the way a press outside does, and arrivals read the same state
    /// either way: the one after it is a banner on the desk again, not another row behind a centre
    /// that is no longer there.
    #[test]
    fn escape_closes_the_centre_for_arrivals_too() {
        let (mut layer, notifications) = layer_with(2);
        layer.set_center_open(true);
        assert!(
            notifications.centre_open(),
            "the arrivals join the open centre"
        );
        let escape = ui::synthetic_input::named_key(ui::winit::keyboard::NamedKey::Escape);
        assert!(layer.route(&escape, 1.0), "the centre takes the key");
        assert!(!layer.center_open(), "the centre is gone");
        assert!(
            !notifications.centre_open(),
            "and the next arrival is a banner again"
        );
    }

    /// The shell draws its own centre over the desk: a banner up when it opens goes into it, what
    /// arrives joins it, and once the shell says it closed, the desk's own panel is not opened in
    /// its place.
    #[test]
    fn the_shells_centre_holds_arrivals_without_opening_the_desks_panel() {
        let (layer, notifications) = layer_with(1);
        notifications.set_shell_centre(true);
        assert!(notifications.centre_open());
        assert!(
            notifications.log().active().is_empty(),
            "the banner up went into it rather than covering it"
        );
        notifications.set_shell_centre(false);
        assert!(!notifications.centre_open());
        assert!(
            !layer.center_open(),
            "the shell's centre never stands the desk's panel up"
        );
    }

    #[test]
    fn banners_retire_into_the_centre_and_it_closes_on_an_outside_press() {
        let (mut layer, notifications) = layer_with(2);
        let id = notifications.log().active()[0].id;
        notifications.host().dismiss(id);
        assert_eq!(notifications.counts(), (1, 1));
        layer.set_center_open(true);
        let panel = layer.layout().unwrap().panel();
        assert_eq!(
            (panel.x0, panel.y0, panel.x1),
            (PLACE.x0, PLACE.y0, PLACE.x1),
            "the centre stands where the stack placed it"
        );
        assert!(
            panel.y1 <= PLACE.y1,
            "as tall as its rows, within its place"
        );
        let node = layer.accessibility().unwrap();
        assert_eq!(node.children.len(), 1, "the centre's dialog is exposed");
        assert!(press(&mut layer, Point::new(10.0, 700.0)));
        assert!(!layer.center_open());
        assert!(
            !notifications.centre_open(),
            "a centre closed by a press outside no longer holds arrivals: the next one is a banner"
        );
        assert!(
            notifications.take_outcomes().is_empty(),
            "closing a banner answers nobody"
        );
    }

    /// A host showing the banners in a surface of its own feeds it and answers it through the
    /// notifications; the layer shows only the centre, and the host hears each outcome as it
    /// happens.
    #[test]
    fn banners_shown_elsewhere_leave_the_layer_only_the_centre() {
        let (layer, notifications) = layer_with(2);
        notifications.show_banners_elsewhere();
        let heard = Rc::new(RefCell::new(Vec::new()));
        let tap = Rc::clone(&heard);
        notifications.on_outcome(move |outcome| {
            if let NotificationOutcome::Activated { action, .. } = outcome {
                tap.borrow_mut().push(action.clone());
            }
        });
        assert!(!layer.visible(), "no banners on the surface");
        let feed = notifications.feed();
        assert_eq!(feed.banners.len(), 2, "the host's own surface gets them");
        let chosen = feed.banners[0].notification.id;
        notifications.answer(crate::notifications::BannerAsk::Activate(chosen));
        assert_eq!(*heard.borrow(), [Some("default".to_owned())]);
        assert_eq!(notifications.take_outcomes().len(), 1, "and still queued");
        notifications.host().open_center();
        assert_eq!(
            notifications.take_centre_ask(),
            Some(true),
            "the host asks for the centre, which whoever draws it opens"
        );
    }

    /// The person opened the centre to read what it holds: no banner stands over the rows it
    /// covers, wherever the host draws the banners, what arrives meanwhile goes into the centre,
    /// and the ones that came before are rows of it too — a banner is gone from the desk once the
    /// centre took it.
    #[test]
    fn a_banner_never_stands_over_the_open_centre() {
        let (mut layer, notifications) = layer_with(2);
        notifications.show_banners_elsewhere();
        layer.feed_stack();
        assert_eq!(
            notifications.feed().banners.len(),
            2,
            "the host's own surface shows the banners"
        );
        layer.set_center_open(true);
        assert_eq!(
            notifications.feed().banners.len(),
            0,
            "nothing stands over the centre"
        );
        assert_eq!(layer.stack.feed().banners.len(), 0);
        // One arriving now belongs to the rows the person is reading.
        notifications.post(Notification::new("Kaffee ist fertig", ""));
        assert_eq!(
            notifications.feed().banners.len(),
            0,
            "it does not become a banner"
        );
        assert!(
            notifications
                .log()
                .history()
                .iter()
                .any(|notification| notification.title == "Kaffee ist fertig"),
            "it is a row of the centre"
        );
        layer.set_center_open(false);
        layer.feed_stack();
        assert_eq!(
            notifications.feed().banners.len(),
            0,
            "the banners the centre took stay rows of the centre"
        );
        assert!(
            notifications.log().history().len() >= 3,
            "all three are rows the person reads there"
        );
        // What arrives once it is closed is a banner again.
        notifications.post(Notification::new("Das Wasser kocht", ""));
        assert_eq!(
            notifications.feed().banners.len(),
            1,
            "a desk with no centre over it shows banners again"
        );
    }

    /// The centre arrives and leaves on the standard motion: closed, it is still drawn while it
    /// goes and takes no press meanwhile.
    #[test]
    fn the_centre_is_drawn_until_its_leaving_is_over() {
        let (mut layer, _) = layer_with(2);
        let theme = ui::Theme::dark();
        // Opened and read at rest: from there the leaving has somewhere to start.
        layer.set_center_open(true);
        layer.appear.snap(true);
        paint(&mut layer, &theme);
        assert!(layer.center_open());
        assert!(layer.visible());
        let panel = layer.layout().expect("an open centre has a panel").panel();
        // A press asked for it: the motion, not a key's landing at once.
        ui::motion::note_source(ui::tokens::MotionSource::Pointer);
        let frozen = ui::motion::testing::frozen();
        layer.set_center_open(false);
        paint(&mut layer, &theme);
        assert!(layer.center.is_some(), "a leaving centre is still drawn");
        assert!(!layer.center_open());
        assert!(
            !press(&mut layer, panel.center()),
            "a leaving centre takes no press"
        );
        drop(frozen);
        let _settled = ui::motion::testing::settled();
        paint(&mut layer, &theme);
        assert!(layer.center.is_none(), "gone once its leaving is over");
    }

    #[test]
    fn nothing_is_exposed_or_consumed_without_notifications() {
        let (mut layer, _) = layer_with(0);
        assert!(layer.accessibility().is_none());
        assert!(!press(&mut layer, Point::new(1200.0, 80.0)));
    }

    #[test]
    fn portable_banner_clicks_complete_cancel_and_yield_to_history() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        for interrupted in [false, true] {
            let (mut layer, notifications) = layer_with(1);
            let theme = ui::Theme::dark().with_reduce_motion(true);
            layer.draw_child(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut ui::TextRenderer::new(),
                &theme,
                &ui::presence::PresenceCtx::default(),
            );
            let point = layer.stack.rect().center();
            layer.route(&ui::synthetic_input::cursor_moved(point.x, point.y), 1.0);
            assert!(press(&mut layer, point));
            assert!(
                notifications.take_outcomes().is_empty(),
                "pressing is not choosing"
            );
            if interrupted {
                layer.set_center_open(true);
            }
            assert!(layer.route(&ui::synthetic_input::mouse_release(), 1.0));
            let outcomes = notifications.take_outcomes();
            if interrupted {
                assert!(
                    outcomes.is_empty(),
                    "a banner the centre retired answers nobody"
                );
                assert_eq!(
                    notifications.counts(),
                    (0, 1),
                    "a banner up when the centre opens goes into it"
                );
                assert!(layer.center_open());
            } else {
                assert!(
                    matches!(&outcomes[..], [NotificationOutcome::Activated { action, .. }]
                    if action.as_deref() == Some("default"))
                );
                assert_eq!(notifications.counts(), (0, 1));
            }
        }
    }

    #[test]
    fn choosing_a_banner_hands_its_default_action_to_the_host() {
        let (_, notifications) = layer_with(1);
        let id = notifications.log().active()[0].id;
        notifications.host().activate(id);
        let outcomes = notifications.take_outcomes();
        assert!(
            matches!(&outcomes[..], [NotificationOutcome::Activated { notification, action }]
                if notification.id == id && action.as_deref() == Some("default")),
            "{outcomes:?}"
        );
        assert_eq!(
            notifications.counts(),
            (0, 1),
            "a chosen banner is retained"
        );
    }

    #[test]
    fn a_retained_notification_without_actions_opens_its_app_and_stays_until_dismissed() {
        let (mut layer, notifications) = layer_with(2);
        let quiet = notifications.log().active()[1].id;
        notifications.host().dismiss(quiet);
        assert!(matches!(
            notifications.host().activate_history(quiet),
            HistoryActivation::OpenApp { ref app_id, .. } if app_id == "so.tilo.clocks"
        ));
        let banner = notifications.log().active()[0].id;
        layer.set_center_open(true);
        // Opening the centre retired the banner still up into it, in the same app's group: one row
        // for the app, and dismissing it takes both.
        assert_eq!(
            notifications.log().history().len(),
            2,
            "the retired banner and the kept one"
        );
        let layout = layer.layout().unwrap();
        let dismiss = layout.row_controls(layout.row(0), &RowShape::default());
        assert!(press(&mut layer, dismiss.center()));
        assert_eq!(notifications.log().history().len(), 0);
        // Both rows the app's one row held go: the banner retirement put in the centre, and the one
        // kept there all along.
        let gone: Vec<u64> = notifications
            .take_outcomes()
            .into_iter()
            .filter_map(|outcome| match outcome {
                NotificationOutcome::Dismissed { id } => Some(id),
                NotificationOutcome::Activated { .. } => None,
            })
            .collect();
        assert_eq!(gone.len(), 2, "one dismissal per row the app's row held");
        assert!(
            gone.contains(&banner) && gone.contains(&quiet),
            "the retired banner and the kept notification are dismissed with their app's row: {gone:?}"
        );
    }

    #[derive(Default)]
    struct Platform(std::sync::Mutex<Vec<String>>);
    impl ui::notify::Notifier for Platform {
        fn show(&self, n: &ui::notify::Notification) -> Result<(), ui::notify::NotifyError> {
            self.0.lock().unwrap().push(n.title.clone());
            Ok(())
        }
        fn withdraw(&self, _: &str) -> Result<(), ui::notify::NotifyError> {
            Ok(())
        }
    }

    fn notifier() -> (
        AppNotifier,
        std::sync::Arc<Platform>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let platform = std::sync::Arc::new(Platform::default());
        let background = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notifier = AppNotifier::new(
            platform.clone(),
            std::sync::Arc::clone(&background),
            std::sync::Arc::new(|| {}),
        );
        (notifier, platform, background)
    }

    fn clocks(tab: u64) -> Option<(String, String, Option<u64>)> {
        (tab == 7).then(|| ("so.tilo.clocks".to_owned(), "Clocks".to_owned(), Some(1)))
    }

    #[test]
    fn an_apps_notifications_show_on_the_desktop_attributed_to_its_window() {
        use ui::notify::Notifier as _;
        let (notifier, platform, background) = notifier();
        let mut inbox = notifier.inbox();
        let notifications = DesktopNotifications::default();
        {
            let _running = ui::widget::panic_scope::enter(Some(7));
            notifier
                .show(
                    &ui::notify::Notification::new("timer", "Tea", "Steeped")
                        .with_activation("tilo-clocks:timer/1"),
                )
                .unwrap();
            notifier
                .show(&ui::notify::Notification::new("timer", "Tea", "Ready"))
                .unwrap();
        }
        assert!(inbox.drain(&notifications, &clocks));
        let log = notifications.log();
        assert_eq!(log.active().len(), 1, "the same tag replaces its banner");
        let shown = &log.active()[0];
        assert_eq!(shown.body, "Ready");
        assert_eq!(shown.app_id.as_deref(), Some("so.tilo.clocks"));
        assert_eq!(shown.app_name.as_deref(), Some("Clocks"));
        drop(log);
        assert!(
            platform.0.lock().unwrap().is_empty(),
            "a page in view keeps them on its desktop"
        );

        background.store(true, std::sync::atomic::Ordering::Release);
        {
            let _running = ui::widget::panic_scope::enter(Some(7));
            notifier
                .show(
                    &ui::notify::Notification::new("alarm", "Alarm", "07:00")
                        .with_activation("tilo-clocks:alarm/1")
                        .with_action("Stop"),
                )
                .unwrap();
        }
        inbox.drain(&notifications, &clocks);
        assert_eq!(
            *platform.0.lock().unwrap(),
            ["Alarm"],
            "a hidden page's go to the platform too"
        );
        let alarm = notifications.log().active()[1].clone();
        assert_eq!(alarm.default_action_key().as_deref(), Some("default"));
        assert_eq!(
            alarm.actions[0].1, "Stop",
            "the button says what choosing it does"
        );
        assert_eq!(
            inbox.take_activation(alarm.id).as_deref(),
            Some("tilo-clocks:alarm/1")
        );
        {
            let _running = ui::widget::panic_scope::enter(Some(7));
            notifier.withdraw("timer").unwrap();
        }
        inbox.drain(&notifications, &clocks);
        assert_eq!(notifications.counts(), (1, 0));
    }

    #[test]
    fn another_surfaces_window_is_left_to_it_and_no_surface_means_the_platform() {
        use ui::notify::Notifier as _;
        let (notifier, platform, _) = notifier();
        {
            let inbox = notifier.inbox();
            drop(inbox);
        }
        notifier
            .show(&ui::notify::Notification::new("t", "Alone", ""))
            .unwrap();
        assert_eq!(*platform.0.lock().unwrap(), ["Alone"]);

        let mut ours = notifier.inbox();
        let mut theirs = notifier.inbox();
        let (mine, other) = (
            DesktopNotifications::default(),
            DesktopNotifications::default(),
        );
        {
            let _running = ui::widget::panic_scope::enter(Some(7));
            notifier
                .show(&ui::notify::Notification::new("t", "Theirs", ""))
                .unwrap();
        }
        assert!(!ours.drain(&mine, &|_| None), "not a window of ours");
        assert!(theirs.drain(&other, &clocks));
        assert_eq!((mine.counts(), other.counts()), ((0, 0), (1, 0)));
    }

    #[test]
    fn a_sender_withdraws_live_and_retained_notifications() {
        let (_, notifications) = layer_with(2);
        let live = notifications.log().active()[0].id;
        let retained = notifications.log().active()[1].id;
        notifications.host().dismiss(retained);
        assert!(notifications.withdraw(live));
        assert!(notifications.withdraw(retained));
        assert!(!notifications.withdraw(retained));
        assert_eq!(notifications.counts(), (0, 0));
    }
    fn reader_key_event(key: ui::winit::keyboard::NamedKey) -> WindowEvent {
        use ui::winit::event::{KeyEvent, RawKeyEvent};
        use ui::winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};
        let logical = Key::Named(key);
        WindowEvent::KeyboardInput {
            device_id: ui::winit::event::DeviceId::dummy(),
            event: KeyEvent::synthetic(
                RawKeyEvent {
                    physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                    state: ElementState::Pressed,
                },
                logical.clone(),
                None,
                KeyLocation::Standard,
                false,
                None,
                logical,
            ),
            is_synthetic: true,
        }
    }

    #[test]
    fn portable_notification_reader_returns_to_history_before_closing() {
        use ui::winit::keyboard::NamedKey;
        let (mut layer, notifications) = layer_with(0);
        // Nothing to open and no app to go to: the row opens the reader.
        let id = notifications.post(Notification::new(
            "Backup couldn't finish",
            "Reconnect the disk and the next backup picks up where this one stopped.",
        ));
        notifications.host().dismiss(id);
        notifications.take_outcomes();
        layer.set_center_open(true);
        assert!(layer.route(&reader_key_event(NamedKey::Tab), 1.0));
        assert!(layer.route(&reader_key_event(NamedKey::Enter), 1.0));
        assert!(layer.centre().unwrap().is_reading());
        assert!(layer.route(&reader_key_event(NamedKey::Delete), 1.0));
        assert!(layer.route(&reader_key_event(NamedKey::Enter), 1.0));
        assert_eq!(notifications.log().history().len(), 1);
        assert!(notifications.take_outcomes().is_empty());
        assert!(layer.route(&reader_key_event(NamedKey::Escape), 1.0));
        assert!(layer.center_open());
        assert!(!layer.centre().unwrap().is_reading());
        assert!(layer.route(&reader_key_event(NamedKey::Enter), 1.0));
        assert!(layer.centre().unwrap().is_reading());
        let back = layer.layout().unwrap().reader_back();
        assert!(press(&mut layer, back.center()));
        assert!(layer.center_open());
        assert!(!layer.centre().unwrap().is_reading());
        assert!(layer.route(&reader_key_event(NamedKey::Escape), 1.0));
        assert!(!layer.center_open());
        assert!(notifications.take_outcomes().is_empty());
        assert!(notifications.take_open_app().is_none());
    }

    /// An ask as a host offers it: its answer, and "Not now" under it.
    fn ask() -> Notification {
        let mut ask = Notification::new(
            "Keep this identity?",
            "A passkey brings you back on any browser.",
        );
        ask.actions = vec![
            (
                crate::host_action_key("keep-identity"),
                "Keep with passkey".to_owned(),
            ),
            (
                crate::notifications::NOT_NOW_ACTION.to_owned(),
                crate::i18n::t("notification-not-now"),
            ),
        ];
        ask.icon_kind = crate::notifications::NotificationIconKind::Permission;
        ask.dismiss_after = crate::notifications::PERSISTENT_DISMISS;
        ask
    }

    #[test]
    fn an_ask_shows_not_now_then_its_answer() {
        let notifications = DesktopNotifications::default();
        notifications.post(ask());
        let notice = notifications.log().active()[0].clone();
        let keys: Vec<String> = notifications
            .host()
            .actions_for(&notice)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, ["not-now", "host:keep-identity"]);
        assert_eq!(
            notice.default_action_key().as_deref(),
            Some("host:keep-identity")
        );
        let plain = Notification::new("Tea", "Ready");
        assert!(notifications.host().actions_for(&plain).is_empty());
    }

    #[test]
    fn not_now_keeps_the_ask_in_the_centre_without_answering_it() {
        let notifications = DesktopNotifications::default();
        let id = notifications.post(ask());
        notifications.host().invoke_action(id, "not-now");
        assert!(notifications.take_outcomes().is_empty());
        assert_eq!(notifications.counts(), (0, 1));
        let activation = notifications.host().activate_history(id);
        assert!(matches!(activation, HistoryActivation::Invoked));
        assert!(matches!(
            &notifications.take_outcomes()[..],
            [NotificationOutcome::Activated { action: Some(action), .. }]
                if action == "host:keep-identity"
        ));
    }

    #[test]
    fn a_retained_rows_action_answers_its_sender_and_leaves_the_centre() {
        let notifications = DesktopNotifications::default();
        let ask = notifications.post(ask());
        notifications.host().invoke_action(ask, "not-now");
        let tea = notifications.post(Notification::new("Tea", "Ready"));
        notifications.host().dismiss(tea);
        notifications.take_outcomes();
        assert_eq!(notifications.counts(), (0, 2));
        notifications
            .host()
            .invoke_action(ask, "host:keep-identity");
        assert!(matches!(
            &notifications.take_outcomes()[..],
            [NotificationOutcome::Activated { action: Some(action), .. }]
                if action == "host:keep-identity"
        ));
        let left: Vec<u64> = notifications.log().history().iter().map(|n| n.id).collect();
        assert_eq!(left, [tea], "the answered row is gone, the other stays");
    }

    #[test]
    fn answering_an_ask_hands_its_action_to_the_host() {
        let notifications = DesktopNotifications::default();
        let id = notifications.post(ask());
        notifications.host().invoke_action(id, "host:keep-identity");
        assert!(matches!(
            &notifications.take_outcomes()[..],
            [NotificationOutcome::Activated { action: Some(action), .. }]
                if action == "host:keep-identity"
        ));
    }

    #[test]
    fn an_ask_for_an_action_no_longer_offered_is_withdrawn_everywhere() {
        let notifications = DesktopNotifications::default();
        notifications.post(ask());
        let put_off = notifications.post(ask());
        notifications.host().invoke_action(put_off, "not-now");
        let other = notifications.post(Notification::new("Tea", "Ready"));
        assert_eq!(notifications.counts(), (2, 1));
        assert!(notifications.withdraw_host_action("keep-identity"));
        assert_eq!(notifications.counts(), (1, 0));
        assert_eq!(notifications.log().active()[0].id, other);
        assert!(!notifications.withdraw_host_action("keep-identity"));
    }
}
