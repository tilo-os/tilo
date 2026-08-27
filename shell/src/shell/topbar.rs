//! `TopBar` — the shell's top-edge chrome row.
//!
//! `TopBar = Row([LeftNav, CenterWidgets, RightTray])`.
//!
//! LeftNav:
//! - overview toggle button,
//! - workspace up/down arrows (clamped at the ends of the workspace list),
//! - workspace-name button that requests the WorkspaceSettings modal.
//!
//! CenterWidgets cycles clock / sparkline / calendar on the wheel, with a
//! cooldown. RightTray holds:
//! - the visible collaborator avatars (first N entries of
//!   `tree.collaborators`, with an `overflow_count`-driven `+N` pill),
//! - the QuickSettings gear button that emits
//!   [`TopBarOutput::OpenQuickSettings`].
//!
//! The state fields for the center cluster — `center`, `last_wheel`,
//! `cooldown` — are declared up-front. The RightTray is stateless at the
//! logical level: it reads collaborators straight from the borrowed
//! `WorkspaceTree`.
//!
//! The shell root owns the TopBar and forwards its `TopBarOutput` values to
//! decide when to open popovers/modals (WorkspaceSettings, QuickSettings);
//! that keeps the topbar itself free of references to the other chrome
//! widgets and keeps the compositor's ownership graph acyclic.

use std::time::Duration;

use ui::widget::tooltip::Tooltip;

use crate::shell::state::{Collaborator, WorkspaceTree};
use crate::shell::system_monitor::{SparkSample, SystemMonitor};

/// Events the shell dispatches into the TopBar in response to user input
/// on the LeftNav cluster. `emit` applies them to a borrowed
/// [`WorkspaceTree`].
///
/// `ToggleOverview` does not mutate the tree itself — the outer shell owns
/// the overview-visible flag — so `emit` simply acknowledges it. Callers
/// match on this enum exhaustively, so widening it later forces every
/// existing match arm to be updated explicitly — the desired safety
/// property.
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
    /// QuickSettings popover.
    OpenQuickSettings,
}

/// Which cycling widget occupies the center of the TopBar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CenterWidget {
    Clock,
    SystemMonitor,
    Calendar,
}

/// The TopBar's logical state. See module docs for the ownership story.
pub struct TopBar {
    /// Currently visible center widget.
    center: CenterWidget,
    /// Timestamp of the last wheel event consumed by the center widget,
    /// or `None` if no wheel event has been accepted yet. This enforces
    /// the 200 ms cooldown — the first wheel always passes, then
    /// subsequent events must wait `cooldown` before cycling again.
    last_wheel: Option<Duration>,
    /// Cooldown window between successive center-widget cycles.
    cooldown: Duration,
    /// Latest sparkline samples for the `SystemMonitor` center variant.
    /// Refreshed once per frame by the shell tick hook via
    /// [`TopBar::set_system_monitor`]; rendering code reads this cache
    /// instead of borrowing the monitor across frames.
    spark_samples: Vec<SparkSample>,
    /// Hover tooltip for the LeftNav workspace-name button. The shell
    /// calls [`TopBar::hover_ws_name`] on pointer enter/leave and
    /// [`TopBar::tick`] once per frame; the primitive flips visible once
    /// the accumulated hover crosses its 600 ms reveal delay.
    ws_name_tooltip: Tooltip,
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
            ws_name_tooltip: Tooltip::new("Open workspace settings"),
        }
    }

    /// The currently visible center widget.
    pub fn center_widget(&self) -> CenterWidget {
        self.center
    }

    /// The configured wheel-cycle cooldown. Exposed so tests can assert
    /// the 200 ms default without reaching into private fields.
    pub fn cooldown(&self) -> Duration {
        self.cooldown
    }

    /// Timestamp of the last accepted wheel event, or `Duration::ZERO` if
    /// no wheel event has ever been accepted. Exposed for tests and the
    /// shell tick hook.
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
                tree.switch_up();
            }
            TopBarEvent::WorkspaceDown => {
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
    /// cooldown is active are silently dropped (no visual feedback).
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
        const VARIANTS: [CenterWidget; 3] = [
            CenterWidget::Clock,
            CenterWidget::SystemMonitor,
            CenterWidget::Calendar,
        ];
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

    /// Pointer entered (`hovered == true`) or left (`hovered == false`) the
    /// LeftNav workspace-name button. Drives the hover timing on the
    /// attached tooltip; see [`TopBar::ws_name_tooltip_visible`].
    ///
    /// The primitive seeds its accumulator with `Duration::ZERO` on enter,
    /// so the shell must call [`TopBar::tick`] to advance time. On leave
    /// the accumulator is cleared and the tooltip hides immediately.
    pub fn hover_ws_name(&mut self, hovered: bool) {
        if hovered {
            self.ws_name_tooltip.on_hover_enter(Duration::ZERO);
        } else {
            self.ws_name_tooltip.on_hover_leave();
        }
    }

    /// Advance hover-timing clocks owned by the topbar by `dt`. Currently
    /// only the workspace-name tooltip consumes this; future center-widget
    /// or right-tray tooltips can be multiplexed here without touching the
    /// shell's per-frame call site.
    pub fn tick(&mut self, dt: Duration) {
        self.ws_name_tooltip.tick(dt);
    }

    /// `true` once the hover over the workspace-name button has persisted
    /// long enough to reveal the tooltip. Returns `false` while the pointer
    /// is not over the button or before the reveal delay has elapsed.
    pub fn ws_name_tooltip_visible(&self) -> bool {
        self.ws_name_tooltip.is_visible()
    }
}

impl Default for TopBar {
    fn default() -> Self {
        Self::new()
    }
}
