//! `TopBar` — the shell's top-edge chrome row.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` §5 —
//! `TopBar = Row([LeftNav, CenterWidgets, RightTray])`.
//!
//! T18 owns the **LeftNav** surface:
//! - overview toggle button,
//! - workspace up/down arrows (clamped at the ends of the workspace list),
//! - workspace-name button that requests the WorkspaceSettings modal.
//!
//! T19 adds the CenterWidgets cluster (clock / sparkline / calendar) with
//! wheel cycling + cooldown. T20 adds the RightTray surface:
//! - the visible collaborator avatars (first N entries of
//!   `tree.collaborators`, with an `overflow_count`-driven `+N` pill),
//! - the QuickSettings gear button that emits
//!   [`TopBarOutput::OpenQuickSettings`].
//!
//! The state fields for the center cluster — `center`, `last_wheel`,
//! `cooldown` — are declared up-front so T19/T20 don't need to reshape the
//! struct mid-plan. The RightTray is stateless at the logical level: it
//! reads collaborators straight from the borrowed `WorkspaceTree`.
//!
//! The shell root owns the TopBar and forwards its `TopBarOutput` values to
//! decide when to open popovers/modals (WorkspaceSettings, QuickSettings);
//! that keeps the topbar itself free of references to the other chrome
//! widgets and keeps the compositor's ownership graph acyclic.

use std::time::Duration;

use crate::shell::state::{Collaborator, WorkspaceTree};
use crate::shell::system_monitor::{SparkSample, SystemMonitor};

/// Events the shell dispatches into the TopBar in response to user input
/// on the LeftNav cluster. `emit` applies them to a borrowed
/// [`WorkspaceTree`].
///
/// `ToggleOverview` does not mutate the tree itself — the outer shell owns
/// the overview-visible flag — so `emit` simply acknowledges it. Future
/// tasks can widen this enum (e.g. center-widget cycling keys); existing
/// callers that match on it will need to add arms explicitly, which is the
/// desired safety property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopBarEvent {
    /// User clicked the overview toggle. Handled by the outer shell.
    ToggleOverview,
    /// Activate the workspace preceding the current one. Clamped at index 0.
    WorkspaceUp,
    /// Activate the workspace following the current one. Clamped at len-1.
    WorkspaceDown,
}

/// One-shot outputs the TopBar surfaces back to the shell root. The shell
/// decides how to open the corresponding popover/modal; the TopBar itself
/// does not hold handles to them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopBarOutput {
    /// Click on the workspace-name button — the shell should open the
    /// WorkspaceSettings modal for the active workspace.
    OpenWorkspaceSettings,
    /// Click on the RightTray gear — the shell should open the
    /// QuickSettings popover. Declared here so T20's RightTray test can
    /// match on it without another enum revision.
    OpenQuickSettings,
}

/// Which cycling widget occupies the center of the TopBar. T18 only ever
/// renders `Clock`; T19 wires wheel-cycling between the three variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CenterWidget {
    Clock,
    SystemMonitor,
    Calendar,
}

/// The TopBar's logical state. See module docs for the ownership story.
pub struct TopBar {
    /// Currently visible center widget. T18 leaves this at `Clock`.
    center: CenterWidget,
    /// Timestamp of the last wheel event consumed by the center widget,
    /// or `None` if no wheel event has been accepted yet. T19 reads this
    /// to enforce the 200 ms cooldown — the first wheel always passes,
    /// then subsequent events must wait `cooldown` before cycling again.
    last_wheel: Option<Duration>,
    /// Cooldown window between successive center-widget cycles.
    cooldown: Duration,
    /// Latest sparkline samples for the `SystemMonitor` center variant.
    /// Refreshed once per frame by the shell tick hook via
    /// [`TopBar::set_system_monitor`]; rendering code reads this cache
    /// instead of borrowing the monitor across frames.
    spark_samples: Vec<SparkSample>,
}

impl TopBar {
    /// Construct a fresh TopBar with the Clock widget selected and the
    /// wheel-cooldown clock reset.
    pub fn new() -> Self {
        Self {
            center: CenterWidget::Clock,
            last_wheel: None,
            cooldown: Duration::from_millis(200),
            spark_samples: Vec::new(),
        }
    }

    /// The currently visible center widget. Exposed for T19.
    pub fn center_widget(&self) -> CenterWidget {
        self.center
    }

    /// The configured wheel-cycle cooldown. Exposed so T19's tests can
    /// assert the 200 ms default without reaching into private fields.
    pub fn cooldown(&self) -> Duration {
        self.cooldown
    }

    /// Timestamp of the last accepted wheel event, or `Duration::ZERO` if
    /// no wheel event has ever been accepted. Exposed for T19 tests and
    /// the shell tick hook.
    pub fn last_wheel(&self) -> Duration {
        self.last_wheel.unwrap_or(Duration::ZERO)
    }

    /// Apply a [`TopBarEvent`] to the borrowed [`WorkspaceTree`].
    ///
    /// `WorkspaceUp` / `WorkspaceDown` clamp at the ends of
    /// `tree.workspaces`; `ToggleOverview` is a no-op here because the
    /// overview-visible flag lives on the outer shell.
    pub fn emit(&mut self, event: TopBarEvent, tree: &mut WorkspaceTree) {
        match event {
            TopBarEvent::WorkspaceUp => {
                // `switch_up` is a no-op at index 0, matching the clamp
                // contract the T18 test expects.
                tree.switch_up();
            }
            TopBarEvent::WorkspaceDown => {
                // Same clamp contract — `switch_down` refuses to advance
                // past the last workspace.
                tree.switch_down();
            }
            TopBarEvent::ToggleOverview => {
                // Owned by the outer shell; no tree mutation to perform.
            }
        }
    }

    /// The workspace-name button was clicked. Signal to the shell that it
    /// should open the WorkspaceSettings modal for the active workspace.
    pub fn click_workspace_name(&mut self) -> TopBarOutput {
        TopBarOutput::OpenWorkspaceSettings
    }

    /// Visible avatars for the RightTray: the first `max` collaborators in
    /// `tree.collaborators`. If `max == 0` the returned slice is empty.
    /// Remaining collaborators are accounted for via [`TopBar::overflow_count`].
    pub fn visible_avatars<'a>(
        &self,
        tree: &'a WorkspaceTree,
        max: usize,
    ) -> Vec<&'a Collaborator> {
        tree.collaborators.iter().take(max).collect()
    }

    /// Count of collaborators not shown as individual avatars — rendered as
    /// the `+N` overflow pill in the RightTray. Returns 0 when all fit.
    pub fn overflow_count(&self, tree: &WorkspaceTree, max: usize) -> usize {
        tree.collaborators.len().saturating_sub(max)
    }

    /// The RightTray gear (QuickSettings trigger) was clicked. Signal to
    /// the shell that it should open the QuickSettings popover.
    pub fn click_tray_settings(&mut self) -> TopBarOutput {
        TopBarOutput::OpenQuickSettings
    }

    /// Handle a wheel event over the center-widget cluster. `dir` is a raw
    /// delta — only its sign matters. `now` is the monotonic clock used to
    /// rate-limit cycles; see [`TopBar::cooldown`]. Calls while the
    /// cooldown is active are silently dropped (no visual feedback — the
    /// spec §5 calls this "gentle rate-limit").
    ///
    /// Indexing wraps modulo 3 across `Clock → SystemMonitor → Calendar →
    /// Clock`, so wheel-down advances forward and wheel-up steps back.
    /// `dir == 0` is a no-op.
    pub fn on_center_wheel(&mut self, dir: i32, now: Duration) {
        let step = dir.signum();
        if step == 0 {
            return;
        }
        // First wheel always accepted; subsequent wheels must respect the
        // 200 ms cooldown so flicky trackpads don't skim past every widget.
        let allowed = match self.last_wheel {
            None => true,
            Some(prev) => now.saturating_sub(prev) >= self.cooldown,
        };
        if !allowed {
            return;
        }
        const VARIANTS: [CenterWidget; 3] =
            [CenterWidget::Clock, CenterWidget::SystemMonitor, CenterWidget::Calendar];
        let cur = VARIANTS.iter().position(|v| *v == self.center).unwrap_or(0) as i32;
        let len = VARIANTS.len() as i32;
        // Euclidean rem so negative `cur + step` wraps to the positive side.
        let next = ((cur + step) % len + len) % len;
        self.center = VARIANTS[next as usize];
        self.last_wheel = Some(now);
    }

    /// Refresh the sparkline cache from the shell's owned
    /// [`SystemMonitor`]. The shell calls this once per frame from its
    /// tick hook so the `SystemMonitor` center variant always renders
    /// from the most recent samples without the topbar borrowing the
    /// monitor across frames.
    pub fn set_system_monitor(&mut self, monitor: &SystemMonitor) {
        self.spark_samples = monitor.samples().iter().map(|s| **s).collect();
    }

    /// Sparkline samples cached by [`TopBar::set_system_monitor`].
    /// Oldest-first. Empty until the first tick-hook call.
    pub fn spark_samples(&self) -> &[SparkSample] {
        &self.spark_samples
    }
}

impl Default for TopBar {
    fn default() -> Self {
        Self::new()
    }
}
