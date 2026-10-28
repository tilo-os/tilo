//! Telling the person when the GPU stopped working.
//!
//! A GPU that dies mid-session must not take the desktop with it: the
//! compositor's renderer notices, gives the GPU up and draws in software
//! (`ui::gpu::recover`). The desktop then keeps working, only slower, and
//! the person should know why and what brings the speed back. The renderer
//! queues the notice on its own thread; the compositor loop drains the queue
//! into the notification centre.

use std::sync::Mutex;

use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_lock::MutexRecover as _;

use crate::shell::i18n::t;

static QUEUE: Mutex<Vec<Notification>> = Mutex::new(Vec::new());

/// The desktop gave its GPU up and draws in software from now on.
pub fn gpu_given_up() {
    QUEUE.lock_recover().push(
        Notification::new(t("graphics-stopped-title"), t("graphics-stopped-body"))
            .with_severity(NotificationSeverity::Warning)
            .with_app_name(t("graphics-app-name")),
    );
}

/// Notices queued since the last call.
pub fn drain() -> Vec<Notification> {
    std::mem::take(&mut *QUEUE.lock_recover())
}
