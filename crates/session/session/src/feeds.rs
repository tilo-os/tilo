//! What the session tells its services as it runs.
//! Both follow the desk's revision: the portal hears which apps run, which exited and when the
//! screen locked, and the Files daemon and the capture broker hear what the person took away.

use crate::desk::running_apps;

impl crate::Session {
    /// Tell the portal what it asks of the session about its apps: which run and how, which exited,
    /// and when the screen locks.
    pub(crate) fn feed_portal(&mut self) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        for (window, _) in self.compositor.windows() {
            if let (Some(pid), Some(app_id)) = (window.pid, window.app_id) {
                desktop.note_running_app(pid, &app_id);
                self.pids.insert(pid, app_id);
            }
        }
        let running = running_apps(&self.compositor);
        if running != self.running {
            desktop.set_running_apps(running.clone());
            self.running = running;
        }
        let locked = self.compositor.locked();
        if locked != self.screen_locked {
            desktop.set_screen_locked(locked);
            self.screen_locked = locked;
        }
        for pid in self.compositor.take_exited_pids() {
            if let Some(app_id) = self.pids.remove(&pid) {
                desktop.app_exited(&app_id);
            }
        }
    }

    /// Publish the desk's file access list to the Files daemon, and end what
    /// apps may no longer hold, whenever the desk changed.
    ///
    /// Both follow the same revision because both are the same decision: what
    /// an app was allowed when it was launched is not what it is allowed now.
    ///
    /// A take-away reaches the app where it acts — its next file open is
    /// refused by the mount, its next capture ask is refused by the broker,
    /// and a stream it already holds is stopped — while the app itself keeps
    /// running: nothing here closes, pauses or restarts it.
    pub(crate) fn feed_capabilities(&self) {
        let revision = self.shared.handle.revision();
        // A list the daemon did not take stands for nothing: until one does,
        // the desk hands the same revision over again.
        let unpublished = self
            .shared
            .fs_authority
            .as_ref()
            .is_some_and(crate::fs_authority::Publisher::unpublished);
        if self.shared.capabilities_fed.get() == Some(revision) && !unpublished {
            return;
        }
        self.shared.capabilities_fed.set(Some(revision));
        let now = tilo_clock::unix_now();
        let tree = self.shared.handle.read(std::clone::Clone::clone);
        if let Some(publisher) = &self.shared.fs_authority {
            publisher.publish(&crate::fs_authority::list(
                &tree,
                &self.shared.person,
                crate::fs_authority::device_key(),
                now,
            ));
        }
        let Some(captures) = self
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.portal())
            .map(|portal| portal.captures().clone())
        else {
            return;
        };
        let ended = captures.keep_only(|app, workspace, kind| {
            crate::capture_grants::still_allowed(&tree, app, workspace, kind, now)
        });
        if ended > 0 {
            tracing::debug!(ended, "captures ended: the desk no longer allows them");
        }
    }
}
