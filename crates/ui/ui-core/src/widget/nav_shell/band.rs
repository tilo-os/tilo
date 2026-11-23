//! The band across the window's foot ([`NavShell::bottom_band`]): what belongs to the whole
//! window rather than to what it shows, such as a song playing whatever page is open or a
//! call's controls.

use std::cell::Cell;

use vello::kurbo::Rect;

use super::layout::plan_columns;
use super::{
    Frame, Inputs, ManualContext, NavGeometry, NavShell, Part, WidgetLayoutError, measure,
};
use crate::widget::Element;

#[derive(Clone, Debug, Default)]
pub(super) struct Band {
    pub(super) has: bool,
    height: Cell<f64>,
}

impl NavShell {
    /// Put `band` across the window's foot, under every column: the sidebar, the inspector and
    /// their drawers stop above it, and it is as tall as it measures at the window's width.
    #[must_use]
    pub fn bottom_band(mut self, band: Element) -> Self {
        self.inputs.borrow_mut().band.has = true;
        self.band = Some(band);
        self.structure_dirty = true;
        self
    }

    pub(super) fn paint_band(&mut self, f: &mut Frame<'_>) {
        let rect = self.geometry.band;
        if !self.built_has(Part::Band) || rect.height() <= 0.0 {
            return;
        }
        let theme = f.theme.clone();
        crate::surface::separator(
            f.scene,
            f.transform,
            rect.x0,
            rect.x1,
            rect.y0,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        f.draw(self.part_mut(Part::Band), &theme);
    }
}

pub(super) fn plan(inputs: &Inputs, w: f64, h: f64) -> NavGeometry {
    let band = if inputs.band.has {
        inputs.band.height.get().clamp(0.0, h)
    } else {
        0.0
    };
    let mut g = plan_columns(inputs, w, h - band);
    if band > 0.0 {
        g.band = Rect::new(0.0, h - band, w, h);
    }
    g
}

pub(super) fn measure_band(
    inputs: &Inputs,
    ctx: &mut ManualContext<'_>,
    width: f64,
) -> Result<(), WidgetLayoutError> {
    if let Some(index) = inputs.parts.iter().position(|part| *part == Part::Band) {
        let (_, height) = measure(ctx, index, Some(width))?;
        inputs.band.height.set(height);
    }
    Ok(())
}
