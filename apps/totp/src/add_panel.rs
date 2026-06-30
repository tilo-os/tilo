//! The "Add account" modal — the app's second screen.
//!
//! Two ways in, both landing in the same [`crate::model::Account`]:
//!
//!  * **Paste an `otpauth://` URI** (the string a QR code encodes) into
//!    the top field and press *Parse* — every parameter (secret, issuer,
//!    label, algorithm, digits, period) is decoded and fills the form.
//!  * **Manual entry** — issuer, account label, base32 secret, plus
//!    segmented selectors for the algorithm (SHA1/256/512) and digit
//!    count (6/8), and a period field.
//!
//! The panel is driven imperatively by [`crate::app::AuthenticatorApp`]
//! (laid out by hand, not via taffy) because it is a transient overlay.
//! It owns its [`ui::widget::TextInput`] fields in local mode and manages
//! focus among them itself, forwarding key/pointer events to the focused
//! field. Results surface as an [`AddPanelEvent`] drained by the app.

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use uuid::Uuid;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use vello::Scene;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::model::{self, Account, Algorithm};

/// Outcome the app acts on.
#[derive(Clone, Debug)]
pub enum AddPanelEvent {
    Cancel,
    Save(Account),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Uri,
    Issuer,
    Label,
    Secret,
    Period,
}

const FIELD_ORDER: [Field; 5] = [
    Field::Uri,
    Field::Issuer,
    Field::Label,
    Field::Secret,
    Field::Period,
];

const MODAL_W: f64 = 520.0;
const MODAL_H: f64 = 560.0;
const INPUT_H: f64 = 40.0;

pub struct AddAccountPanel {
    open: bool,
    window: Rect,
    modal: Rect,

    uri: TextInput,
    issuer: TextInput,
    label: TextInput,
    secret: TextInput,
    period: TextInput,

    algorithm: Algorithm,
    digits: u8,

    focused: Option<Field>,
    last_mouse: Point,
    error: String,
    event: Option<AddPanelEvent>,

    // Laid-out hit rects, recomputed in `layout`.
    uri_rect: Rect,
    parse_rect: Rect,
    issuer_rect: Rect,
    label_rect: Rect,
    secret_rect: Rect,
    period_rect: Rect,
    alg_rects: [Rect; 3],
    digit_rects: [Rect; 2],
    cancel_rect: Rect,
    save_rect: Rect,
}

impl Default for AddAccountPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl AddAccountPanel {
    pub fn new() -> Self {
        Self {
            open: false,
            window: Rect::ZERO,
            modal: Rect::ZERO,
            uri: TextInput::local(),
            issuer: TextInput::local(),
            label: TextInput::local(),
            secret: TextInput::local(),
            period: TextInput::local().with_initial_value("30"),
            algorithm: Algorithm::Sha1,
            digits: 6,
            focused: None,
            last_mouse: Point::ZERO,
            error: String::new(),
            event: None,
            uri_rect: Rect::ZERO,
            parse_rect: Rect::ZERO,
            issuer_rect: Rect::ZERO,
            label_rect: Rect::ZERO,
            secret_rect: Rect::ZERO,
            period_rect: Rect::ZERO,
            alg_rects: [Rect::ZERO; 3],
            digit_rects: [Rect::ZERO; 2],
            cancel_rect: Rect::ZERO,
            save_rect: Rect::ZERO,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the panel, resetting every field to defaults.
    pub fn open(&mut self) {
        self.open = true;
        self.error.clear();
        self.uri.set_text("");
        self.issuer.set_text("");
        self.label.set_text("");
        self.secret.set_text("");
        self.period.set_text("30");
        self.algorithm = Algorithm::Sha1;
        self.digits = 6;
        self.set_focus(Some(Field::Issuer));
    }

    pub fn close(&mut self) {
        self.open = false;
        self.set_focus(None);
    }

    pub fn take_event(&mut self) -> Option<AddPanelEvent> {
        self.event.take()
    }

    fn field_mut(&mut self, f: Field) -> &mut TextInput {
        match f {
            Field::Uri => &mut self.uri,
            Field::Issuer => &mut self.issuer,
            Field::Label => &mut self.label,
            Field::Secret => &mut self.secret,
            Field::Period => &mut self.period,
        }
    }

    fn set_focus(&mut self, f: Option<Field>) {
        for field in FIELD_ORDER {
            let on = Some(field) == f;
            self.field_mut(field).set_focus(on);
        }
        self.focused = f;
    }

    fn focus_next(&mut self, backward: bool) {
        let cur = self
            .focused
            .and_then(|f| FIELD_ORDER.iter().position(|x| *x == f))
            .unwrap_or(0);
        let n = FIELD_ORDER.len();
        let next = if backward {
            (cur + n - 1) % n
        } else {
            (cur + 1) % n
        };
        self.set_focus(Some(FIELD_ORDER[next]));
    }

    /// Recompute the modal + every field rect for the given window.
    pub fn layout(&mut self, window: Rect) {
        self.window = window;
        let w = MODAL_W.min(window.width() - 48.0);
        let h = MODAL_H.min(window.height() - 48.0);
        let x0 = window.center().x - w / 2.0;
        let y0 = window.center().y - h / 2.0;
        self.modal = Rect::new(x0, y0, x0 + w, y0 + h);

        let pad = 24.0;
        let left = x0 + pad;
        let right = x0 + w - pad;
        let full_w = right - left;
        let mut y = y0 + 64.0; // below title

        // URI row: field + Parse button.
        let parse_w = 96.0;
        self.uri_rect = Rect::new(left, y, right - parse_w - 8.0, y + INPUT_H);
        self.parse_rect = Rect::new(right - parse_w, y, right, y + INPUT_H);
        y += INPUT_H + 28.0; // gap for "or enter manually" divider

        let label_gap = 18.0;
        let row_gap = 14.0;

        self.issuer_rect = Rect::new(left, y + label_gap, right, y + label_gap + INPUT_H);
        y += label_gap + INPUT_H + row_gap;

        self.label_rect = Rect::new(left, y + label_gap, right, y + label_gap + INPUT_H);
        y += label_gap + INPUT_H + row_gap;

        self.secret_rect = Rect::new(left, y + label_gap, right, y + label_gap + INPUT_H);
        y += label_gap + INPUT_H + row_gap;

        // Algorithm chips (left), digits chips (mid), period (right).
        let chip_y = y + label_gap;
        let alg_w = 64.0;
        for i in 0..3 {
            let cx = left + i as f64 * (alg_w + 6.0);
            self.alg_rects[i] = Rect::new(cx, chip_y, cx + alg_w, chip_y + INPUT_H);
        }
        let dig_w = 52.0;
        let dig_x = left + full_w * 0.56;
        for i in 0..2 {
            let cx = dig_x + i as f64 * (dig_w + 6.0);
            self.digit_rects[i] = Rect::new(cx, chip_y, cx + dig_w, chip_y + INPUT_H);
        }
        let period_x = right - 84.0;
        self.period_rect = Rect::new(period_x, chip_y, right, chip_y + INPUT_H);
        // (chip row is the last laid-out content row before the footer)

        // Footer buttons.
        let btn_w = 120.0;
        let btn_h = 42.0;
        let by = y0 + h - pad - btn_h;
        self.save_rect = Rect::new(right - btn_w, by, right, by + btn_h);
        self.cancel_rect = Rect::new(right - btn_w * 2.0 - 12.0, by, right - btn_w - 12.0, by + btn_h);

        // Push rects into the text inputs.
        self.uri.apply_layout_external(self.uri_rect);
        self.issuer.apply_layout_external(self.issuer_rect);
        self.label.apply_layout_external(self.label_rect);
        self.secret.apply_layout_external(self.secret_rect);
        self.period.apply_layout_external(self.period_rect);
    }

    /// Build an account from the current field values, or an error
    /// string describing why it can't be saved.
    fn build_account(&self) -> Result<Account, String> {
        let secret = model::normalise_secret(self.secret.value());
        if secret.is_empty() {
            return Err("Enter a base32 secret".to_string());
        }
        if crate::totp::decode_secret(&secret).map(|b| b.is_empty()).unwrap_or(true) {
            return Err("Secret is not valid base32".to_string());
        }
        let issuer = self.issuer.value().trim().to_string();
        let label = self.label.value().trim().to_string();
        if issuer.is_empty() && label.is_empty() {
            return Err("Enter an issuer or account name".to_string());
        }
        let period = self.period.value().trim().parse::<u32>().unwrap_or(30).clamp(5, 300);
        // Pick a colour slot from the issuer so new accounts get a stable
        // distinct hue; the app may override.
        let color = (issuer.len().wrapping_add(label.len()) % 7) as u8;
        Ok(Account {
            id: Uuid::new_v4(),
            issuer,
            label,
            secret,
            algorithm: self.algorithm,
            digits: self.digits,
            period,
            color,
            order: 0,
        })
    }

    /// Parse the URI field into the manual fields.
    fn parse_uri(&mut self) {
        let uri = self.uri.value().to_string();
        if uri.trim().is_empty() {
            self.error = "Paste an otpauth:// URI first".to_string();
            return;
        }
        match model::parse_otpauth_uri(&uri, Uuid::new_v4()) {
            Ok(acc) => {
                self.issuer.set_text(&acc.issuer);
                self.label.set_text(&acc.label);
                self.secret.set_text(&acc.secret);
                self.period.set_text(&acc.period.to_string());
                self.algorithm = acc.algorithm;
                self.digits = acc.digits;
                self.error.clear();
                self.set_focus(Some(Field::Issuer));
            }
            Err(e) => self.error = e,
        }
    }

    fn try_save(&mut self) {
        match self.build_account() {
            Ok(acc) => {
                self.event = Some(AddPanelEvent::Save(acc));
                self.open = false;
            }
            Err(e) => self.error = e,
        }
    }

    // ---- drawing -----------------------------------------------------

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.open {
            return;
        }
        let presence = PresenceCtx::empty();
        // Scrim.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &self.window,
        );
        // Modal card.
        let card = RoundedRect::from_rect(self.modal, theme.tokens.radii.xl as f64);
        scene.fill(Fill::NonZero, transform, theme.tokens.colors.surfaces.raised, None, &card);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &card,
        );

        let left = self.modal.x0 + 24.0;
        // Title.
        text_renderer.draw_text_weighted(
            scene,
            "Add account",
            22.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((left, self.modal.y0 + 22.0)),
            None,
            TextWeight::Bold,
        );

        // URI field + Parse button.
        self.draw_field_label(scene, transform, text_renderer, theme, "Scan / paste otpauth:// URI", self.uri_rect, false);
        if self.uri.value().is_empty() {
            self.draw_placeholder(scene, transform, text_renderer, theme, self.uri_rect, "otpauth://totp/Issuer:account?secret=…");
        }
        self.uri.draw(scene, transform, text_renderer, theme, &presence);
        self.draw_button(scene, transform, text_renderer, theme, self.parse_rect, "Parse", false, true);

        // "or enter manually" divider.
        let div_y = (self.uri_rect.y1 + self.issuer_rect.y0) / 2.0 - 2.0;
        self.draw_divider(scene, transform, text_renderer, theme, div_y, "or enter manually");

        // Manual fields.
        self.draw_text_field(scene, transform, text_renderer, theme, "Issuer", Field::Issuer, "GitHub");
        self.draw_text_field(scene, transform, text_renderer, theme, "Account name", Field::Label, "ada@example.com");
        self.draw_text_field(scene, transform, text_renderer, theme, "Secret (base32)", Field::Secret, "JBSWY3DPEHPK3PXP");

        // Algorithm + digits + period row labels.
        self.draw_field_label(scene, transform, text_renderer, theme, "Algorithm", self.alg_rects[0], false);
        for (i, alg) in Algorithm::ALL.iter().enumerate() {
            let selected = *alg == self.algorithm;
            let hovered = self.alg_rects[i].contains(self.last_mouse);
            self.draw_chip(scene, transform, text_renderer, theme, self.alg_rects[i], alg.as_str(), selected, hovered);
        }
        self.draw_field_label(scene, transform, text_renderer, theme, "Digits", self.digit_rects[0], false);
        for (i, d) in [6u8, 8u8].iter().enumerate() {
            let selected = *d == self.digits;
            let hovered = self.digit_rects[i].contains(self.last_mouse);
            self.draw_chip(scene, transform, text_renderer, theme, self.digit_rects[i], &d.to_string(), selected, hovered);
        }
        self.draw_field_label(scene, transform, text_renderer, theme, "Period", self.period_rect, false);
        self.period.draw(scene, transform, text_renderer, theme, &presence);

        // Error line.
        if !self.error.is_empty() {
            text_renderer.draw_text(
                scene,
                &self.error,
                12.0,
                theme.tokens.colors.semantic.danger,
                transform * Affine::translate((left, self.cancel_rect.y0 - 22.0)),
                None,
            );
        }

        // Footer buttons.
        let can_save = !self.secret.value().trim().is_empty();
        let cancel_hover = self.cancel_rect.contains(self.last_mouse);
        let save_hover = self.save_rect.contains(self.last_mouse);
        self.draw_button(scene, transform, text_renderer, theme, self.cancel_rect, "Cancel", cancel_hover, true);
        self.draw_primary_button(scene, transform, text_renderer, theme, self.save_rect, "Add account", save_hover, can_save);
    }

    fn draw_field_label(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        label: &str,
        field_rect: Rect,
        _focused: bool,
    ) {
        text_renderer.draw_text_weighted(
            scene,
            label,
            12.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((field_rect.x0, field_rect.y0 - 16.0)),
            None,
            TextWeight::Medium,
        );
    }

    fn draw_text_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        label: &str,
        field: Field,
        placeholder: &str,
    ) {
        let presence = PresenceCtx::empty();
        let rect = match field {
            Field::Issuer => self.issuer_rect,
            Field::Label => self.label_rect,
            Field::Secret => self.secret_rect,
            _ => return,
        };
        self.draw_field_label(scene, transform, text_renderer, theme, label, rect, false);
        let empty = self.field_mut(field).value().is_empty();
        if empty {
            self.draw_placeholder(scene, transform, text_renderer, theme, rect, placeholder);
        }
        self.field_mut(field).draw(scene, transform, text_renderer, theme, &presence);
    }

    fn draw_placeholder(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        text: &str,
    ) {
        let pad = theme.padding_standard as f64;
        text_renderer.draw_text(
            scene,
            text,
            16.0,
            theme.tokens.colors.text_dim.with_alpha_factor(0.6),
            transform * Affine::translate((rect.x0 + pad, rect.center().y - 10.0)),
            Some((rect.width() - pad * 2.0) as f32),
        );
    }

    fn draw_divider(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        y: f64,
        text: &str,
    ) {
        let left = self.modal.x0 + 24.0;
        let right = self.modal.x1 - 24.0;
        let (tw, _) = text_renderer.measure(text, 11.0, None);
        let cx = (left + right) / 2.0;
        let line = theme.tokens.colors.border_standard.with_alpha_factor(0.6);
        let l1 = Rect::new(left, y, cx - tw as f64 / 2.0 - 10.0, y + 1.0);
        let l2 = Rect::new(cx + tw as f64 / 2.0 + 10.0, y, right, y + 1.0);
        scene.fill(Fill::NonZero, transform, line, None, &l1);
        scene.fill(Fill::NonZero, transform, line, None, &l2);
        text_renderer.draw_text(
            scene,
            text,
            11.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((cx - tw as f64 / 2.0, y - 7.0)),
            None,
        );
    }

    fn draw_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        selected: bool,
        hovered: bool,
    ) {
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
        let bg = if selected {
            theme.accent_primary
        } else if hovered {
            theme.tokens.colors.surface_white_10
        } else {
            theme.tokens.colors.surfaces.base
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        if !selected {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_standard,
                None,
                &shape,
            );
        }
        let color = if selected { Color::WHITE } else { theme.tokens.colors.text_primary };
        let (tw, th) = text_renderer.measure_weighted(label, 13.0, None, TextWeight::Medium);
        text_renderer.draw_text_weighted(
            scene,
            label,
            13.0,
            color,
            transform * Affine::translate((rect.center().x - tw as f64 / 2.0, rect.center().y - th as f64 / 2.0)),
            None,
            TextWeight::Medium,
        );
    }

    fn draw_button(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        hovered: bool,
        enabled: bool,
    ) {
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
        let bg = if hovered && enabled {
            theme.bg_hover
        } else {
            theme.tokens.colors.surfaces.base
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &shape,
        );
        let color = if enabled {
            theme.tokens.colors.text_primary
        } else {
            theme.tokens.colors.text_dim
        };
        let (tw, th) = text_renderer.measure_weighted(label, 14.0, None, TextWeight::Medium);
        text_renderer.draw_text_weighted(
            scene,
            label,
            14.0,
            color,
            transform * Affine::translate((rect.center().x - tw as f64 / 2.0, rect.center().y - th as f64 / 2.0)),
            None,
            TextWeight::Medium,
        );
    }

    fn draw_primary_button(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        hovered: bool,
        enabled: bool,
    ) {
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
        let bg = if !enabled {
            theme.accent_primary.with_alpha_factor(0.4)
        } else if hovered {
            theme.accent_hover
        } else {
            theme.accent_primary
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        let (tw, th) = text_renderer.measure_weighted(label, 14.0, None, TextWeight::Semibold);
        text_renderer.draw_text_weighted(
            scene,
            label,
            14.0,
            Color::WHITE,
            transform * Affine::translate((rect.center().x - tw as f64 / 2.0, rect.center().y - th as f64 / 2.0)),
            None,
            TextWeight::Semibold,
        );
    }

    // ---- events ------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if !self.open {
            return;
        }
        // The modal is fully blocking — every event is consumed.
        *handled = true;

        // Pointer moves drive hover state for buttons/chips (via
        // `last_mouse`, read in `draw`) and must reach every text field so
        // their `is_hovered` is set before a click lands — TextInput only
        // places its caret on a press while hovered.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            for field in FIELD_ORDER {
                let mut throwaway = None;
                self.field_mut(field).handle_event(
                    event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy,
                );
            }
            return;
        }

        // Keyboard: Esc cancels, Enter saves, Tab cycles focus.
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
            ..
        } = event
        {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.event = Some(AddPanelEvent::Cancel);
                    self.open = false;
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    self.try_save();
                    return;
                }
                Key::Named(NamedKey::Tab) => {
                    self.focus_next(modifiers.state().shift_key());
                    return;
                }
                _ => {}
            }
        }

        // Mouse press: chips / buttons / focus switching.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            let p = self.last_mouse;
            if self.parse_rect.contains(p) {
                self.parse_uri();
                return;
            }
            if self.cancel_rect.contains(p) {
                self.event = Some(AddPanelEvent::Cancel);
                self.open = false;
                return;
            }
            if self.save_rect.contains(p) {
                self.try_save();
                return;
            }
            for (i, alg) in Algorithm::ALL.iter().enumerate() {
                if self.alg_rects[i].contains(p) {
                    self.algorithm = *alg;
                    return;
                }
            }
            for (i, d) in [6u8, 8u8].iter().enumerate() {
                if self.digit_rects[i].contains(p) {
                    self.digits = *d;
                    return;
                }
            }
            // Focus a text field if clicked.
            for field in FIELD_ORDER {
                let r = self.field_rect(field);
                if r.contains(p) {
                    self.set_focus(Some(field));
                    // Forward so the field positions its caret.
                    self.field_mut(field).handle_event(
                        event, scale_factor, modifiers, state, path, handled, &mut None, node_id, taffy,
                    );
                    return;
                }
            }
            // Click outside any field — clear text focus but keep modal.
            self.set_focus(None);
            return;
        }

        // Forward remaining events (key chars, drag) to the focused field.
        if let Some(f) = self.focused {
            let mut throwaway = None;
            self.field_mut(f).handle_event(
                event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy,
            );
        }
    }

    fn field_rect(&self, f: Field) -> Rect {
        match f {
            Field::Uri => self.uri_rect,
            Field::Issuer => self.issuer_rect,
            Field::Label => self.label_rect,
            Field::Secret => self.secret_rect,
            Field::Period => self.period_rect,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> AddAccountPanel {
        let mut p = AddAccountPanel::new();
        p.open();
        p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        p
    }

    #[test]
    fn parse_uri_fills_fields() {
        let mut p = opened();
        p.uri.set_text("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&algorithm=SHA256&digits=8&period=45");
        p.parse_uri();
        assert_eq!(p.issuer.value(), "Acme");
        assert_eq!(p.label.value(), "bob");
        assert_eq!(p.secret.value(), "JBSWY3DPEHPK3PXP");
        assert_eq!(p.algorithm, Algorithm::Sha256);
        assert_eq!(p.digits, 8);
        assert_eq!(p.period.value(), "45");
    }

    #[test]
    fn build_account_validates_secret() {
        let mut p = opened();
        p.issuer.set_text("Acme");
        p.secret.set_text("not base32 !!");
        assert!(p.build_account().is_err());
        p.secret.set_text("JBSWY3DPEHPK3PXP");
        assert!(p.build_account().is_ok());
    }

    #[test]
    fn save_emits_event_and_closes() {
        let mut p = opened();
        p.issuer.set_text("Acme");
        p.secret.set_text("JBSWY3DPEHPK3PXP");
        p.try_save();
        assert!(!p.is_open());
        assert!(matches!(p.take_event(), Some(AddPanelEvent::Save(_))));
    }
}
