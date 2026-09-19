//! Pure retention-policy presentation shared by the live editor and History.

use crate::doc::{BackupsDoc, PolicyField, StoredRetention};
use crate::model::RetentionPolicy;
use crate::paint;
use automerge::{AutoCommit, ObjId};
use ui::widget::{Button, ButtonFace};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

const PAD: f64 = 16.0;
const ROW_H: f64 = 56.0;
const ROW_GAP: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionSlot {
    pub field: PolicyField,
    pub stored: Option<i64>,
    pub effective: u32,
    pub default_unrecorded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPresentation {
    pub slots: [RetentionSlot; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetentionControl {
    pub rect: Rect,
    pub field: PolicyField,
    pub delta: i64,
}

impl Default for RetentionPresentation {
    fn default() -> Self {
        let policy = RetentionPolicy::default();
        Self {
            slots: [
                RetentionSlot {
                    field: PolicyField::Hourly,
                    stored: None,
                    effective: policy.hourly,
                    default_unrecorded: true,
                },
                RetentionSlot {
                    field: PolicyField::Daily,
                    stored: None,
                    effective: policy.daily,
                    default_unrecorded: true,
                },
                RetentionSlot {
                    field: PolicyField::Weekly,
                    stored: None,
                    effective: policy.weekly,
                    default_unrecorded: true,
                },
                RetentionSlot {
                    field: PolicyField::Monthly,
                    stored: None,
                    effective: policy.monthly,
                    default_unrecorded: true,
                },
            ],
        }
    }
}

impl RetentionPresentation {
    pub fn from_doc(doc: &AutoCommit, root: &ObjId) -> Self {
        let slot = |field| {
            let stored = BackupsDoc::stored_retention(doc, root, field);
            RetentionSlot {
                field,
                stored: match stored {
                    StoredRetention::Int(value) => Some(value),
                    _ => None,
                },
                effective: BackupsDoc::retention(doc, root, field),
                default_unrecorded: stored == StoredRetention::Absent,
            }
        };
        Self {
            slots: [
                slot(PolicyField::Hourly),
                slot(PolicyField::Daily),
                slot(PolicyField::Weekly),
                slot(PolicyField::Monthly),
            ],
        }
    }

    pub fn effective_policy(self) -> RetentionPolicy {
        RetentionPolicy {
            hourly: self.slots[0].effective,
            daily: self.slots[1].effective,
            weekly: self.slots[2].effective,
            monthly: self.slots[3].effective,
        }
    }

    pub fn accessibility(self) -> String {
        self.slots
            .iter()
            .map(|slot| {
                let value = slot
                    .stored
                    .map_or_else(|| slot.effective.to_string(), |value| value.to_string());
                if slot.default_unrecorded {
                    format!("{}: {value} (default, not recorded)", slot.field.label())
                } else if slot.stored.is_none() {
                    format!(
                        "{}: {value} (invalid stored value; displaying default)",
                        slot.field.label()
                    )
                } else {
                    format!("{}: {value}", slot.field.label())
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn panel_height(scale: f64) -> f64 {
        PAD + 4.0 * (ROW_H + (scale - 1.0) * 44.0) + 3.0 * (ROW_GAP + (scale - 1.0) * 8.0) + PAD
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Retention editor painting combines render context, panel geometry, focus and hover state, and interaction mode so live and history views share the same rows."
    )]
    pub fn draw_editor(
        self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        focused: Option<usize>,
        hovered: Option<(PolicyField, i64)>,
        interactive: bool,
    ) -> Vec<RetentionControl> {
        paint::card(
            scene,
            transform,
            panel,
            16.0,
            theme.tokens.colors.surfaces.raised,
        );
        let scale = f64::from(text.text_scale());
        let row_h = ROW_H + (scale - 1.0) * 44.0;
        let row_gap = ROW_GAP + (scale - 1.0) * 8.0;
        let mut controls = Vec::new();
        for (index, slot) in self.slots.iter().enumerate() {
            let y = panel.y0 + PAD + index as f64 * (row_h + row_gap);
            let row = Rect::new(panel.x0 + 12.0, y, panel.x1 - 12.0, y + row_h);
            if focused == Some(index) {
                paint::fill_rrect(scene, transform, row, 10.0, theme.tokens.colors.lift(0.5));
                paint::stroke_rrect(
                    scene,
                    transform,
                    row,
                    10.0,
                    theme.tokens.colors.border_focus,
                    2.0,
                );
            }
            paint::dot(
                scene,
                transform,
                Point::new(row.x0 + 18.0 * scale, row.center().y),
                5.0,
                slot_color(theme, slot.field),
            );
            paint::text_bold(
                scene,
                text,
                transform,
                slot.field.label(),
                14.5,
                theme.tokens.colors.text_primary,
                row.x0 + 34.0 * scale,
                row.y0 + 8.0 * scale,
            );
            let value = slot
                .stored
                .map_or_else(|| slot.effective.to_string(), |value| value.to_string());
            let note = if slot.default_unrecorded {
                format!("{value} · default, not recorded")
            } else if slot.stored.is_none() {
                format!("{value} · invalid stored value")
            } else {
                format!("{value} · keep over the {}", slot.field.unit())
            };
            paint::text(
                scene,
                text,
                transform,
                &note,
                11.0,
                theme.tokens.colors.text_dim,
                row.x0 + 34.0 * scale,
                row.y0 + 30.0 * scale,
            );
            if interactive {
                let cy = row.center().y;
                let plus = Rect::new(
                    row.x1 - 44.0 * scale,
                    cy - 18.0 * scale,
                    row.x1 - 8.0 * scale,
                    cy + 18.0 * scale,
                );
                let value_left = plus.x0 - 64.0 * scale;
                let minus = Rect::new(
                    value_left - 44.0 * scale,
                    cy - 18.0 * scale,
                    value_left - 8.0 * scale,
                    cy + 18.0 * scale,
                );
                for (rect, glyph, delta) in [(minus, "–", -1), (plus, "+", 1)] {
                    Button::paint(
                        scene,
                        transform,
                        text,
                        theme,
                        &ButtonFace::new(rect, glyph)
                            .radius(8.0)
                            .text_size(18.0)
                            .hovered(hovered == Some((slot.field, delta))),
                    );
                    controls.push(RetentionControl {
                        rect,
                        field: slot.field,
                        delta,
                    });
                }
                paint::text_center(
                    scene,
                    text,
                    transform,
                    &slot.effective.to_string(),
                    20.0,
                    theme.tokens.colors.text_primary,
                    value_left - 8.0 * scale,
                    plus.x0,
                    cy - 12.0 * scale,
                );
            }
        }
        controls
    }
}

fn slot_color(theme: &Theme, field: PolicyField) -> vello::peniko::Color {
    match field {
        PolicyField::Hourly => theme.tokens.colors.semantic.info,
        PolicyField::Daily => theme.accent_primary,
        PolicyField::Weekly => theme.tokens.colors.semantic.success,
        PolicyField::Monthly => theme.tokens.colors.semantic.warning,
    }
}
