//! Pure create-draft projection and geometry shared by live and History views.

use crate::doc::CreateField;
use crate::draw::{self, TextStyle};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Point, Rect};

/// Off the spacing scale by design: one create-form row (label plus input)
/// is a fixed step shared by the live and History geometry.
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
    pub fields: Vec<FieldGeometry>,
}

pub(crate) fn create_form_geometry(full: Rect) -> CreateFormGeometry {
    let mw = 520.0_f64.min((full.width() - 48.0).max(240.0));
    let mh = 540.0_f64.min((full.height() - 32.0).max(420.0));
    let modal = Rect::from_center_size(full.center(), (mw, mh));
    let fields = create_form_fields(modal);
    CreateFormGeometry { modal, fields }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftProjection {
    pub fields: Vec<(CreateField, Option<String>)>,
    pub unknown_descendants: Vec<String>,
}

impl DraftProjection {
    fn unknown_metadata_lines(&self) -> Vec<String> {
        if self.unknown_descendants.is_empty() {
            vec!["None in this version".into()]
        } else {
            self.unknown_descendants.clone()
        }
    }

    pub(crate) fn accessibility(&self) -> String {
        let mut lines: Vec<String> = self
            .fields
            .iter()
            .map(|(field, value)| {
                format!(
                    "{}: {}",
                    field.label(),
                    value.as_deref().unwrap_or("absent")
                )
            })
            .collect();
        if self.unknown_descendants.is_empty() {
            lines.push("Unknown draft descendants: none".into());
        } else {
            lines.push(format!(
                "Unknown draft descendants: {}",
                self.unknown_descendants.join(", ")
            ));
        }
        lines.join("\n")
    }

    pub(crate) fn draw(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, full: Rect) {
        let geometry = create_form_geometry(full);
        let modal = geometry.modal;
        draw::fill_round(
            scene,
            modal,
            f64::from(theme.tokens.radii.lg),
            ui::chrome::panel_ground(theme),
        );
        draw::stroke_round(
            scene,
            modal.inset(-0.5),
            f64::from(theme.tokens.radii.lg) - 0.5,
            1.0,
            ui::chrome::panel_border(theme),
        );
        draw::text(
            scene,
            tr,
            "Container draft",
            theme.tokens.typography.scale.xl,
            theme.tokens.colors.text_primary,
            Point::new(modal.x0 + 24.0, modal.y0 + 18.0),
            TextWeight::Bold,
        );
        draw::text(
            scene,
            tr,
            "Historical unsubmitted podman run fields",
            theme.tokens.typography.scale.xs,
            theme.tokens.colors.text_dim,
            Point::new(modal.x0 + 24.0, modal.y0 + 42.0),
            TextWeight::Regular,
        );
        for field_geometry in geometry.fields {
            draw_field_frame(scene, tr, theme, field_geometry, false);
            let value = self
                .fields
                .iter()
                .find(|(field, _)| *field == field_geometry.field)
                .and_then(|(_, value)| value.as_deref())
                .unwrap_or("Absent in this version");
            draw::text_clip(
                scene,
                tr,
                value,
                &TextStyle {
                    size: theme.tokens.typography.scale.sm,
                    color: theme.tokens.colors.text_primary,
                    weight: TextWeight::Regular,
                },
                Point::new(
                    field_geometry.input.x0 + 12.0,
                    field_geometry.input.y0 + 6.0,
                ),
                (field_geometry.input.width() - 24.0).max(1.0),
            );
        }

        let metadata_x = modal.x0 + 24.0;
        let metadata_width = (modal.width() - 48.0).max(1.0);
        draw::text(
            scene,
            tr,
            "Unknown draft descendants",
            theme.tokens.typography.scale.xs,
            theme.tokens.colors.text_dim,
            Point::new(metadata_x, modal.y0 + 412.0),
            TextWeight::Medium,
        );
        let lines = self.unknown_metadata_lines();
        for (index, line) in lines.iter().take(4).enumerate() {
            draw::text_clip(
                scene,
                tr,
                line,
                &TextStyle {
                    size: theme.tokens.typography.scale.xs,
                    color: theme.tokens.colors.text_primary,
                    weight: TextWeight::Regular,
                },
                Point::new(metadata_x, modal.y0 + 430.0 + index as f64 * 20.0),
                metadata_width,
            );
        }
        if lines.len() > 4 {
            draw::text_clip(
                scene,
                tr,
                &format!("+{} more", lines.len() - 4),
                &TextStyle {
                    size: theme.tokens.typography.scale.xs,
                    color: theme.tokens.colors.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(metadata_x, modal.y0 + 510.0),
                metadata_width,
            );
        }
    }
}
