//! The Calculator's toolbar, as slots in the window's bar
//! ([`ui::window_toolbar`]).
//!
//! The bar names the mode as its location and shows the modes as its view
//! switcher (Calculator, Graph 2D, Graph 3D), a graph's formulas as its
//! sidebar and the angle unit in its menu. Past calculations are the
//! window's History, which the bar opens itself.
//! Each slot runs the same semantic action an assistive technology runs on
//! the calculator, so there is one state path for all of them.

use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use super::CalculatorApp;
use crate::actions::GraphKind;
use crate::model::Mode;

const RADIANS: &str = "calculator.angle:radians";
const DEGREES: &str = "calculator.angle:degrees";

/// The semantic target of the side panel of graph `kind`.
fn panel_target(kind: GraphKind) -> &'static str {
    match kind {
        GraphKind::Curve2d => "calculator.graph.panel:2d",
        GraphKind::Surface3d => "calculator.graph.panel:3d",
    }
}

impl CalculatorApp {
    /// The window bar's slots for the mode shown.
    fn window_toolbar(&self) -> WindowToolbar {
        let mode = self.model.mode;
        let mut toolbar = WindowToolbar {
            // The bar names what the window shows: its mode.
            location: Some(ToolbarLocation {
                label: mode.label().into(),
                ..ToolbarLocation::default()
            }),
            views: Some(ToolbarViews {
                name: "Mode".into(),
                choices: Mode::all()
                    .iter()
                    .map(|choice| ToolbarView {
                        id: format!("calculator.mode:{}", choice.index()),
                        label: choice.label().into(),
                    })
                    .collect(),
                chosen: format!("calculator.mode:{}", mode.index()),
            }),
            ..WindowToolbar::default()
        };
        if let Some(kind) = mode.graph() {
            toolbar.sidebar = Some(self.panel(kind).side().is_open());
        }
        toolbar.actions.push(
            ToolbarAction::new(RADIANS, "ruler", "Radians")
                .on(!self.model.deg)
                .in_menu(),
        );
        toolbar.actions.push(
            ToolbarAction::new(DEGREES, "ruler", "Degrees")
                .on(self.model.deg)
                .in_menu(),
        );
        toolbar
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run what the person did in the window's bar, through the same
    /// semantic actions assistive technology runs.
    pub(super) fn apply_window_toolbar(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) {
        for event in ui::window_toolbar::take_events() {
            let target = match &event {
                ToolbarEvent::ToggleSidebar => match self.model.mode.graph() {
                    Some(kind) => panel_target(kind).to_owned(),
                    None => continue,
                },
                ToolbarEvent::Action { id } if id.starts_with("calculator.") => id.clone(),
                _ => continue,
            };
            ui::widget::Widget::perform_semantic_action(
                self,
                &target,
                &ui::a11y::Action::Activate,
                edits,
                path,
            );
        }
    }
}
