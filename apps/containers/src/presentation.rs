//! Create-form geometry and field chrome.

use crate::doc::CreateField;
use crate::draw;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Point, Rect};

/// Off the spacing scale by design: one create-form row (label plus input)
/// is a fixed step.
pub(crate) const ROW_STEP: f64 = 56.0;

#[derive(Clone, Copy, Debug)]
pub(crate) struct FieldGeometry {
    pub field: CreateField,
    pub label_at: Point,
    pub input: Rect,
}

#[derive(Clone, Debug)]
pub(crate) struct CreateFormGeometry {
    pub modal: Rect,
}

pub(crate) fn create_form_geometry(full: Rect) -> CreateFormGeometry {
    let mw = 520.0_f64.min((full.width() - 48.0).max(240.0));
    let mh = 540.0_f64.min((full.height() - 32.0).max(420.0));
    let modal = Rect::from_center_size(full.center(), (mw, mh));
    CreateFormGeometry { modal }
}

pub(crate) fn create_form_fields(modal: Rect) -> Vec<FieldGeometry> {
    CreateField::ALL
        .into_iter()
        .enumerate()
        .map(|(index, field)| {
            let y = modal.y0 + 68.0 + index as f64 * ROW_STEP;
            FieldGeometry {
                field,
                label_at: Point::new(modal.x0 + 24.0, y),
                input: Rect::new(modal.x0 + 24.0, y + 14.0, modal.x1 - 24.0, y + 42.0),
            }
        })
        .collect()
}

pub(crate) fn draw_field_frame(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    geometry: FieldGeometry,
    focused: bool,
) {
    let colors = &theme.tokens.colors;
    draw::text(
        scene,
        tr,
        geometry.field.label(),
        theme.tokens.typography.scale.xs,
        colors.text_dim,
        geometry.label_at,
        TextWeight::Medium,
    );
    if focused {
        draw::fill_round(
            scene,
            geometry.input,
            f64::from(theme.tokens.radii.md),
            colors.surfaces.base,
        );
        draw::stroke_round(
            scene,
            geometry.input,
            f64::from(theme.tokens.radii.md),
            2.0,
            colors.border_focus,
        );
    } else {
        draw::card(
            scene,
            geometry.input,
            f64::from(theme.tokens.radii.md),
            colors.surfaces.base,
        );
    }
}
