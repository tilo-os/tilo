//! What stands in a harness's place as its root: the component a window shows, mounted over the
//! root it replaced.
//!
//! A component asks to open another record (`component_lifecycle`), and a host builds the
//! application it stood a window in for: both replace the root the harness shows with a component,
//! keeping the viewport, theme and fonts the frame was set up with, and closing whatever the root
//! it replaces owned.

use crate::Widget;
use crate::headless::{ComponentRunnerWakeState, FrameHarness};

/// A component mounted ready to stand in place of the root a harness shows
/// ([`FrameHarness::take_component_root`]): nothing of the root it replaces is
/// closed until it is taken, so a host that fails to mount it keeps what it
/// showed.
struct MountedComponent {
    root: Box<dyn Widget>,
    wake: ComponentRunnerWakeState,
    history: Option<crate::history_mode::HistoryRegistration>,
    control_domain: crate::ControlDomain,
    owner_queue: crate::owner_wake::OwnerWakeQueue,
}

impl FrameHarness {
    /// Show the record the component asked to open: close it keeping its recovery, and mount the
    /// component it built in its place with everything the host set kept.
    pub(super) fn swap_requested_component(&mut self) {
        let Some(replacement) = crate::component_lifecycle::take_replacement(self.root.as_mut())
        else {
            return;
        };
        let notify = self.component_wake.as_ref().map_or_else(
            || std::sync::Arc::new(|| {}) as std::sync::Arc<dyn Fn() + Send + Sync>,
            |wake| std::sync::Arc::clone(&wake.notify),
        );
        let (component, outcome) = replacement.into_parts();
        let mounted = match self.mount_root_component(component, notify) {
            Ok(mounted) => mounted,
            Err(error) => {
                outcome.answer(Err(error.to_string()));
                return;
            }
        };
        // The asking component answers the operation that asked before it
        // closes.
        outcome.answer(Ok(()));
        self.poll_control_invocations();
        self.take_component_root(mounted);
    }

    /// Show `component` as the root in place of what this harness shows, its wakes reaching
    /// `notify`: a host that stood a window in for its application until the record it opens came
    /// builds the application here, keeping the viewport, theme and fonts. What the root it
    /// replaces owned closes.
    ///
    /// # Errors
    /// Refuses a component that cannot mount, keeping the root it showed.
    pub fn show_component(
        &mut self,
        component: crate::application::AppComponent,
        notify: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), crate::launch::LaunchError> {
        let mounted = self.mount_root_component(component, notify)?;
        self.take_component_root(mounted);
        Ok(())
    }

    /// Mount `component` as a root, ready to stand in place of the one this harness shows
    /// ([`Self::take_component_root`]).
    fn mount_root_component(
        &self,
        component: crate::application::AppComponent,
        notify: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Result<MountedComponent, crate::launch::LaunchError> {
        let owner_queue = self.owner_wake_queue.clone().unwrap_or_default();
        let (root, wake, history) = Self::mount_component(component, notify, &owner_queue)?;
        let mut root = root;
        let control_domain = crate::control_custody::ControlDomain::outer();
        crate::widget::controls::install_control_custody(
            root.as_mut(),
            &control_domain,
            crate::control_custody::CustodyAttachment::Initial,
        );
        control_domain.finish_initial_installation();
        Ok(MountedComponent {
            root,
            wake,
            history,
            control_domain,
            owner_queue,
        })
    }

    /// Show `mounted` as the root, closing what the root it replaces owned.
    fn take_component_root(&mut self, mounted: MountedComponent) {
        let MountedComponent {
            root,
            wake,
            history,
            control_domain,
            owner_queue,
        } = mounted;
        let reports = {
            self.close_components();
            self.take_component_close_reports()
        };
        self.component_close_reports.extend(reports);
        self.control_domain = control_domain;
        self.component_wake = Some(Box::new(wake));
        self.history = None;
        self.pending_history = None;
        self.discard_layout_tree();
        self.focused = None;
        self.focus_before_trap = None;
        self.focus_trap_node = None;
        self.ordinary_palette_return_focus = None;
        self.root_type = None;
        self.root = root;
        self.owner_wake_queue = Some(owner_queue);
        if let Some(registration) = history
            && let Err(error) = self.install_history(registration)
        {
            tracing::warn!(%error, "the replaced record shows without its history");
        }
    }
}
