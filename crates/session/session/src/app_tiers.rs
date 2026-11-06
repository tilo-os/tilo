//! Apps nobody is looking at: the tier they run in, and the memory they give
//! back first.
//!
//! The desk knows which windows it shows — the front tab of each group on the
//! strip in front, and the dialogs floating over them — and the compositor
//! knows which process made each window, so the session knows every app the
//! person can see. It hands those processes to the apps it runs
//! ([`tilo_app_launch::Spawner::tier_by_visibility`]) once per pass: an app
//! with a window the desk shows is in the foreground tier of its cgroup, an
//! app with none is in the background tier, written through the same tier
//! files a launch is born with.
//!
//! That tier is what the memory watch goes by when the machine runs low: the
//! apps the person is looking at keep what they hold, and the ones they cannot
//! see give a quarter of it back first, so the app in front stays responsive
//! without anything being closed.

impl crate::Session {
    /// Put every app this session runs in the tier of what the person can
    /// see, once per pass.
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
        apps.tier_by_visibility(&on_screen);
    }
}
