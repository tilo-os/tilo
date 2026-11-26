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

use ui::time::Instant;

use crate::desktop::notifications::NotificationHost;
use crate::desktop::notifications::{
    ContinuityNotificationId, HistoryActivation, Notification, NotificationLog,
    NotificationSeverity,
};
use tilo_lock::MutexRecover as _;

/// What the person did with a notification that its sender should hear.
#[derive(Clone, Debug)]
pub enum NotificationOutcome {
    /// The person chose the notification: its default action, or opening its application.
    Activated {
        notification: Box<Notification>,
        action: Option<String>,
    },
    /// The person closed a banner or removed a retained notification from the centre.
    Dismissed {
        id: u64,
        /// The daemon's notification the row stood for, when the log still knows it: the sender
        /// hears its notification is gone rather than having run out.
        sender: Option<u32>,
    },
}

#[derive(Default)]
pub struct NoticeState {
    pub log: RefCell<NotificationLog>,
    /// Bumped by every change, so the layer repaints only when one happened.
    pub revision: Cell<u64>,
    pub outcomes: RefCell<Vec<NotificationOutcome>>,
    /// The person asked the centre open or closed (the bell, a chord, the banner stack's "more"
    /// card): whoever draws it over the windows takes the ask on its next pass.
    pub centre_asked: Cell<Option<bool>>,
    /// An app a row of the centre asked to open: the shell focuses its window.
    pub open_app: RefCell<Option<String>>,
    /// Where the shown windows' content starts: banners stand below it.
    pub clear_top: Cell<f64>,
    /// Notifications posted since the person last looked at the centre, by id: what the bell
    /// counts.
    pub unread: RefCell<std::collections::BTreeSet<u64>>,
    /// The host shows the banners elsewhere (an app of their own); the surface shows only the
    /// centre.
    pub banners_elsewhere: Cell<bool>,
    /// The notification centre is open: no banner stands over it, and what arrives while it is open
    /// goes into it rather than onto the shell.
    pub centre_open: Cell<bool>,
    /// Told of each outcome as it happens.
    pub taps: RefCell<Vec<OutcomeTap>>,
    /// Why fresh banners are held right now (Do Not Disturb, a focus schedule, presenting), as the
    /// bar and the centre's header say it.
    pub quiet: RefCell<Option<String>>,
    /// What each app's badge shows, by the id the launcher attributed: its unread count where the
    /// person keeps its badges on, and none where they hide them.
    pub badges: RefCell<std::collections::BTreeMap<String, u32>>,
    /// The shell's own centre stands over the shell, as its reading says. Its own state: the shell
    /// knows when it closes, the shell's layer when its own does.
    shell_centre: Cell<bool>,
}

pub type OutcomeTap = Rc<dyn Fn(&NotificationOutcome)>;

impl NoticeState {
    pub fn touch(&self) {
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
        // The person closed the banner: its sender hears it was dismissed, not that it ran out.
        if let Some(notification) = self.retire_active(id) {
            self.outcome(NotificationOutcome::Dismissed {
                id,
                sender: notification.sender_id,
            });
        }
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
        let taken = {
            let mut log = self.log.borrow_mut();
            log.take_active(id).or_else(|| log.take_history(id))
        };
        if let Some(notification) = taken {
            self.outcome(NotificationOutcome::Dismissed {
                id,
                sender: notification.sender_id,
            });
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
        if key == crate::desktop::notifications::NOT_NOW_ACTION {
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
    pub log: Rc<NoticeState>,
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
    /// update is. Returns the id it shows under; `None` when the shell showed none from that origin
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
    /// live or retained: its origin is done with it. Returns whether the shell
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

    /// The centre stands open on the shell: what arrives joins its list, so no banner covers it. A
    /// banner up when it opens goes into it, since a stack over an open centre would take the
    /// presses meant for its rows. Opened or closed, the person has seen what it holds: nothing is
    /// unread after.
    pub fn set_centre_open(&self, open: bool) {
        if open == self.log.centre_open.get() {
            return;
        }
        self.mark_read();
        if open {
            self.retire_banners();
        }
        self.log.centre_open.set(open);
    }

    /// The shell's centre stands over the shell, or has closed: the shell says which, every pass,
    /// since it knows every way the person leaves it. What arrives joins the list it shows while it
    /// is up, and none covers it.
    pub fn set_shell_centre(&self, open: bool) {
        if open && !self.log.shell_centre.get() {
            self.retire_banners();
        }
        self.log.shell_centre.set(open);
    }

    /// Whether the centre is open, on the shell or over it.
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

    /// How many unread notifications the app with verified id `app_id` shows as its badge: its
    /// count where the person keeps its badges on, and none where they hide them.
    #[must_use]
    pub fn app_badge(&self, app_id: &str) -> u32 {
        self.log.badges.borrow().get(app_id).copied().unwrap_or(0)
    }

    /// Show each app's badge as `badges` says: its unread count where the person keeps its badges
    /// on, and none where they hide them. What every view of the badges reads changes with it.
    pub fn show_badges(&self, badges: std::collections::BTreeMap<String, u32>) {
        let mut held = self.log.badges.borrow_mut();
        if *held == badges {
            return;
        }
        *held = badges;
        drop(held);
        self.log.touch();
    }

    /// When the shell's notifications next change with nobody asking: a banner's dwell running out,
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
            .filter(|notification| !crate::desktop::state::is_host_offer(notification))
            .cloned()
            .collect();
        crate::desktop::notifications::encode_history(
            &history,
            |id| self.is_unread(id),
            wall_now,
            ui::time::now(),
        )
    }

    /// Replace the centre's history with the one [`Self::kept`] wrote elsewhere, as Settings or an
    /// agent changed it while this session runs: shown as it is now, nothing as new banners.
    pub fn replace_kept(
        &self,
        bytes: &[u8],
        wall_now: ui::time::SystemTime,
    ) -> Result<(), serde_json::Error> {
        let kept = crate::desktop::notifications::decode_history(bytes, wall_now, ui::time::now())?;
        let mut history = Vec::with_capacity(kept.len());
        {
            let mut unread = self.log.unread.borrow_mut();
            for notification in self.log.log.borrow().history() {
                unread.remove(&notification.id);
            }
            for (notification, was_unread) in kept {
                if was_unread {
                    unread.insert(notification.id);
                }
                history.push(notification);
            }
        }
        self.log.log.borrow_mut().replace_history(history);
        self.log.touch();
        Ok(())
    }

    /// Put the history [`Self::kept`] wrote back into the centre, without showing it as new
    /// banners: what the person had not seen is still unread, so the bell counts it again.
    pub fn restore_kept(
        &self,
        bytes: &[u8],
        wall_now: ui::time::SystemTime,
    ) -> Result<(), serde_json::Error> {
        let kept = crate::desktop::notifications::decode_history(bytes, wall_now, ui::time::now())?;
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

    /// The host shows the banners in a surface of their own (the surface's banner feed); the
    /// desktop surface shows only the centre.
    pub fn show_banners_elsewhere(&self) {
        self.log.banners_elsewhere.set(true);
        self.log.touch();
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

    /// Clear every notice the shell lists, the banners still up with the ones retired into the
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

    pub fn host(&self) -> &dyn NotificationHost {
        &*self.log
    }
}

/// What an application asked of the desktop's notifications, queued until a surface's next update:
/// applications post from wherever their code runs, the surface attributes and shows on its own
/// thread.
#[derive(Debug)]
pub enum AppRequest {
    Show {
        /// The window whose application code was running.
        tab: Option<u64>,
        /// Boxed, so a queued withdrawal, which is only a tag, is not as large as a
        /// notification with its sender and app.
        notification: Box<ui::notify::Notification>,
    },
    Withdraw {
        tab: Option<u64>,
        tag: String,
    },
}

/// Requests and how many surfaces passed them over: a window another surface shows is that
/// surface's to attribute.
pub type Queue = std::sync::Arc<std::sync::Mutex<Vec<(AppRequest, u8)>>>;

/// Surfaces that pass a request over this many times show it unattributed: the window that posted
/// it has closed.
pub const UNCLAIMED_PASSES: u8 = 4;

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
            tab: tilo_ui_base::widget::panic_scope::current(),
            notification: Box::new(notification.clone()),
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
            tab: tilo_ui_base::widget::panic_scope::current(),
            tag: tag.to_owned(),
        });
        Ok(())
    }
}

/// The application a window runs: its id, display name and workspace.
pub type AppOf<'a> =
    &'a dyn Fn(u64) -> Option<(String, String, Option<tilo_desktop_model::WorkspaceId>)>;

impl AppInbox {
    /// Show and withdraw what applications asked since the last call. `app_of` names the
    /// application a window of this surface runs; a request from a window it does not show is left
    /// for the surface that does. Returns whether anything changed.
    pub fn drain(&mut self, notifications: &DesktopNotifications, app_of: AppOf<'_>) -> bool {
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
                    let notification = *notification;
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
                    if shown.app_name.is_none() {
                        shown.app_name = notification.app;
                    }
                    if let Some(from) = notification.from {
                        shown = if from.agent {
                            shown.with_agent_from(from.name, from.id)
                        } else {
                            shown.with_from(from.name, from.id)
                        };
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

/// A leaf over the whole surface, out of the strip's flow.
pub fn overlay_style() -> ui::taffy::Style {
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
mod unread_tests {
    use super::*;

    /// The bell counts what the person has not seen across a restart: a kept notice they never looked
    /// at is unread in the next session, and one they read stays read.
    #[test]
    fn unread_notices_stay_unread_across_a_restart() {
        let wall = ui::time::SystemTime::now();
        let before = DesktopNotifications::default();
        before.post_quiet(Notification::new("Seen", ""));
        let _ = before.take_history_dirty();
        before.mark_read();
        assert!(
            before.take_history_dirty(),
            "reading the centre is kept with it"
        );
        before.post_quiet(Notification::new("New", ""));
        assert_eq!(before.unread(), 1);
        let kept = before.kept(wall).unwrap();
        let after = DesktopNotifications::default();
        after.restore_kept(&kept, wall).unwrap();
        assert_eq!(after.counts(), (0, 2), "both rows are back in the centre");
        assert_eq!(after.unread(), 1, "only the one never seen is unread");
        let unread: Vec<_> = after
            .log()
            .history()
            .iter()
            .filter(|notice| after.is_unread(notice.id))
            .map(|notice| notice.title.clone())
            .collect();
        assert_eq!(unread, ["New"]);
    }
}

#[cfg(test)]
mod centre_tests {
    use super::*;
    use crate::desktop::notifications::RemoteNotificationRoute;

    fn two() -> DesktopNotifications {
        let notifications = DesktopNotifications::default();
        for i in 0..2 {
            notifications.post(Notification::new(format!("n{i}"), "body"));
        }
        notifications
    }

    tilo_test_env::scenario! {
        fn the_centre_holds_news_and_arrivals;

        phase notifications_stay_unread_until_the_centre_opens;
        // The bell counts what arrived since the person last looked: opening the centre reads it all,
        // and one taken back is no longer news.
        {
            let notifications = two();
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
            notifications.set_centre_open(true);
            assert_eq!(notifications.unread(), 0, "opening the centre reads them");
            assert!(
                ids.iter().all(|id| !notifications.is_unread(*id)),
                "every row is read once the centre opened"
            );
            assert_ne!(notifications.revision(), revision, "the bell repaints");
            notifications.set_centre_open(false);
            let fresh = notifications.post(Notification::new("Grace renamed it", ""));
            assert_eq!(notifications.unread(), 1, "a new one is unread");
            assert!(notifications.withdraw(fresh));
            assert_eq!(notifications.unread(), 0, "a withdrawn one is not news");
        }

        phase another_devices_projection_updates_in_place_and_retires_together;
        // A notification another of the person's devices projected updates the row it already shows,
        // and its live and kept copies go together when that device is done with it.
        {
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
                !notifications.retire_remote("studio", id),
                "another device's id is not this one's"
            );
        }

        phase the_shells_centre_holds_arrivals_without_opening_the_shells_panel;
        // The shell draws its own centre over the shell: a banner up when it opens goes into it, what
        // arrives joins it, and once the shell says it closed, the shell's own panel is not opened in
        // its place.
        {
            let notifications = two();
            notifications.set_shell_centre(true);
            assert!(notifications.centre_open());
            assert!(
                notifications.log().active().is_empty(),
                "the banner up went into it rather than covering it"
            );
            notifications.set_shell_centre(false);
            assert!(!notifications.centre_open());
        }

        phase a_banner_never_stands_over_the_open_centre;
        // The person opened the centre to read what it holds: no banner stands over the rows it
        // covers, what arrives meanwhile goes into the centre, and the ones that came before are rows
        // of it too — a banner is gone from the shell once the centre took it.
        {
            let notifications = two();
            notifications.set_centre_open(true);
            assert_eq!(
                notifications.log().active().len(),
                0,
                "nothing stands over the centre"
            );
            // One arriving now belongs to the rows the person is reading.
            notifications.post(Notification::new("Kaffee ist fertig", ""));
            assert!(
                notifications.log().active().is_empty(),
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
            notifications.set_centre_open(false);
            assert!(
                notifications.log().active().is_empty(),
                "the banners the centre took stay rows of the centre"
            );
            assert!(
                notifications.log().history().len() >= 3,
                "all three are rows the person reads there"
            );
            // What arrives once it is closed is a banner again.
            notifications.post(Notification::new("Das Wasser kocht", ""));
            assert_eq!(
                notifications.log().active().len(),
                1,
                "a shell with no centre over it shows banners again"
            );
        }

        phase banners_retire_into_the_centre_and_it_closes_on_an_outside_press;
        {
            let notifications = two();
            let id = notifications.log().active()[0].id;
            notifications.host().dismiss(id);
            assert_eq!(notifications.counts(), (1, 1));
            notifications.set_centre_open(true);
            assert!(notifications.centre_open());
            notifications.set_centre_open(false);
            assert!(
                !notifications.centre_open(),
                "a centre closed no longer holds arrivals: the next one is a banner"
            );
            assert!(
                matches!(
                    notifications.take_outcomes().as_slice(),
                    [NotificationOutcome::Dismissed { sender: None, .. }]
                ),
                "closing a banner with no sender answers nobody, and still says it was dismissed"
            );
        }
    }
}
