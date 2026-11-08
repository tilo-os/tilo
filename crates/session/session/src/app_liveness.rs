//! Apps that stop answering: the desk says which windows they are, and the
//! person's Wait or Force quit reaches the compositor.
//!
//! The compositor pings every app it shows and expects an answer
//! (`tilo_compositor`'s app liveness): one that misses its answer past the
//! timeout has stopped listening, and the window it left behind is what the
//! person presses at in vain. The compositor knows which windows those are;
//! the desk is where a person, a script and an agent read them, so this pass
//! hands the set to the desk ([`DesktopHandle::set_unresponsive_windows`]) and
//! carries out what the person chose there
//! ([`DesktopHandle::take_liveness_choices`]). Both choices go to the
//! compositor: Wait gives the app another period to answer, Force quit ends
//! it — its connection first, so its windows go, then the processes behind
//! it, which a hung app cannot end itself.

use tilo_desktop_model::TabId;
use tilo_desktop_surface::LivenessChoice;

impl crate::Session {
    /// Tell the desk which apps stopped answering, and carry out what the person chose on their
    /// windows.
    pub(crate) fn follow_app_liveness(&mut self) {
        self.publish_app_liveness();
        for (tab, choice) in self.handle().take_liveness_choices() {
            self.answer_app_liveness(tab, choice);
        }
    }

    /// The windows that say "Not responding", as the compositor judges them now: the same set the
    /// overview, the bar and an agent read.
    fn publish_app_liveness(&self) {
        self.handle()
            .set_unresponsive_windows(self.compositor.not_responding_windows().to_vec());
    }

    /// The person chose `choice` on the window of `tab` .
    fn answer_app_liveness(&mut self, tab: TabId, choice: LivenessChoice) {
        let Some(window) = self.handle().window_of_tab(tab) else {
            // The window went between the press and this pass: nothing is left to wait for or to
            // quit.
            return;
        };
        match choice {
            LivenessChoice::Wait => {
                self.compositor.wait_window(window);
            }
            LivenessChoice::ForceQuit => {
                if let Some(pid) = self.compositor.force_quit_window(window) {
                    end_app(pid);
                }
            }
        }
        self.publish_app_liveness();
    }
}

/// End the app whose process is `pid` : its whole app cgroup — the launcher and everything it
/// started — when the launcher put it in a scope of its own, and the process itself otherwise. A
/// hung app acts on no polite signal, so this one cannot be caught.
fn end_app(pid: u32) {
    // An app the session runs is never the session itself: a client in this process is a fixture,
    // and the compositor's disconnect has ended it.
    if pid == std::process::id() {
        return;
    }
    #[cfg(target_os = "linux")]
    if let Some(cgroup) = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .and_then(|cgroup| launcher::app_cgroup::app_cgroup_at(&cgroup))
        && launcher::app_cgroup::kill(&launcher::app_cgroup::fs_path(&cgroup)).is_ok()
    {
        return;
    }
    if let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
}
