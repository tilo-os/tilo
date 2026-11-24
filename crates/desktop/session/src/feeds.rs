//! What the session tells its services as it runs.
//! Both follow the session's revision: the portal hears which apps run, which exited and when the
//! screen locked, and the capture broker hears what the person took away.

use crate::background::running_apps;

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

    /// End what apps may no longer hold, whenever the session or the person's own store changed:
    /// what an app was allowed when it was launched is not what it is allowed now. The person's
    /// store holds their device answers, which they take back on any of their devices. (The Files
    /// mounts hear the file access list from the device's daemon.)
    ///
    /// A take-away reaches the app where it acts — its next capture ask is refused, and a stream,
    /// input capture or remote control it already holds is ended — while the app itself keeps
    /// running: nothing here closes, pauses or restarts it.
    pub(crate) fn feed_capabilities(&self) {
        let portal = self.desktop.as_ref().and_then(|desktop| desktop.portal());
        let personal = portal
            .and_then(|portal| portal.consents().grants().changes())
            .map_or(0, |changes| *changes.borrow());
        let revision = (
            self.shared.handle.revision(),
            personal,
            tilo_identity::policy_change::epoch(),
        );
        if self.shared.capabilities_fed.get() == Some(revision) {
            return;
        }
        self.shared.capabilities_fed.set(Some(revision));
        let Some(portal) = portal else {
            return;
        };
        let tree = self.shared.handle.read(std::clone::Clone::clone);
        let grants = portal.consents().grants();
        grants.refuse_in(crate::capture_grants::refusals(
            &tree,
            tilo_clock::unix_now_ms(),
        ));
        let taken = grants.taken_back();
        let ended = portal.end_captures(|app, workspace, device| {
            crate::capture_grants::ended(grants, &taken, app, workspace, device)
        });
        if ended > 0 {
            tracing::debug!(ended, "captures ended: the session no longer allows them");
        }
        portal.announce_device_answers();
    }
}
