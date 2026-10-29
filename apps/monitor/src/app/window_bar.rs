//! Monitor's controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the Processes and Resources views, Refresh, the update frequency as a menu
//! button, and on the process list Tree view, with Include children and
//! Kernel threads in the bar's menu. The bar's search narrows the process
//! list ([`crate::view::ProcessesPane`] is its `Filterable`). The window
//! draws no header row and the list no row of toggles of its own.

use ui::framework::BarCommands;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarView, ToolbarViews, WindowToolbar};

use super::{MonitorApp, Tab};
use crate::model::ProcessDataStatus;
use crate::tree::SumMode;
use crate::view::{Cadence, Request};

/// What an entry of the window bar does.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum BarCommand {
    Refresh,
    TreeMode,
    SumChildren,
    KernelThreads,
    /// Sample at this cadence.
    Cadence(Cadence),
}

impl Tab {
    /// The view's id in the window bar.
    pub(crate) fn view_id(self) -> &'static str {
        match self {
            Self::Processes => "monitor.view.processes",
            Self::Resources => "monitor.view.resources",
        }
    }
}

impl MonitorApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let state = self.state.borrow();
        toolbar.views = Some(ToolbarViews {
            name: String::from("View"),
            choices: Tab::ALL
                .iter()
                .map(|tab| ToolbarView {
                    id: tab.view_id().to_owned(),
                    label: tab.label().to_owned(),
                })
                .collect(),
            chosen: state.tab.view_id().to_owned(),
        });
        let bar: &BarCommands<BarCommand> = &self.bar;
        let chosen = state.cadence();
        let cadences = Cadence::all()
            .into_iter()
            .map(|cadence| {
                let icon = if cadence == Cadence::Paused {
                    "pause"
                } else {
                    "timer"
                };
                bar.entry(icon, cadence.label(), BarCommand::Cadence(cadence))
                    .on(cadence == chosen)
            })
            .collect();
        let mut actions = vec![
            bar.entry("refresh", "Refresh", BarCommand::Refresh),
            ToolbarAction::new(
                "monitor.cadence",
                "timer",
                format!("Update frequency: {}", chosen.label()),
            )
            .with_items(cadences),
        ];
        let lists = state.tab == Tab::Processes
            && state.model.process_status == ProcessDataStatus::Ready
            && state.shared.borrow().total > 0;
        if lists {
            actions.push(
                bar.entry("list-tree", "Tree view", BarCommand::TreeMode)
                    .on(state.tree_mode),
            );
            if state.tree_mode {
                actions.push(
                    bar.entry("layers", "Include children", BarCommand::SumChildren)
                        .on(state.sum_mode == SumMode::SumSubtree)
                        .in_menu(),
                );
            }
            actions.push(
                bar.entry("eye", "Kernel threads", BarCommand::KernelThreads)
                    .on(state.prefs.show_kernel_threads)
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
            let mut state = self.state.borrow_mut();
            let request = match self.bar.pressed(&event) {
                Some(BarCommand::Cadence(cadence)) => {
                    state.set_cadence(cadence);
                    continue;
                }
                Some(BarCommand::Refresh) => Request::Refresh,
                Some(BarCommand::TreeMode) => Request::TreeMode(!state.tree_mode),
                Some(BarCommand::SumChildren) => {
                    Request::SumChildren(state.sum_mode != SumMode::SumSubtree)
                }
                Some(BarCommand::KernelThreads) => {
                    Request::KernelThreads(!state.prefs.show_kernel_threads)
                }
                None => {
                    let ToolbarEvent::Action { id } = event else {
                        continue;
                    };
                    match Tab::ALL.into_iter().find(|tab| tab.view_id() == id) {
                        Some(tab) => Request::Tab(tab),
                        None => continue,
                    }
                }
            };
            state.shared.borrow_mut().push(request);
        }
        asked
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}
