//! The notification a note raises when its save gave up while nobody could
//! see it, and that notification's "Try again" coming back.
//!
//! The save status surface decides when (see
//! [`ui::SaveStatusSurface::take_notice`]); this posts it through the host's
//! notifications under a tag of this window's own, and routes the click back
//! to the window that raised it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Once};
use ui::lock::MutexRecover as _;

/// The activation URI scheme Notes owns.
const SCHEME: &str = "tilo-notes";
/// The activation of "Try again", followed by the window's id.
const RETRY: &str = "tilo-notes:save-retry/";

/// Windows waiting for a "Try again", and those it reached.
#[derive(Default)]
struct Retries {
    windows: HashMap<u64, ui::wake::WakeHandle>,
    asked: HashSet<u64>,
}

static RETRIES: LazyLock<Mutex<Retries>> = LazyLock::new(Mutex::default);

fn retries() -> MutexGuard<'static, Retries> {
    RETRIES.lock_recover()
}

/// A click on "Try again", from whatever thread the platform reports it.
fn activated(uri: &str) {
    let Some(id) = uri
        .strip_prefix(RETRY)
        .and_then(|id| id.parse::<u64>().ok())
    else {
        return;
    };
    let mut retries = retries();
    if let Some(wake) = retries.windows.get(&id).cloned() {
        retries.asked.insert(id);
        drop(retries);
        wake.wake();
    }
}

/// One window's save notification.
pub(crate) struct SaveNotices {
    id: u64,
    notifier: Option<Arc<dyn ui::notify::Notifier>>,
}

impl SaveNotices {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        static HANDLER: Once = Once::new();
        HANDLER.call_once(|| ui::notify::set_activation_handler(SCHEME, activated));
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        retries().windows.insert(id, ui::wake::WakeHandle::new());
        Self { id, notifier: None }
    }

    /// The tag this window's notification shows under: one per window, so
    /// two notes that both failed to save each say so.
    fn tag(&self) -> String {
        format!("save-{}", self.id)
    }

    /// The host's notifications, resolved once.
    fn notifier(&mut self) -> Arc<dyn ui::notify::Notifier> {
        self.notifier
            .get_or_insert_with(|| ui::host::current().notifications)
            .clone()
    }

    /// Raise or take down what `surface` asks at `now` about the note
    /// titled `title`, and take a "Try again" chosen on the notification.
    /// Returns whether the person asked to try again, so the note's changes
    /// are submitted once more.
    pub(crate) fn deliver(
        &mut self,
        surface: &mut ui::SaveStatusSurface,
        title: Option<&str>,
        now: ui::time::Instant,
    ) -> bool {
        let asked = retries().asked.remove(&self.id);
        if asked {
            surface.retry_from_notice();
        }
        if surface.take_withdrawal()
            && let Err(error) = self.notifier().withdraw(&self.tag())
        {
            tracing::debug!(%error, "withdrawing the note's save notification");
        }
        if let Some(notice) = surface.take_notice(now, title, &ui::i18n!("notes-title")) {
            let notification = ui::notify::Notification::new(self.tag(), notice.title, notice.body)
                .with_activation(format!("{RETRY}{}", self.id))
                .with_action(notice.action)
                .with_severity(ui::notify::Severity::Warning);
            if let Err(error) = self.notifier().show(&notification) {
                tracing::warn!(%error, "telling the person a note was not saved");
            }
        }
        asked
    }

    /// Use `notifier` instead of the host's.
    #[cfg(test)]
    pub(crate) fn set_notifier(&mut self, notifier: Arc<dyn ui::notify::Notifier>) {
        self.notifier = Some(notifier);
    }

    /// Click this window's notification, as the platform reports it.
    #[cfg(test)]
    pub(crate) fn click(&self) {
        activated(&format!("{RETRY}{}", self.id));
    }
}

impl Drop for SaveNotices {
    fn drop(&mut self) {
        let mut retries = retries();
        retries.windows.remove(&self.id);
        retries.asked.remove(&self.id);
    }
}
