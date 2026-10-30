//! The Calculator's toolbar, as slots in the window's bar
//! ([`ui::window_toolbar`]).
//!
//! The bar names the mode as its location and shows the modes as its view
//! switcher (Calculator, Graph 2D, Graph 3D, Convert), a graph's formulas as
//! its sidebar and the angle unit in its menu. Each slot is a binding of an
//! effect of the window ([`CalculatorEffect`]), which the app answers where
//! it answers every effect: the same door an assistive technology, the
//! palette and an agent take, so there is one state path for all of them.
//! Past calculations are the window's History, which the bar opens itself.

use ui::framework::{Choice, WindowBar};

use super::CalculatorApp;
use crate::actions::CalculatorEffect;
use crate::model::Mode;
use crate::resource::Calculator;

/// What shows `mode`: the effect the mode's own semantic action runs.
fn show(mode: Mode) -> CalculatorEffect {
    match mode {
        Mode::Calc => CalculatorEffect::ShowCalculator,
        Mode::Convert => CalculatorEffect::ShowConvert,
        Mode::Graph2d => CalculatorEffect::ShowGraph2d,
        Mode::Graph3d => CalculatorEffect::ShowGraph3d,
    }
}

impl CalculatorApp {
    /// The window bar's slots for the mode shown, binding each to the effect
    /// that takes it.
    pub(super) fn window_bar(&self) -> Option<WindowBar<Calculator>> {
        let dispatch = self.dispatch.as_ref()?;
        let mode = self.model.mode;
        let places = Mode::all();
        let choices = places
            .iter()
            .map(|choice| Choice::new(choice.label(), &show(*choice)));
        let mut bar = WindowBar::new(dispatch.clone()).label(mode.label()).views(
            "Mode",
            places
                .iter()
                .position(|choice| *choice == mode)
                .unwrap_or_default(),
            choices,
        );
        if let Some(kind) = mode.graph() {
            bar = bar.sidebar(
                self.panel(kind).side().is_open(),
                &CalculatorEffect::TogglePanel,
            );
        }
        for (name, effect, on) in [
            ("Radians", CalculatorEffect::UseRadians, !self.model.deg),
            ("Degrees", CalculatorEffect::UseDegrees, self.model.deg),
        ] {
            bar = bar.effect(&effect).named("ruler", name).on(on).in_menu();
        }
        Some(bar)
    }
}
