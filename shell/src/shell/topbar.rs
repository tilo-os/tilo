//! `TopBar` — the shell's top-edge chrome row: `Row([LeftNav, CenterWidgets,
//! RightTray])`. `CenterWidgets` cycles clock/sparkline/calendar on the
//! wheel with a cooldown; `RightTray` reads collaborators straight off
//! the borrowed `WorkspaceTree`.
//!
//! The shell root owns the `TopBar` and forwards its `TopBarOutput` to
//! decide when to open popovers/modals, keeping the topbar free of
//! references to other chrome widgets and the ownership graph acyclic.

use std::time::Duration;

use ui::widget::tooltip::Tooltip;

use crate::shell::state::{Collaborator, WorkspaceTree};
use crate::shell::system_monitor::{SparkSample, SystemMonitor};

/// `emit` applies these to a borrowed [`WorkspaceTree`]; `ToggleOverview`
/// is a no-op there since the outer shell owns that flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopBarEvent {
    ToggleOverview,
    /// Clamped at index 0.
    WorkspaceUp,
    /// Clamped at len-1.
    WorkspaceDown,
}

/// One-shot outputs back to the shell root, which decides how to open
/// the corresponding popover/modal; the `TopBar` itself holds no handles
/// to them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopBarOutput {
    OpenWorkspaceSettings,
    OpenQuickSettings,
}

/// Which cycling widget occupies the center of the `TopBar`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CenterWidget {
    Clock,
    SystemMonitor,
    Calendar,
}

pub struct TopBar {
    center: CenterWidget,
    /// `None` before the first wheel event — the first always passes.
    last_wheel: Option<Duration>,
    cooldown: Duration,
    /// Refreshed once per frame by the shell tick hook so rendering
    /// doesn't borrow the monitor across frames.
    spark_samples: Vec<SparkSample>,
    /// Driven by [`TopBar::hover_ws_name`]/[`TopBar::tick`]; flips
    /// visible after a 600ms reveal delay.
    ws_name_tooltip: Tooltip,
}

impl TopBar {
    pub fn new() -> Self {
        Self {
            center: CenterWidget::Clock,
            last_wheel: None,
            cooldown: Duration::from_millis(200),
            spark_samples: Vec::new(),
            ws_name_tooltip: Tooltip::new("Open workspace settings"),
        }
    }

    pub fn center_widget(&self) -> CenterWidget {
        self.center
    }

    /// Exposed for tests.
    pub fn cooldown(&self) -> Duration {
        self.cooldown
    }

    pub fn last_wheel(&self) -> Duration {
        self.last_wheel.unwrap_or(Duration::ZERO)
    }

    /// `WorkspaceUp`/`WorkspaceDown` clamp at the tree's ends.
    pub fn emit(&mut self, event: TopBarEvent, tree: &mut WorkspaceTree) {
        match event {
            TopBarEvent::WorkspaceUp => {
                tree.switch_up();
            }
            TopBarEvent::WorkspaceDown => {
                tree.switch_down();
            }
            TopBarEvent::ToggleOverview => {
                // Owned by the outer shell.
            }
        }
    }

    pub fn click_workspace_name(&mut self) -> TopBarOutput {
        TopBarOutput::OpenWorkspaceSettings
    }

    /// The rest are accounted for via [`TopBar::overflow_count`].
    pub fn visible_avatars<'a>(
        &self,
        tree: &'a WorkspaceTree,
        max: usize,
    ) -> Vec<&'a Collaborator> {
        tree.collaborators.iter().take(max).collect()
    }

    pub fn overflow_count(&self, tree: &WorkspaceTree, max: usize) -> usize {
        tree.collaborators.len().saturating_sub(max)
    }

    pub fn click_tray_settings(&mut self) -> TopBarOutput {
        TopBarOutput::OpenQuickSettings
    }

    /// Only `dir`'s sign matters. Wraps modulo 3 across
    /// `Clock → SystemMonitor → Calendar → Clock`.
    pub fn on_center_wheel(&mut self, dir: i32, now: Duration) {
        let step = dir.signum();
        if step == 0 {
            return;
        }
        // First wheel always accepted; the cooldown stops flicky
        // trackpads skimming past every widget.
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
        // Euclidean rem so a negative `cur + step` wraps positive.
        let next = ((cur + step) % len + len) % len;
        self.center = VARIANTS[next as usize];
        self.last_wheel = Some(now);
    }

    /// Called once per frame so the topbar never borrows the monitor
    /// across frames.
    pub fn set_system_monitor(&mut self, monitor: &SystemMonitor) {
        self.spark_samples = monitor.samples().iter().map(|s| **s).collect();
    }

    pub fn spark_samples(&self) -> &[SparkSample] {
        &self.spark_samples
    }

    /// Seeds the tooltip accumulator on enter (the shell must call
    /// [`TopBar::tick`] to advance it); clears on leave.
    pub fn hover_ws_name(&mut self, hovered: bool) {
        if hovered {
            self.ws_name_tooltip.on_hover_enter(Duration::ZERO);
        } else {
            self.ws_name_tooltip.on_hover_leave();
        }
    }

    /// Only the workspace-name tooltip consumes this now; future
    /// tooltips can multiplex here.
    pub fn tick(&mut self, dt: Duration) {
        self.ws_name_tooltip.tick(dt);
    }

    pub fn ws_name_tooltip_visible(&self) -> bool {
        self.ws_name_tooltip.is_visible()
    }
}

impl Default for TopBar {
    fn default() -> Self {
        Self::new()
    }
}
