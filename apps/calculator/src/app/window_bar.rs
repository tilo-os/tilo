//! The Calculator's toolbar: the mode as the bar's location, the modes as its
//! view switcher, a graph's formulas as its sidebar. Each slot binds a
//! [`CalculatorEffect`], so the bar takes the same path as the palette and an
//! agent.

use ui::framework::{Choice, WindowBar};

use super::CalculatorApp;
use crate::actions::CalculatorEffect;
use crate::model::Mode;
use crate::resource::Calculator;

fn show(mode: Mode) -> CalculatorEffect {
    match mode {
        Mode::Calc => CalculatorEffect::ShowCalculator,
        Mode::Convert => CalculatorEffect::ShowConvert,
        Mode::Graph2d => CalculatorEffect::ShowGraph2d,
        Mode::Graph3d => CalculatorEffect::ShowGraph3d,
    }
}

impl CalculatorApp {
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
