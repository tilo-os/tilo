//! The sheet's geometry: where its card, title, header, body and footer
//! actions stand inside a window, worked out before anything but the title
//! and the action labels is measured.

use vello::kurbo::{Point, Rect, Vec2};

use super::{
    ModalAction, ModalEmphasis, ModalHit, ModalLayout, ModalSpec, action_height, card_width, pad,
    title_icon_side, title_lines, title_row_height, uses_full_window,
};
use crate::interaction::InteractionEnvironment;
use crate::platform::DialogActionOrder;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::control_size::ControlSize;
use crate::widget::floating_surface;
use tilo_sort::CompactSort as _;

/// The part of `window` a sheet `wanted_h` tall may cover. A sheet that
/// fits below the window's header row (and clear of the bottom edge by the
/// content gutter) stays below it, so the row's controls stay readable. A
/// taller one takes the whole window inside the gutter: its own content
/// and footer matter more than the header row it covers.
fn sheet_room(window: Rect, wanted_h: f64, theme: &Theme) -> Rect {
    if uses_full_window(window, theme) {
        return window;
    }
    let pad = pad(theme);
    let header = f64::from(theme.tokens.metrics.header_height);
    let bottom = (window.y1 - pad).max(window.y0);
    let below_header = (window.y0 + header).min(bottom);
    let top = if wanted_h <= bottom - below_header {
        below_header
    } else {
        (window.y0 + pad).min(bottom)
    };
    Rect::new(window.x0, top, window.x1, bottom)
}

/// The side the sheet's own controls take: the theme's target, grown with the
/// text, and never under what the person's pointer needs — a finger's 48 where
/// a mouse's 32 will do.
pub(super) fn target(theme: &Theme, environment: InteractionEnvironment) -> f64 {
    f64::from(theme.tokens.metrics.min_pointer_target).max(f64::from(environment.minimum_target()))
}

/// Compute the geometry for `spec` inside `window` for a person with a
/// pointing device.
pub fn layout(
    window: Rect,
    spec: &ModalSpec<'_>,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> ModalLayout {
    layout_for(
        window,
        spec,
        text_renderer,
        theme,
        InteractionEnvironment::default(),
    )
}

/// Compute the geometry for `spec` inside `window` for a person reaching it
/// through `environment`: on a coarse pointer the sheet's own controls take a
/// finger's target.
pub fn layout_for(
    window: Rect,
    spec: &ModalSpec<'_>,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    environment: InteractionEnvironment,
) -> ModalLayout {
    let spacing = &theme.tokens.spacing;
    let pad = pad(theme);
    let w = card_width(window, spec.max_width, theme);
    let has_title = !spec.title.is_empty() || spec.closable;
    let min_target = target(theme, environment);
    let row_h = title_row_height(theme).max(min_target);
    let close_side = min_target;
    // The close control's glyph lines up with the content's trailing edge,
    // so its hit target reaches a little into the padding.
    let close_overhang = (close_side - f64::from(theme.tokens.metrics.icon_md)) / 2.0;
    // The title stops where the close control starts, so a long title wraps
    // (and then elides) instead of running under it.
    // A title icon stands at the leading edge; the title starts after it.
    let icon_lead = if spec.icon.is_some() && has_title {
        title_icon_side(theme) + f64::from(spacing.s3)
    } else {
        0.0
    };
    let title_w = if spec.closable {
        w - pad * 2.0 + close_overhang - close_side - f64::from(spacing.s2)
    } else {
        w - pad * 2.0
    } - icon_lead;
    let title_w = title_w.max(0.0);
    let lines = title_lines(text_renderer, spec.title, title_w, theme).len();
    let line_h = f64::from(theme.text(TextRole::Title).line_height_px());
    let title_h = if has_title {
        (line_h * lines.max(1) as f64).max(row_h)
    } else {
        0.0
    };
    let title_gap = if has_title {
        f64::from(spacing.s4)
    } else {
        0.0
    };
    let footer = footer_geometry(
        spec,
        (w - pad * 2.0).max(0.0),
        text_renderer,
        theme,
        min_target,
    );
    let footer_h = footer.height;
    let header_h = spec.header_height.max(0.0);
    let header_gap = if header_h > 0.0 {
        f64::from(spacing.s4)
    } else {
        0.0
    };
    let chrome_h = pad * 2.0 + title_h + title_gap + header_h + header_gap + footer_h;
    let wanted_h = chrome_h + spec.body_height.max(0.0);
    let room = sheet_room(window, wanted_h, theme);
    let avail_h = room.height();
    let h = if uses_full_window(window, theme) {
        avail_h
    } else {
        wanted_h.min(avail_h)
    };
    // The sheet never grows past the window: a footer nobody can reach is
    // worse than a body that scrolls.
    let body_overflows = wanted_h > avail_h + 0.5;

    let c = window.center();
    let top = (c.y - h / 2.0).max(room.y0);
    let anchor = spec.anchor.filter(|_| !uses_full_window(window, theme));
    let card = if uses_full_window(window, theme) {
        window
    } else if let Some(anchor) = anchor {
        floating_surface::place_sheet_beside(anchor, (w, h), window, room, f64::from(spacing.s2))
    } else {
        Rect::new(c.x - w / 2.0, top, c.x + w / 2.0, top + h)
    };
    // The sheet's edges on the display's pixels: half a pixel of centring blurs its border and
    // the actions under it across two.
    let card = floating_surface::on_device_pixels(card);
    let title_top = card.y0 + pad;

    let close = spec.closable.then(|| {
        let x1 = card.x1 - pad + close_overhang;
        let mid = title_top + row_h / 2.0;
        floating_surface::on_device_pixels(Rect::new(
            x1 - close_side,
            mid - close_side / 2.0,
            x1,
            mid + close_side / 2.0,
        ))
    });
    let title = floating_surface::on_device_pixels(Rect::new(
        card.x0 + pad + icon_lead,
        title_top,
        card.x0 + pad + icon_lead + title_w,
        title_top + title_h,
    ));

    let body_top = title.y1 + title_gap;
    let header = (header_h > 0.0).then(|| {
        floating_surface::on_device_pixels(Rect::new(
            card.x0 + pad,
            body_top,
            card.x1 - pad,
            body_top + header_h,
        ))
    });
    let body_top = header.map_or(body_top, |header| header.y1 + header_gap);
    let body = floating_surface::on_device_pixels(Rect::new(
        card.x0 + pad,
        body_top,
        card.x1 - pad,
        (card.y1 - pad - footer_h).max(body_top),
    ));

    let actions = place_actions(spec, card, &footer, theme, min_target);
    let destructive = spec
        .actions
        .iter()
        .any(|a| a.emphasis == ModalEmphasis::Danger);
    let primary = if destructive {
        spec.actions
            .iter()
            .position(|a| a.emphasis == ModalEmphasis::Secondary && a.enabled)
    } else {
        spec.actions
            .iter()
            .position(|a| a.is_default() && a.enabled)
    };

    ModalLayout {
        scrim: window,
        card,
        title,
        close,
        header,
        body,
        actions,
        body_overflows,
        primary,
        dismiss_on_scrim: spec.dismiss_on_scrim,
        anchored: anchor.is_some(),
        foot: false,
    }
}

/// Put every box of `layout` on the display's pixels once its placement is settled: a sheet
/// raised from the foot or shortened by what its body scrolls keeps its edges where pixels are.
pub(super) fn onto_device_pixels(layout: &mut super::ModalLayout) {
    layout.card = floating_surface::on_device_pixels(layout.card);
    layout.title = floating_surface::on_device_pixels(layout.title);
    layout.close = layout.close.map(floating_surface::on_device_pixels);
    layout.header = layout.header.map(floating_surface::on_device_pixels);
    layout.body = floating_surface::on_device_pixels(layout.body);
    for action in &mut layout.actions {
        if action.area() > 0.0 {
            *action = floating_surface::on_device_pixels(*action);
        }
    }
}

/// Raise `layout`, a compact window's full-window page, as a sheet from the
/// window's foot that is as tall as what it holds: `content_h` of body, the
/// title above it, the footer below it and a grip `grip` tall along its top.
/// A form of a few fields asks from the foot, where the thumb is; one too
/// tall for most of the window stays the whole page. Whether it rose.
pub fn rise_from_foot(layout: &mut ModalLayout, content_h: f64, grip: f64) -> bool {
    if layout.card != layout.scrim {
        return false;
    }
    let window = layout.scrim;
    let above = layout.body.y0 - layout.card.y0;
    let below = layout.card.y1 - layout.body.y1;
    let needed = grip + above + content_h.max(0.0) + below;
    if needed > window.height() * 0.85 {
        return false;
    }
    let card = Rect::new(window.x0, window.y1 - needed, window.x1, window.y1);
    let shift = Vec2::new(0.0, card.y0 + grip - layout.card.y0);
    layout.title = layout.title + shift;
    layout.close = layout.close.map(|close| close + shift);
    layout.header = layout.header.map(|header| header + shift);
    layout.body.y0 += shift.y;
    layout.card = card;
    layout.body_overflows = false;
    layout.foot = true;
    true
}

struct FooterGeometry {
    widths: Vec<f64>,
    stacked: bool,
    height: f64,
}

/// Reserve the footer before allocating body space. Labels are measured in
/// the real control typography, so larger text and translations can wrap
/// the action group into separate rows without covering the body.
fn footer_geometry(
    spec: &ModalSpec<'_>,
    available_width: f64,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    min_target: f64,
) -> FooterGeometry {
    let action_h = action_height(theme).max(min_target);
    let spacing = &theme.tokens.spacing;
    let label = ControlSize::Regular.label_style(theme);
    let label_pad = f64::from(spacing.s4);
    let min_w = f64::from(spacing.s12) * 2.0;
    let widths: Vec<_> = spec
        .actions
        .iter()
        .map(|action| {
            let (width, _) = text_renderer.measure_style(action.label, None, &label);
            (f64::from(width) + label_pad * 2.0).max(min_w)
        })
        .collect();
    let gap = f64::from(spacing.s2);
    let gaps = widths.len().saturating_sub(1) as f64;
    let stacked = widths.iter().sum::<f64>() + gaps * gap > available_width;
    let height = if widths.is_empty() {
        0.0
    } else {
        f64::from(spacing.s6)
            + if stacked {
                widths.len() as f64 * action_h + gaps * gap
            } else {
                action_h
            }
    };
    FooterGeometry {
        widths,
        stacked,
        height,
    }
}

/// The footer's action rects, indexed by the authored action while the
/// visual order follows emphasis and the platform's convention. Where the
/// primary action stands last, an alternate answer stands apart at the
/// leading edge (Don't save … Cancel, Save); where it stands first, the
/// alternate follows it (Save, Don't save, Cancel).
fn place_actions(
    spec: &ModalSpec<'_>,
    card: Rect,
    footer: &FooterGeometry,
    theme: &Theme,
    min_target: f64,
) -> Vec<Rect> {
    let mut actions = vec![Rect::ZERO; spec.actions.len()];
    if spec.actions.is_empty() {
        return actions;
    }
    let spacing = &theme.tokens.spacing;
    let pad = pad(theme);
    let button_h = action_height(theme).max(min_target);
    let gap = f64::from(spacing.s2);
    let by = card.y1 - pad - button_h;
    let order = spec.platform.dialog_action_order();
    let (leading, mut trailing): (Vec<usize>, Vec<usize>) =
        (0..spec.actions.len()).partition(|&i| {
            order == DialogActionOrder::PrimaryLast
                && spec.actions.get(i).is_some_and(ModalAction::is_alternate)
        });
    trailing.compact_sort_by_key(|&index| {
        let Some(a) = spec.actions.get(index) else {
            return 3;
        };
        match (order, a.is_default(), a.is_alternate()) {
            (DialogActionOrder::PrimaryFirst, true, _) => 0,
            (DialogActionOrder::PrimaryFirst, _, true) => 1,
            (DialogActionOrder::PrimaryFirst, _, _)
            | (DialogActionOrder::PrimaryLast, false, _) => 2,
            (DialogActionOrder::PrimaryLast, true, _) => 3,
        }
    });
    if footer.stacked {
        let rows_height = footer.height - f64::from(spacing.s6);
        let mut y = card.y1 - pad - rows_height;
        for index in leading.iter().chain(&trailing) {
            actions[*index] = Rect::new(card.x0 + pad, y, card.x1 - pad, y + button_h);
            y += button_h + gap;
        }
        return actions;
    }
    let mut bx = card.x1 - pad;
    for &index in trailing.iter().rev() {
        let bw = footer.widths[index];
        if let Some(slot) = actions.get_mut(index) {
            *slot = Rect::new(bx - bw, by, bx, by + button_h);
        }
        bx -= bw + gap;
    }
    let mut lx = card.x0 + pad;
    for &index in &leading {
        let bw = footer.widths[index];
        if let Some(slot) = actions.get_mut(index) {
            *slot = Rect::new(lx, by, lx + bw, by + button_h);
        }
        lx += bw + gap;
    }
    for action in &mut actions {
        if action.area() > 0.0 {
            *action = floating_surface::on_device_pixels(*action);
        }
    }
    actions
}

impl ModalLayout {
    /// Resolve a point to what it hit, controls first. `None` only for a
    /// point outside the window entirely.
    pub fn hit(&self, p: Point) -> Option<ModalHit> {
        if let Some(c) = self.close
            && c.contains(p)
        {
            return Some(ModalHit::Close);
        }
        for (i, r) in self.actions.iter().enumerate() {
            if r.area() > 0.0 && r.contains(p) {
                return Some(ModalHit::Action(i));
            }
        }
        if self.card.contains(p) {
            return Some(ModalHit::Card);
        }
        if self.scrim.contains(p) {
            return Some(ModalHit::Scrim);
        }
        None
    }

    /// True for the close control, and for the scrim when the spec allows
    /// it. Everything else is swallowed.
    pub fn dismisses(&self, p: Point) -> bool {
        match self.hit(p) {
            Some(ModalHit::Close) => true,
            Some(ModalHit::Scrim) => self.dismiss_on_scrim,
            _ => false,
        }
    }
}
