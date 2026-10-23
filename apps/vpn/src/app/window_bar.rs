//! The VPN window's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): Back where the window shows one column at a
//! time, the selected connection as the location, its views (Status,
//! Configuration, Activity), Import configuration and the connection's main
//! action (Connect, Disconnect or Set up on this device). The window draws
//! no header row and no tabs of its own.

use ui::widget::{NavPane, NavShell};
use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use super::VpnApp;
use crate::view::{DetailTab, Intent, MainAction};

pub(crate) const IMPORT: &str = "vpn-import";
pub(crate) const MAIN_ACTION: &str = "vpn-main-action";

impl VpnApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let one_at_a_time = !self.shell.geometry().stack.is_empty();
        let list_in_front = one_at_a_time && self.shell.front_pane() == NavPane::Overview;
        let selected = self.vault.selected_conn().filter(|_| !list_in_front);
        let label = match selected {
            Some(connection) => Some(connection.name.clone()),
            None if self.shell.has_overview() => Some(String::from("Connections")),
            None => None,
        };
        toolbar.location = label.map(|label| ToolbarLocation {
            label,
            ..ToolbarLocation::default()
        });
        let mut actions = vec![ToolbarAction::new(IMPORT, "plus", "Import configuration")];
        if let Some(connection) = selected {
            toolbar.views = Some(ToolbarViews {
                name: String::from("View"),
                choices: DetailTab::ALL
                    .iter()
                    .map(|tab| ToolbarView {
                        id: tab.id().to_owned(),
                        label: tab.label().to_owned(),
                    })
                    .collect(),
                chosen: self.detail_tab.id().to_owned(),
            });
            let action = MainAction::of(connection, self.is_provisioned(connection.id));
            if action != MainAction::Connecting {
                actions.push(
                    ToolbarAction::new(MAIN_ACTION, "power", action.label())
                        .on(action == MainAction::Disconnect),
                );
            }
        }
        NavShell::push_window_actions(&mut toolbar, actions);
        toolbar
    }

    /// Hand what the person did in the window's bar to the shell's own
    /// slots, or queue it as the intent its control would have. Returns
    /// whether anything was asked.
    pub(super) fn take_window_toolbar_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            let ToolbarEvent::Action { id } = event else {
                continue;
            };
            let selected = self.vault.selected_conn().map(|connection| connection.id);
            let intent = match (id.as_str(), selected) {
                (IMPORT, _) => Intent::OpenImport,
                (MAIN_ACTION, Some(connection)) => Intent::MainAction(connection),
                (view, Some(_)) => match DetailTab::ALL.iter().find(|tab| tab.id() == view) {
                    Some(tab) => Intent::SetTab(*tab),
                    None => continue,
                },
                _ => continue,
            };
            self.intents.push(intent);
        }
        asked
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}
