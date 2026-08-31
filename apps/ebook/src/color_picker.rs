//! Modal HSV colour picker used by the colour-scheme manager.
//!
//! This component owns the transient colour, drag geometry, drawing, and
//! topmost hit stack. The app only persists the semantic edit it emits.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::model::{hex_string, hsv_to_rgb, rgb_to_hsv};
use crate::paint;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PickTarget {
    NewScheme,
    Slot(String, usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ColorEdit {
    pub(super) target: PickTarget,
    pub(super) rgb: [u8; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragPart {
    Sv,
    Hue,
}

#[derive(Clone)]
struct Pick {
    target: PickTarget,
    hue: f32,
    sat: f32,
    val: f32,
}

impl Pick {
    fn from_rgb(target: PickTarget, rgb: [u8; 3]) -> Self {
        let (hue, sat, val) = rgb_to_hsv(rgb);
        Self {
            target,
            hue,
            sat,
            val,
        }
    }

    fn edit(&self) -> ColorEdit {
        ColorEdit {
            target: self.target.clone(),
            rgb: hsv_to_rgb(self.hue, self.sat, self.val),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HitAction {
    Swallow,
    Cancel,
    Done,
    Swatch([u8; 3]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PickerPress {
    Swallow,
    Cancel,
    Done,
    Edit(ColorEdit),
}

#[derive(Default)]
pub(super) struct ColorPicker {
    pick: Option<Pick>,
    drag: Option<DragPart>,
    sv: Rect,
    hue: Rect,
    hits: Vec<(Rect, HitAction)>,
}

impl ColorPicker {
    pub(super) fn open(&mut self, target: PickTarget, rgb: [u8; 3]) {
        self.pick = Some(Pick::from_rgb(target, rgb));
        self.drag = None;
    }

    pub(super) fn is_open(&self) -> bool {
        self.pick.is_some()
    }

    pub(super) fn cancel(&mut self) {
        self.pick = None;
        self.drag = None;
        self.hits.clear();
    }

    pub(super) fn take(&mut self) -> Option<ColorEdit> {
        self.drag = None;
        self.pick.take().map(|pick| pick.edit())
    }

    pub(super) fn press(&mut self, point: Point) -> Option<PickerPress> {
        self.pick.as_ref()?;
        let drag = if self.sv.contains(point) {
            Some(DragPart::Sv)
        } else if self.hue.contains(point) {
            Some(DragPart::Hue)
        } else {
            None
        };
        if let Some(drag) = drag {
            self.drag = Some(drag);
            return self.steer(point).map(PickerPress::Edit);
        }
        let action = self
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, action)| action.clone());
        match action {
            Some(HitAction::Swallow) => Some(PickerPress::Swallow),
            Some(HitAction::Cancel) => Some(PickerPress::Cancel),
            Some(HitAction::Done) => Some(PickerPress::Done),
            Some(HitAction::Swatch(rgb)) => {
                let pick = self.pick.as_mut()?;
                let (hue, sat, val) = rgb_to_hsv(rgb);
                pick.hue = hue;
                pick.sat = sat;
                pick.val = val;
                Some(PickerPress::Edit(pick.edit()))
            }
            None => None,
        }
    }

    pub(super) fn drag_to(&mut self, point: Point) -> Option<ColorEdit> {
        self.drag?;
        self.steer(point)
    }

    pub(super) fn end_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }

    fn steer(&mut self, point: Point) -> Option<ColorEdit> {
        let part = self.drag?;
        let pick = self.pick.as_mut()?;
        match part {
            DragPart::Sv => {
                pick.sat =
                    ((point.x - self.sv.x0) / self.sv.width().max(1.0)).clamp(0.0, 1.0) as f32;
                pick.val = 1.0
                    - ((point.y - self.sv.y0) / self.sv.height().max(1.0)).clamp(0.0, 1.0) as f32;
            }
            DragPart::Hue => {
                pick.hue = (((point.y - self.hue.y0) / self.hue.height().max(1.0)).clamp(0.0, 1.0)
                    as f32)
                    * 360.0;
            }
        }
        Some(pick.edit())
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        cursor: Point,
    ) {
        self.hits.clear();
        self.sv = Rect::ZERO;
        self.hue = Rect::ZERO;
        let Some(pick) = self.pick.clone() else {
            return;
        };
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(0.45),
            None,
            &viewport,
        );
        self.hits.push((viewport, HitAction::Cancel));

        let width = 372.0_f64.min(viewport.width() - 40.0);
        let height = 396.0_f64.min(viewport.height() - 40.0);
        let x = viewport.x0 + (viewport.width() - width) / 2.0;
        let y = viewport.y0 + (viewport.height() - height) / 2.0;
        let panel = Rect::new(x, y, x + width, y + height);
        paint::fill_round(
            scene,
            transform,
            panel,
            f64::from(theme.tokens.radii.lg),
            colors.surfaces.raised,
        );
        paint::stroke_round(
            scene,
            transform,
            panel,
            f64::from(theme.tokens.radii.lg),
            colors.border_standard,
            1.0,
        );
        self.hits.push((panel, HitAction::Swallow));

        let ix0 = x + 20.0;
        let ix1 = x + width - 20.0;
        let title = match &pick.target {
            PickTarget::NewScheme => "New scheme — pick a background",
            PickTarget::Slot(_, 0) => "Page colour",
            PickTarget::Slot(_, 1) => "Text colour",
            PickTarget::Slot(..) => "Accent colour",
        };
        text.draw_text_weighted(
            scene,
            title,
            15.0,
            colors.text_primary,
            transform * Affine::translate((ix0, y + 16.0)),
            None,
            TextWeight::Bold,
        );

        let top = y + 46.0;
        self.draw_sv_hue(scene, transform, colors, ix0, ix1, top, 190.0, &pick);
        let preview_y = top + 206.0;
        draw_preview(scene, transform, text, colors, ix0, preview_y, &pick);
        self.draw_chips(scene, transform, theme, ix0, ix1, preview_y + 46.0, cursor);
        self.draw_buttons(scene, transform, text, theme, ix1, y + height - 46.0);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_sv_hue(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        colors: &ui::tokens::colors::Colors,
        ix0: f64,
        ix1: f64,
        top: f64,
        size: f64,
        pick: &Pick,
    ) {
        self.sv = Rect::new(ix0, top, ix0 + size, top + size);
        self.hue = Rect::new(ix1 - 22.0, top, ix1, top + size);
        let cell = size / 20.0;
        for row in 0..20 {
            for col in 0..20 {
                let sat = (col as f32 + 0.5) / 20.0;
                let val = 1.0 - (row as f32 + 0.5) / 20.0;
                let rgb = hsv_to_rgb(pick.hue, sat, val);
                paint::fill_rect(
                    scene,
                    t,
                    Rect::new(
                        self.sv.x0 + f64::from(col) * cell,
                        self.sv.y0 + f64::from(row) * cell,
                        self.sv.x0 + f64::from(col + 1) * cell + 0.8,
                        self.sv.y0 + f64::from(row + 1) * cell + 0.8,
                    ),
                    Color::from_rgb8(rgb[0], rgb[1], rgb[2]),
                );
            }
        }
        paint::stroke_round(scene, t, self.sv, 6.0, colors.border_standard, 1.0);
        let cursor = Point::new(
            self.sv.x0 + f64::from(pick.sat) * size,
            self.sv.y0 + f64::from(1.0 - pick.val) * size,
        );
        paint::circle(scene, t, cursor, 7.0, Color::from_rgba8(255, 255, 255, 230));
        paint::stroke_round(
            scene,
            t,
            Rect::new(
                cursor.x - 6.0,
                cursor.y - 6.0,
                cursor.x + 6.0,
                cursor.y + 6.0,
            ),
            6.0,
            Color::from_rgba8(0, 0, 0, 180),
            1.5,
        );
        let band = size / 48.0;
        for i in 0..48 {
            let rgb = hsv_to_rgb(i as f32 / 48.0 * 360.0, 1.0, 1.0);
            let by = self.hue.y0 + f64::from(i) * band;
            paint::fill_rect(
                scene,
                t,
                Rect::new(self.hue.x0, by, self.hue.x1, by + band + 0.8),
                Color::from_rgb8(rgb[0], rgb[1], rgb[2]),
            );
        }
        paint::stroke_round(scene, t, self.hue, 4.0, colors.border_standard, 1.0);
        let mark_y = self.hue.y0 + f64::from(pick.hue / 360.0) * size;
        paint::stroke_round(
            scene,
            t,
            Rect::new(
                self.hue.x0 - 3.0,
                mark_y - 3.0,
                self.hue.x1 + 3.0,
                mark_y + 3.0,
            ),
            3.0,
            Color::from_rgb8(250, 250, 252),
            2.0,
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "draw context plus chip row geometry"
    )]
    fn draw_chips(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        ix0: f64,
        ix1: f64,
        y: f64,
        cursor: Point,
    ) {
        const CHIPS: [[u8; 3]; 10] = [
            [0xEF, 0x44, 0x44],
            [0xF5, 0x9E, 0x0B],
            [0xFA, 0xCC, 0x15],
            [0x10, 0xB9, 0x81],
            [0x3B, 0x82, 0xF6],
            [0xA7, 0x8B, 0xFA],
            [0xF6, 0xEC, 0xD8],
            [0xFC, 0xFC, 0xFA],
            [0x2A, 0x2A, 0x2E],
            [0x14, 0x18, 0x22],
        ];
        let gap = (ix1 - ix0 - CHIPS.len() as f64 * 26.0) / (CHIPS.len() as f64 - 1.0);
        let mut x = ix0;
        for rgb in CHIPS {
            let rect = Rect::new(x, y, x + 26.0, y + 26.0);
            paint::fill_round(
                scene,
                t,
                rect,
                6.0,
                Color::from_rgb8(rgb[0], rgb[1], rgb[2]),
            );
            let hover = rect.contains(cursor);
            paint::stroke_round(
                scene,
                t,
                rect,
                6.0,
                if hover {
                    theme.border_focus
                } else {
                    theme.tokens.colors.border_standard
                },
                if hover { 2.0 } else { 1.0 },
            );
            self.hits.push((rect, HitAction::Swatch(rgb)));
            x += 26.0 + gap;
        }
    }

    fn draw_buttons(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        ix1: f64,
        y: f64,
    ) {
        let done_w = f64::from(
            text.measure_weighted("Done", 13.0, None, TextWeight::Semibold)
                .0,
        ) + 44.0;
        let done = Rect::new(ix1 - done_w, y, ix1, y + 34.0);
        paint::fill_round(scene, t, done, 17.0, theme.accent_primary);
        text.draw_text_weighted(
            scene,
            "Done",
            13.0,
            Color::from_rgb8(248, 248, 252),
            t * Affine::translate((done.x0 + 22.0, y + 9.0)),
            None,
            TextWeight::Semibold,
        );
        self.hits.push((done, HitAction::Done));
        let cancel_w = f64::from(
            text.measure_weighted("Cancel", 13.0, None, TextWeight::Medium)
                .0,
        ) + 30.0;
        let cancel = Rect::new(done.x0 - 10.0 - cancel_w, y, done.x0 - 10.0, y + 34.0);
        paint::fill_round(scene, t, cancel, 17.0, theme.tokens.colors.surfaces.overlay);
        text.draw_text_weighted(
            scene,
            "Cancel",
            13.0,
            theme.tokens.colors.text_primary,
            t * Affine::translate((cancel.x0 + 15.0, y + 9.0)),
            None,
            TextWeight::Medium,
        );
        self.hits.push((cancel, HitAction::Cancel));
    }

    #[cfg(test)]
    pub(super) fn sv_rect(&self) -> Rect {
        self.sv
    }

    #[cfg(test)]
    pub(super) fn done_center(&self) -> Option<Point> {
        self.hits
            .iter()
            .find(|(_, action)| *action == HitAction::Done)
            .map(|(rect, _)| rect.center())
    }
}

fn draw_preview(
    scene: &mut Scene,
    t: Affine,
    text: &mut TextRenderer,
    colors: &ui::tokens::colors::Colors,
    x: f64,
    y: f64,
    pick: &Pick,
) {
    let rgb = pick.edit().rgb;
    let swatch = Rect::new(x, y, x + 54.0, y + 34.0);
    paint::fill_round(
        scene,
        t,
        swatch,
        8.0,
        Color::from_rgb8(rgb[0], rgb[1], rgb[2]),
    );
    paint::stroke_round(scene, t, swatch, 8.0, colors.border_standard, 1.0);
    text.draw_text_weighted(
        scene,
        &hex_string(rgb),
        15.0,
        colors.text_primary,
        t * Affine::translate((x + 66.0, y + 9.0)),
        None,
        TextWeight::Semibold,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(picker: &mut ColorPicker) {
        let mut scene = Scene::new();
        picker.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            Rect::new(0.0, 0.0, 900.0, 700.0),
            Point::ZERO,
        );
    }

    #[test]
    fn sv_drag_is_owned_and_emits_a_semantic_edit() {
        let target = PickTarget::Slot("custom".into(), 2);
        let mut picker = ColorPicker::default();
        picker.open(target.clone(), [0x80, 0x80, 0x80]);
        draw(&mut picker);
        let sv = picker.sv_rect();
        let press = picker.press(Point::new(
            sv.x0 + sv.width() * 0.75,
            sv.y0 + sv.height() * 0.25,
        ));
        assert!(
            matches!(press, Some(PickerPress::Edit(ColorEdit { target: ref got, .. })) if *got == target)
        );
        let edit = picker
            .drag_to(Point::new(sv.x1, sv.y0))
            .expect("active drag emits an edit");
        assert_eq!(edit.target, target);
        assert!(edit.rgb[0] != edit.rgb[1] || edit.rgb[1] != edit.rgb[2]);
        assert!(picker.end_drag());
        assert!(picker.drag_to(sv.center()).is_none());
    }

    #[test]
    fn panel_swallow_and_scrim_cancel_are_topmost_private_hits() {
        let mut picker = ColorPicker::default();
        picker.open(PickTarget::NewScheme, [20, 30, 40]);
        draw(&mut picker);
        assert_eq!(
            picker.press(Point::new(450.0, 170.0)),
            Some(PickerPress::Swallow)
        );
        assert_eq!(
            picker.press(Point::new(4.0, 4.0)),
            Some(PickerPress::Cancel)
        );
    }
}
