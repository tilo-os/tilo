//! The surface's component lifecycle and control custody: every tab's application, shown or not,
//! closes and takes its controls with the surface, whichever strip or row it stands in.

use super::{DesktopSurface, TabId};
use ui::widget::{TileHost, Widget};

impl ui::component_lifecycle::ComponentLifecycle for DesktopSurface {
    /// Every tab's application, shown or not, closes with the surface.
    fn close_components(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        self.each_host(|host| ui::component_lifecycle::close_components(host))
    }

    fn take_component_close_reports(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        let mut reports =
            self.each_host(|host| ui::component_lifecycle::take_component_close_reports(host));
        // Windows a row removed itself: pruned after their close, or dropped by a tree change.
        for row in self.rows.values_mut() {
            reports.extend(ui::component_lifecycle::take_component_close_reports(row));
        }
        reports
    }
}

impl ui::widget::controls::ActionControls for DesktopSurface {
    /// Bind every tab, shown or not, and keep `domain` for the tabs built later; then the overlays
    /// the surface currently shows.
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        self.controls = Some(domain.clone());
        for row in self.rows.values_mut() {
            for group in &mut row.groups {
                let tabs: Vec<TabId> = group.tabs().iter().map(|tab| tab.id).collect();
                for tab in tabs {
                    if let Some(host) = group.host_for_tab_mut(tab) {
                        ui::widget::controls::install_control_custody(host, domain, attachment);
                    }
                }
            }
        }
        for child in self.children_mut() {
            ui::widget::controls::install_control_custody(child, domain, attachment);
        }
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.unrouted_control.is_some()
            || self
                .children()
                .into_iter()
                .any(ui::widget::controls::has_pending_control_invocation)
    }

    /// An action row chosen in search outlives search closing on it.
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.unrouted_control
            .take()
            .or_else(|| ui::widget::controls::take_control_invocation(self.shown_row()?))
    }
}

impl DesktopSurface {
    /// Traversal reaches only active tabs; lifecycle must reach them all.
    fn each_host(
        &mut self,
        mut visit: impl FnMut(&mut TileHost) -> Vec<ui::application::ComponentCloseReport>,
    ) -> Vec<ui::application::ComponentCloseReport> {
        let mut reports = std::mem::take(&mut self.close_reports);
        for row in self.rows.values_mut() {
            for group in &mut row.groups {
                let tabs: Vec<TabId> = group.tabs().iter().map(|tab| tab.id).collect();
                for tab in tabs {
                    if let Some(host) = group.host_for_tab_mut(tab) {
                        reports.extend(visit(host));
                    }
                }
            }
        }
        reports
    }
}
