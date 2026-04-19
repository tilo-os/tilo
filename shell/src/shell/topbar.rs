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
//! Later tasks in this plan flesh out the CenterWidgets (T19 clock/sparkline/
//! calendar with wheel cycling) and RightTray (T20 collaborator avatars +
//! QuickSettings trigger). The state fields for those tasks — `center`,
//! `last_wheel`, `cooldown` — are declared up-front so T19/T20 don't need to
//! reshape the struct mid-plan.
//!
//! The shell root owns the TopBar and forwards its `TopBarOutput` values to
//! decide when to open popovers/modals (WorkspaceSettings, QuickSettings);
//! that keeps the topbar itself free of references to the other chrome
//! widgets and keeps the compositor's ownership graph acyclic.

use std::time::Duration;

use crate::shell::state::WorkspaceTree;

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
    /// Timestamp of the last wheel event consumed by the center widget.
    /// T19 reads this to enforce the 200 ms cooldown.
    last_wheel: Duration,
    /// Cooldown window between successive center-widget cycles.
    cooldown: Duration,
}

impl TopBar {
    /// Construct a fresh TopBar with the Clock widget selected and the
    /// wheel-cooldown clock reset.
    pub fn new() -> Self {
        Self {
            center: CenterWidget::Clock,
            last_wheel: Duration::ZERO,
            cooldown: Duration::from_millis(200),
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

    /// Timestamp of the last accepted wheel event. Exposed for T19.
    pub fn last_wheel(&self) -> Duration {
        self.last_wheel
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
}

impl Default for TopBar {
    fn default() -> Self {
        Self::new()
    }
}
