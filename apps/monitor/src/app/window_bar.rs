//! Monitor's controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the Processes and Resources views, Refresh, the update frequency as a menu
//! button, and on the process list Tree view, with Include children and
//! Kernel threads in the bar's menu. The bar's search narrows the process
//! list ([`crate::view::ProcessesPane`] is its `Filterable`). The window
//! draws no header row and the list no row of toggles of its own.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarView, ToolbarViews, WindowToolbar};

use super::{MonitorApp, Tab};
use crate::model::ProcessDataStatus;
use crate::tree::SumMode;
use crate::view::{Cadence, Request};

pub(crate) const REFRESH: &str = "monitor.refresh";
pub(crate) const CADENCE: &str = "monitor.cadence";
pub(crate) const TREE: &str = "monitor.tree-mode";
pub(crate) const SUM_CHILDREN: &str = "monitor.sum-children";
pub(crate) const KERNEL_THREADS: &str = "monitor.kernel-threads";

impl Tab {
    /// The view's id in the window bar.
    pub(crate) fn view_id(self) -> &'static str {
        match self {
            Self::Processes => "monitor.view.processes",
            Self::Resources => "monitor.view.resources",
        }
    }
}

impl Cadence {
    /// The choice's id in the bar's update frequency menu.
    pub(crate) fn item_id(self) -> String {
        match self {
            Self::Every(ms) => format!("{CADENCE}.{ms}"),
            Self::Paused => format!("{CADENCE}.paused"),
        }
    }
}

impl MonitorApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        toolbar.views = Some(ToolbarViews {
            name: String::from("View"),
            choices: Tab::ALL
                .iter()
                .map(|tab| ToolbarView {
                    id: tab.view_id().to_owned(),
                    label: tab.label().to_owned(),
                })
                .collect(),
            chosen: self.tab.view_id().to_owned(),
        });
        let chosen = self.cadence();
        let cadences = Cadence::all()
            .into_iter()
            .map(|cadence| {
                let icon = if cadence == Cadence::Paused {
                    "pause"
                } else {
                    "timer"
                };
                ToolbarAction::new(cadence.item_id(), icon, cadence.label()).on(cadence == chosen)
            })
            .collect();
        let mut actions = vec![
            ToolbarAction::new(REFRESH, "refresh", "Refresh"),
            ToolbarAction::new(
                CADENCE,
                "timer",
                format!("Update frequency: {}", chosen.label()),
            )
            .with_items(cadences),
        ];
        let lists = self.tab == Tab::Processes
            && self.model.process_status == ProcessDataStatus::Ready
            && self.shared.borrow().total > 0;
        if lists {
            actions.push(ToolbarAction::new(TREE, "list-tree", "Tree view").on(self.tree_mode));
            if self.tree_mode {
                actions.push(
                    ToolbarAction::new(SUM_CHILDREN, "layers", "Include children")
                        .on(self.sum_mode == SumMode::SumSubtree)
                        .in_menu(),
                );
            }
            actions.push(
                ToolbarAction::new(KERNEL_THREADS, "eye", "Kernel threads")
                    .on(self.prefs.show_kernel_threads)
                    .in_menu(),
            );
        }
        ui::widget::NavShell::push_window_actions(&mut toolbar, actions);
        toolbar
    }

    /// Queue what the person did in the window's bar as the request its
    /// control would have made, after the shell's own slots. Returns whether
    /// anything was asked.
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
            if let Some(cadence) = Cadence::all()
                .into_iter()
                .find(|cadence| cadence.item_id() == id)
            {
                self.set_cadence(cadence);
                continue;
            }
            let request = match id.as_str() {
                REFRESH => Request::Refresh,
                TREE => Request::TreeMode(!self.tree_mode),
                SUM_CHILDREN => Request::SumChildren(self.sum_mode != SumMode::SumSubtree),
                KERNEL_THREADS => Request::KernelThreads(!self.prefs.show_kernel_threads),
                view => match Tab::ALL.into_iter().find(|tab| tab.view_id() == view) {
                    Some(tab) => Request::Tab(tab),
                    None => continue,
                },
            };
            self.shared.borrow_mut().push(request);
        }
        asked
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}
