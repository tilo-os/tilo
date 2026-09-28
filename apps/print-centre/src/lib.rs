//! Print Centre: a focused, live view of printers, jobs, supplies, recovery,
//! document-centric printer, queue, recovery and scanner workflows.

use ui::widget::Widget;

pub mod resource;
mod surface;
pub use surface::{DemoScenario, PrintCentre};

#[must_use]
pub fn app_root() -> Box<dyn Widget> {
    Box::new(surface::PrintCentre::new())
}

#[must_use]
pub fn app_root_demo(scenario: DemoScenario) -> Box<dyn Widget> {
    Box::new(PrintCentre::demo(scenario))
}

pub use resource::{PrintCentreResource, PrintCentreShell};
