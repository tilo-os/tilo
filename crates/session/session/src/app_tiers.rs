//! Apps nobody is looking at: the tier they run in, and the memory they give back first.
//! The desk knows which windows it shows — the front tab of each group on the strip in front, and
//! the dialogs floating over them — and the compositor knows which process made each window, so the
//! session knows every app the person can see. It hands those processes to the apps it runs
//! ([`tilo_app_launch::Spawner::tier_by_visibility`]) once per pass: an app that is still opening
//! is in the startup tier of its cgroup, an app with a window the desk shows is in the foreground
//! tier, an app with none is in the background tier, written through the same tier files a launch
//! is born with. The cgroup this session runs in carries the session's own tier, which the pass
//! names so a launch that shares it is never demoted with it.
//! That tier is what the memory watch goes by when the machine runs low: the apps the person is
//! looking at keep what they hold, and the ones they cannot see give a quarter of it back first, so
//! the app in front stays responsive without anything being closed.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The cgroup this session runs in: where its desk is weighted and where a launch a host gave no
/// scope of its own runs. Read once: the pass asks every frame and it cannot change under a running
/// session (the cgroup the session was started in is the one its manager made for it).
fn own_cgroup() -> Option<&'static Path> {
    static OWN: OnceLock<Option<PathBuf>> = OnceLock::new();
    OWN.get_or_init(launcher::app_cgroup::own_cgroup).as_deref()
}

impl crate::Session {
    /// Put every app this session runs in the tier of what the person can see, once per pass.
    pub(crate) fn follow_app_tiers(&self) {
        let Some(apps) = &self.shared.launch else {
            return;
        };
        let Some(in_view) = apps.windows_in_view() else {
            return;
        };
        let on_screen: Vec<u32> = self
            .compositor
            .windows()
            .into_iter()
            .filter(|(window, _)| in_view.contains(&window.id))
            .filter_map(|(window, _)| window.pid)
            .collect();
        apps.tier_by_visibility(&on_screen, own_cgroup());
    }
}
