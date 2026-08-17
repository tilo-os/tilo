//! The "Add account" modal — the app's second screen.
//!
//! The flow is built around how people actually add a 2FA account, easiest
//! path first:
//!
//!  1. **Scan a QR image** — the primary, obvious affordance: decode the QR
//!     out of a screenshot or photo file. This works today, fully offline,
//!     via [`crate::qr`]. Choosing the file goes through the host file
//!     chooser (the XDG portal), never an app-drawn picker.
//!  2. **Paste a setup link** — drop an `otpauth://` URI in and every field
//!     auto-fills.
//!
//! Only when neither of those fits does the user reach for **manual entry**:
//! issuer + account label + secret — the three fields almost everyone
//! needs. The fiddly cryptographic parameters (algorithm, digit count,
//! rotation period) live behind a collapsed **Advanced options** disclosure
//! so the default screen reads as calm, not as a form dump.
//!
//! The panel is driven imperatively by [`crate::app::AuthenticatorApp`]
//! (laid out by hand, not via taffy) because it is a transient overlay. It
//! owns its [`ui::widget::TextInput`] fields in local mode and manages focus
//! among them itself, forwarding key/pointer events to the focused field.
//! Results surface as an [`AddPanelEvent`] drained by the app.

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use uuid::Uuid;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use vello::Scene;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
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
    /// The user asked to import a QR code from an image file. The app routes
    /// this to the **host** file chooser (the XDG Desktop Portal — the app
    /// never draws its own picker, HIG §5), then runs the decode and calls
    /// [`AddAccountPanel::apply_uri`] with the result.
    PickImageForQr,
}

/// Which text field currently has the caret. Ordered for Tab cycling; the
/// advanced fields only participate while the section is expanded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Uri,
    Issuer,
    Label,
    Secret,
    Period,
}

const MODAL_W: f64 = 460.0;
const INPUT_H: f64 = 42.0;
const PAD: f64 = 28.0;

pub struct AddAccountPanel {
    open: bool,
    window: Rect,
    modal: Rect,
    /// Vertical scroll of the body region between the fixed title and footer,
    /// so the form is reachable even in a short window.
    scroll: f64,
    max_scroll: f64,
    /// Set when the Advanced section is revealed so the next layout scrolls
    /// its newly-shown fields into view rather than leaving them below the
    /// fold. Consumed (cleared) by [`layout`].
    reveal_advanced: bool,
    /// Clip rect for the scrolling body (fixed title/footer draw outside it).
    body_clip: Rect,

    // Easy-path entry.
    uri: TextInput,
    /// The paste-a-link row is itself disclosed: collapsed to a single
    /// tappable method until the user chooses it.
    uri_expanded: bool,

    // Manual fields (the three most accounts need).
    issuer: TextInput,
    label: TextInput,
    secret: TextInput,

    // Advanced parameters, hidden until disclosed.
    advanced_open: bool,
    algorithm: Algorithm,
    digits: u8,
    period: TextInput,

    focused: Option<Field>,
    last_mouse: Point,
    error: String,
    event: Option<AddPanelEvent>,

    // Laid-out hit rects, recomputed in `layout`.
    image_card_rect: Rect,
    link_card_rect: Rect,
    uri_rect: Rect,
    parse_rect: Rect,
    issuer_rect: Rect,
    label_rect: Rect,
    secret_rect: Rect,
    advanced_header_rect: Rect,
    alg_rects: [Rect; 3],
    digit_rects: [Rect; 2],
    period_rect: Rect,
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
            scroll: 0.0,
            max_scroll: 0.0,
            reveal_advanced: false,
            body_clip: Rect::ZERO,
            uri: TextInput::local(),
            uri_expanded: false,
            issuer: TextInput::local(),
            label: TextInput::local(),
            secret: TextInput::local(),
            advanced_open: false,
            algorithm: Algorithm::Sha1,
            digits: 6,
            period: TextInput::local().with_initial_value("30"),
            focused: None,
            last_mouse: Point::ZERO,
            error: String::new(),
            event: None,
            image_card_rect: Rect::ZERO,
            link_card_rect: Rect::ZERO,
            uri_rect: Rect::ZERO,
            parse_rect: Rect::ZERO,
            issuer_rect: Rect::ZERO,
            label_rect: Rect::ZERO,
            secret_rect: Rect::ZERO,
            advanced_header_rect: Rect::ZERO,
            alg_rects: [Rect::ZERO; 3],
            digit_rects: [Rect::ZERO; 2],
            period_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
            save_rect: Rect::ZERO,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the panel, resetting every field to defaults and collapsing the
    /// disclosures so each visit starts from the calm default state.
    pub fn open(&mut self) {
        self.open = true;
        self.error.clear();
        self.scroll = 0.0;
        self.reveal_advanced = false;
        self.uri.set_text("");
        self.uri_expanded = false;
        self.issuer.set_text("");
        self.label.set_text("");
        self.secret.set_text("");
        self.advanced_open = false;
        self.algorithm = Algorithm::Sha1;
        self.digits = 6;
        self.period.set_text("30");
        self.set_focus(None);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.set_focus(None);
    }

    /// Open with the Advanced-options section already expanded. Used by the
    /// `--shots` renderer to capture the tall, scrolling form; runtime opens
    /// via [`open`] with Advanced collapsed.
    pub fn open_with_advanced(&mut self) {
        self.open();
        self.advanced_open = true;
        self.reveal_advanced = true;
    }

    pub fn take_event(&mut self) -> Option<AddPanelEvent> {
        self.event.take()
    }

    /// Order text fields participate in Tab cycling, given the current
    /// disclosure state. The URI field joins only when the paste row is
    /// expanded; the period field only when Advanced is open.
    fn field_order(&self) -> Vec<Field> {
        let mut order = Vec::with_capacity(5);
        if self.uri_expanded {
            order.push(Field::Uri);
        }
        order.push(Field::Issuer);
        order.push(Field::Label);
        order.push(Field::Secret);
        if self.advanced_open {
            order.push(Field::Period);
        }
        order
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
        for field in [Field::Uri, Field::Issuer, Field::Label, Field::Secret, Field::Period] {
            let on = Some(field) == f;
            self.field_mut(field).set_focus(on);
        }
        self.focused = f;
    }

    fn focus_next(&mut self, backward: bool) {
        let order = self.field_order();
        if order.is_empty() {
            return;
        }
        let cur = self
            .focused
            .and_then(|f| order.iter().position(|x| *x == f))
            .unwrap_or(0);
        let n = order.len();
        let next = if backward { (cur + n - 1) % n } else { (cur + 1) % n };
        self.set_focus(Some(order[next]));
    }

    /// Reveal the paste-a-link row and focus its field.
    fn expand_uri(&mut self) {
        self.uri_expanded = true;
        self.error.clear();
        self.set_focus(Some(Field::Uri));
    }

    fn toggle_advanced(&mut self) {
        self.advanced_open = !self.advanced_open;
        if self.advanced_open {
            // Reveal the newly-shown Algorithm/Digits/Period fields on the
            // next layout instead of leaving them below the scroll fold.
            self.reveal_advanced = true;
        } else if self.focused == Some(Field::Period) {
            self.set_focus(None);
        }
    }

    // ---- layout ------------------------------------------------------

    /// Recompute the modal + every field rect for the given window.
    ///
    /// The modal grows to fit its content up to the available height; beyond
    /// that the body between the fixed title and footer scrolls, so the form
    /// is fully reachable even in a short window. Body rects are laid out
    /// already offset by the current scroll so hit-testing and drawing share
    /// one geometry.
    pub fn layout(&mut self, window: Rect) {
        self.window = window;
        let w = MODAL_W.min(window.width() - 48.0);
        let left_pad = PAD;
        let content_w = w - left_pad * 2.0;

        let card_h = 56.0;
        let card_gap = 10.0;
        let label_gap = 18.0;
        let row_gap = 14.0;
        let adv_header_h = 26.0;
        // The fixed title block holds the "Add account" heading and its
        // subtitle; the scrolling body starts below it.
        let title_h = 74.0;
        let footer_h = 44.0;
        let footer_gap = 18.0;

        // Natural body height (title/footer excluded). Mirrors the running-y
        // accumulation below exactly so the scroll range is precise.
        let mut body_natural = card_h * 2.0 + card_gap; // two method cards
        if self.uri_expanded {
            body_natural += 18.0 + INPUT_H + 12.0; // gap + uri row
        }
        body_natural += 30.0; // "or enter details" divider
        body_natural += (label_gap + INPUT_H + row_gap) * 3.0; // issuer/label/secret
        body_natural += adv_header_h;
        if self.advanced_open {
            body_natural += 14.0 + 18.0 + INPUT_H + 8.0; // through the algorithm row
            body_natural += 18.0 + INPUT_H; // digits + period row
        }

        let natural_h = title_h + body_natural + footer_gap + footer_h + PAD;
        let avail_h = (window.height() - 32.0).max(320.0);
        let h = natural_h.min(avail_h).max(320.0);

        let x0 = window.center().x - w / 2.0;
        let y0 = (window.center().y - h / 2.0).max(window.y0 + 16.0);
        self.modal = Rect::new(x0, y0, x0 + w, y0 + h);

        let left = x0 + left_pad;
        let right = x0 + w - left_pad;

        // Footer pinned to the bottom of the card (outside the scroll region).
        let btn_w = ((content_w - 12.0) / 2.0).min(140.0);
        let by = y0 + h - PAD - footer_h;
        self.save_rect = Rect::new(right - btn_w, by, right, by + footer_h);
        self.cancel_rect = Rect::new(right - btn_w * 2.0 - 12.0, by, right - btn_w - 12.0, by + footer_h);

        // Scrollable body between the fixed title and the fixed footer.
        let body_top = y0 + title_h;
        let body_bottom = by - footer_gap;
        let visible_body = (body_bottom - body_top).max(0.0);
        self.max_scroll = (body_natural - visible_body).max(0.0);
        // When Advanced was just revealed, scroll so its fields (the
        // bottom-most body content) are on screen rather than below the
        // fold. Consumed here so later resizes keep the user's scroll.
        if self.reveal_advanced {
            self.scroll = self.max_scroll;
            self.reveal_advanced = false;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);
        self.body_clip = Rect::new(x0, body_top, x0 + w, body_bottom);

        // Lay out body rects top-down, already offset by the scroll position.
        let mut y = body_top - self.scroll;

        self.image_card_rect = Rect::new(left, y, right, y + card_h);
        y += card_h + card_gap;
        self.link_card_rect = Rect::new(left, y, right, y + card_h);
        y += card_h;

        if self.uri_expanded {
            y += 18.0;
            let parse_w = 84.0;
            self.uri_rect = Rect::new(left, y, right - parse_w - 8.0, y + INPUT_H);
            self.parse_rect = Rect::new(right - parse_w, y, right, y + INPUT_H);
            y += INPUT_H + 12.0;
        } else {
            self.uri_rect = Rect::ZERO;
            self.parse_rect = Rect::ZERO;
        }

        // "Or enter details" divider occupies 30px.
        y += 30.0;

        self.issuer_rect = Rect::new(left, y + label_gap, right, y + label_gap + INPUT_H);
        y += label_gap + INPUT_H + row_gap;
        self.label_rect = Rect::new(left, y + label_gap, right, y + label_gap + INPUT_H);
        y += label_gap + INPUT_H + row_gap;
        self.secret_rect = Rect::new(left, y + label_gap, right, y + label_gap + INPUT_H);
        y += label_gap + INPUT_H + row_gap;

        self.advanced_header_rect = Rect::new(left, y, right, y + adv_header_h);
        y += adv_header_h;

        if self.advanced_open {
            y += 14.0;
            let chip_y = y + 18.0;
            let alg_w = (content_w - 12.0) / 3.0;
            for i in 0..3 {
                let cx = left + i as f64 * (alg_w + 6.0);
                self.alg_rects[i] = Rect::new(cx, chip_y, cx + alg_w, chip_y + INPUT_H);
            }
            y = chip_y + INPUT_H + 8.0;
            let row_y = y + 18.0;
            let dig_w = 56.0;
            for i in 0..2 {
                let cx = left + i as f64 * (dig_w + 6.0);
                self.digit_rects[i] = Rect::new(cx, row_y, cx + dig_w, row_y + INPUT_H);
            }
            let period_w = 88.0;
            let period_x = right - period_w;
            self.period_rect = Rect::new(period_x, row_y, right, row_y + INPUT_H);
        } else {
            self.alg_rects = [Rect::ZERO; 3];
            self.digit_rects = [Rect::ZERO; 2];
            self.period_rect = Rect::ZERO;
        }

        // Push rects into the text inputs.
        self.uri.apply_layout_external(self.uri_rect);
        self.issuer.apply_layout_external(self.issuer_rect);
        self.label.apply_layout_external(self.label_rect);
        self.secret.apply_layout_external(self.secret_rect);
        self.period.apply_layout_external(self.period_rect);
    }

    // ---- account building / actions ----------------------------------

    /// Build an account from the current field values, or an error string
    /// describing why it can't be saved.
    fn build_account(&self) -> Result<Account, String> {
        let secret = model::normalise_secret(self.secret.value());
        if secret.is_empty() {
            return Err("Enter a secret key".to_string());
        }
        if crate::totp::decode_secret(&secret).map(|b| b.is_empty()).unwrap_or(true) {
            return Err("That secret isn\u{2019}t valid base32".to_string());
        }
        let issuer = self.issuer.value().trim().to_string();
        let label = self.label.value().trim().to_string();
        if issuer.is_empty() && label.is_empty() {
            return Err("Add an issuer or account name".to_string());
        }
        let period = self.period.value().trim().parse::<u32>().unwrap_or(30).clamp(5, 300);
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

    /// Apply a decoded `otpauth://` URI (from paste or QR image import) into
    /// the manual fields. On success the manual
    /// section shows the filled values so the user can review before saving;
    /// on failure the reason surfaces inline.
    pub fn apply_uri(&mut self, uri: &str) {
        match model::parse_otpauth_uri(uri, Uuid::new_v4()) {
            Ok(acc) => {
                self.issuer.set_text(&acc.issuer);
                self.label.set_text(&acc.label);
                self.secret.set_text(&acc.secret);
                self.period.set_text(&acc.period.to_string());
                self.algorithm = acc.algorithm;
                self.digits = acc.digits;
                // Surface non-default crypto params so a reviewing user can
                // see what the code carried.
                if acc.algorithm != Algorithm::Sha1 || acc.digits != 6 || acc.period != 30 {
                    self.advanced_open = true;
                }
                self.error.clear();
                self.set_focus(Some(Field::Issuer));
            }
            Err(e) => self.error = e,
        }
    }

    /// Parse whatever is currently in the URI field.
    fn parse_uri_field(&mut self) {
        let uri = self.uri.value().to_string();
        if uri.trim().is_empty() {
            self.error = "Paste an otpauth:// link first".to_string();
            return;
        }
        self.apply_uri(&uri);
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
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &card,
        );

        let left = self.modal.x0 + PAD;
        // Title + calm subtitle.
        text_renderer.draw_text_weighted(
            scene,
            "Add account",
            22.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((left, self.modal.y0 + 24.0)),
            None,
            TextWeight::Bold,
        );
        text_renderer.draw_text(
            scene,
            "Scan an image, paste a link, or type the details",
            13.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((left, self.modal.y0 + 50.0)),
            None,
        );

        // Everything between the fixed title and footer scrolls; clip it so
        // scrolled rows never bleed into the chrome.
        let clip = RoundedRect::from_rect(self.body_clip, 0.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);

        // --- Method cards. The image importer leads (it is the real QR
        // path today); pasting a setup link is the calm secondary. ---
        let img_hover = self.image_card_rect.contains(self.last_mouse);
        self.draw_method_card(
            scene, transform, text_renderer, theme, self.image_card_rect,
            Glyph::Image, "Scan a QR image", "Read the code from a screenshot or photo", img_hover, true,
        );
        let link_hover = self.link_card_rect.contains(self.last_mouse);
        self.draw_method_card(
            scene, transform, text_renderer, theme, self.link_card_rect,
            Glyph::Link, "Paste a setup link", "Fill everything from an otpauth:// link", link_hover, false,
        );

        // Paste-link field (disclosed).
        if self.uri_expanded {
            if self.uri.value().is_empty() {
                self.draw_placeholder(scene, transform, text_renderer, theme, self.uri_rect, "otpauth://totp/Issuer:account?secret=\u{2026}");
            }
            self.uri.draw(scene, transform, text_renderer, theme, &presence);
            let parse_hover = self.parse_rect.contains(self.last_mouse);
            self.draw_button(scene, transform, text_renderer, theme, self.parse_rect, "Fill", parse_hover, true);
        }

        // "Or enter details" divider.
        let div_y = self.issuer_rect.y0 - 18.0 - 14.0;
        self.draw_divider(scene, transform, text_renderer, theme, div_y, "or enter details");

        // Manual fields.
        self.draw_text_field(scene, transform, text_renderer, theme, "Issuer", Field::Issuer, "GitHub");
        self.draw_text_field(scene, transform, text_renderer, theme, "Account name", Field::Label, "ada@example.com");
        self.draw_text_field(scene, transform, text_renderer, theme, "Secret key", Field::Secret, "JBSWY3DPEHPK3PXP");

        // --- Advanced options disclosure ---
        self.draw_advanced_header(scene, transform, text_renderer, theme);
        if self.advanced_open {
            self.draw_field_label(scene, transform, text_renderer, theme, "Algorithm", self.alg_rects[0]);
            for (i, alg) in Algorithm::ALL.iter().enumerate() {
                let selected = *alg == self.algorithm;
                let hovered = self.alg_rects[i].contains(self.last_mouse);
                self.draw_chip(scene, transform, text_renderer, theme, self.alg_rects[i], alg.as_str(), selected, hovered);
            }
            self.draw_field_label(scene, transform, text_renderer, theme, "Digits", self.digit_rects[0]);
            for (i, d) in [6u8, 8u8].iter().enumerate() {
                let selected = *d == self.digits;
                let hovered = self.digit_rects[i].contains(self.last_mouse);
                self.draw_chip(scene, transform, text_renderer, theme, self.digit_rects[i], &d.to_string(), selected, hovered);
            }
            self.draw_field_label(scene, transform, text_renderer, theme, "Period (s)", self.period_rect);
            self.period.draw(scene, transform, text_renderer, theme, &presence);
        }

        scene.pop_layer();

        // A hint that there is more below when the body is scrollable.
        if self.max_scroll > 0.5 && self.scroll < self.max_scroll - 0.5 {
            self.draw_scroll_hint(scene, transform, theme);
        }

        // Error line, just above the footer.
        if !self.error.is_empty() {
            text_renderer.draw_text(
                scene,
                &self.error,
                12.0,
                theme.tokens.colors.semantic.danger,
                transform * Affine::translate((left, self.cancel_rect.y0 - 20.0)),
                Some((self.modal.width() - PAD * 2.0) as f32),
            );
        }

        // Footer.
        let can_save = !self.secret.value().trim().is_empty();
        let cancel_hover = self.cancel_rect.contains(self.last_mouse);
        let save_hover = self.save_rect.contains(self.last_mouse);
        self.draw_button(scene, transform, text_renderer, theme, self.cancel_rect, "Cancel", cancel_hover, true);
        self.draw_primary_button(scene, transform, text_renderer, theme, self.save_rect, "Add account", save_hover, can_save);
    }

    /// One big, tappable method row: an icon tile, a bold title, and a calm
    /// one-line description. The primary card (QR) gets the accent tint so
    /// the recommended path reads first.
    #[allow(clippy::too_many_arguments)]
    fn draw_method_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        glyph: Glyph,
        title: &str,
        subtitle: &str,
        hovered: bool,
        primary: bool,
    ) {
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.lg as f64);
        let bg = if primary {
            if hovered { theme.accent_hover } else { theme.accent_primary }
        } else if hovered {
            theme.tokens.colors.surface_white_10
        } else {
            theme.tokens.colors.surfaces.base
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        if !primary {
            scene.stroke(&Stroke::new(1.0), transform, theme.tokens.colors.border_standard, None, &shape);
        }

        let on_primary = primary;
        let title_color = if on_primary { Color::WHITE } else { theme.tokens.colors.text_primary };
        let sub_color = if on_primary {
            Color::WHITE.with_alpha_factor(0.82)
        } else {
            theme.tokens.colors.text_dim
        };
        let icon_color = if on_primary { Color::WHITE } else { theme.accent_primary };

        // Icon tile on the left.
        let tile = Rect::new(rect.x0 + 14.0, rect.center().y - 16.0, rect.x0 + 14.0 + 32.0, rect.center().y + 16.0);
        let tile_bg = if on_primary {
            Color::WHITE.with_alpha_factor(0.18)
        } else {
            theme.accent_primary.with_alpha_factor(0.14)
        };
        scene.fill(Fill::NonZero, transform, tile_bg, None, &RoundedRect::from_rect(tile, theme.tokens.radii.md as f64));
        draw_glyph(scene, transform, tile, glyph, icon_color);

        let text_x = tile.x1 + 14.0;
        text_renderer.draw_text_weighted(
            scene,
            title,
            15.0,
            title_color,
            transform * Affine::translate((text_x, rect.center().y - 16.0)),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            subtitle,
            12.0,
            sub_color,
            transform * Affine::translate((text_x, rect.center().y + 3.0)),
            Some((rect.x1 - text_x - 28.0) as f32),
        );

        // Chevron affordance on the right.
        let cx = rect.x1 - 22.0;
        let cy = rect.center().y;
        let mut chev = BezPath::new();
        chev.move_to((cx - 4.0, cy - 6.0));
        chev.line_to((cx + 4.0, cy));
        chev.line_to((cx - 4.0, cy + 6.0));
        let chev_color = if on_primary { Color::WHITE.with_alpha_factor(0.8) } else { theme.tokens.colors.text_dim };
        scene.stroke(&Stroke::new(2.0), transform, chev_color, None, &chev);
    }

    /// A subtle downward chevron at the bottom of the scroll body, shown
    /// only while there is more content below the fold.
    fn draw_scroll_hint(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let cx = self.body_clip.center().x;
        let cy = self.body_clip.y1 - 7.0;
        let mut chev = BezPath::new();
        chev.move_to((cx - 6.0, cy - 3.0));
        chev.line_to((cx, cy + 3.0));
        chev.line_to((cx + 6.0, cy - 3.0));
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.tokens.colors.text_dim.with_alpha_factor(0.8),
            None,
            &chev,
        );
    }

    fn draw_advanced_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let r = self.advanced_header_rect;
        let hovered = r.contains(self.last_mouse);
        // Disclosure triangle.
        let tx = r.x0;
        let ty = r.center().y;
        let mut tri = BezPath::new();
        if self.advanced_open {
            tri.move_to((tx, ty - 4.0));
            tri.line_to((tx + 10.0, ty - 4.0));
            tri.line_to((tx + 5.0, ty + 4.0));
        } else {
            tri.move_to((tx + 1.0, ty - 5.0));
            tri.line_to((tx + 9.0, ty));
            tri.line_to((tx + 1.0, ty + 5.0));
        }
        tri.close_path();
        let color = if hovered { theme.tokens.colors.text_primary } else { theme.tokens.colors.text_dim };
        scene.fill(Fill::NonZero, transform, color, None, &tri);
        text_renderer.draw_text_weighted(
            scene,
            "Advanced options",
            13.0,
            color,
            transform * Affine::translate((r.x0 + 18.0, r.center().y - 8.0)),
            None,
            TextWeight::Medium,
        );
    }

    fn draw_field_label(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        label: &str,
        field_rect: Rect,
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
        self.draw_field_label(scene, transform, text_renderer, theme, label, rect);
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
        let left = self.modal.x0 + PAD;
        let right = self.modal.x1 - PAD;
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
            scene.stroke(&Stroke::new(1.0), transform, theme.tokens.colors.border_standard, None, &shape);
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
        let bg = if hovered && enabled { theme.bg_hover } else { theme.tokens.colors.surfaces.base };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        scene.stroke(&Stroke::new(1.0), transform, theme.tokens.colors.border_standard, None, &shape);
        let color = if enabled { theme.tokens.colors.text_primary } else { theme.tokens.colors.text_dim };
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

        // Pointer moves drive hover state (read in `draw`) and must reach
        // every text field so their `is_hovered` is set before a click lands.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            for field in [Field::Uri, Field::Issuer, Field::Label, Field::Secret, Field::Period] {
                let mut throwaway = None;
                self.field_mut(field).handle_event(
                    event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy,
                );
            }
            return;
        }

        // Wheel scrolls the body when the form is taller than the window.
        // The next layout pass re-offsets every rect from the new scroll.
        if let WindowEvent::MouseWheel { delta, .. } = event {
            if self.max_scroll > 0.0 {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 40.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll);
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

        // Mouse press: cards / disclosures / chips / buttons / focus.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            let p = self.last_mouse;

            // Method cards.
            if self.image_card_rect.contains(p) {
                // The app routes to the host portal chooser + runs the decode.
                self.event = Some(AddPanelEvent::PickImageForQr);
                return;
            }
            if self.link_card_rect.contains(p) {
                self.expand_uri();
                return;
            }
            if self.uri_expanded && self.parse_rect.contains(p) {
                self.parse_uri_field();
                return;
            }
            // Advanced disclosure header.
            if self.advanced_header_rect.contains(p) {
                self.toggle_advanced();
                return;
            }
            // Footer.
            if self.cancel_rect.contains(p) {
                self.event = Some(AddPanelEvent::Cancel);
                self.open = false;
                return;
            }
            if self.save_rect.contains(p) {
                self.try_save();
                return;
            }
            // Advanced chips (only hittable when open — rects are ZERO otherwise).
            if self.advanced_open {
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
            }
            // Focus a text field if clicked.
            for field in self.field_order() {
                let r = self.field_rect(field);
                if r.contains(p) {
                    self.set_focus(Some(field));
                    self.field_mut(field).handle_event(
                        event, scale_factor, modifiers, state, path, handled, &mut None, node_id, taffy,
                    );
                    return;
                }
            }
            // Click elsewhere — drop text focus, keep modal open.
            self.set_focus(None);
            return;
        }

        // Forward remaining events (chars, drag) to the focused field.
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

    /// Test hook: run the exact production save path (validate → emit event).
    #[cfg(test)]
    pub(crate) fn save_for_test(&mut self) {
        self.try_save();
    }

    /// Test hook: current (issuer, label, secret) field values.
    #[cfg(test)]
    pub(crate) fn field_snapshot(&self) -> (String, String, String) {
        (
            self.issuer.value().to_string(),
            self.label.value().to_string(),
            self.secret.value().to_string(),
        )
    }
}

/// The small set of glyphs the method cards draw, rendered as vector paths
/// (no font/icon dependency) so they track the accent colour the theme
/// hands us.
#[derive(Clone, Copy)]
enum Glyph {
    Image,
    Link,
}

/// Draw a method-card glyph centred in `tile`, in `color`.
///
/// The glyph comes from the shared [`ui::icons::Icon`] set so the method
/// cards match the rest of the OS (uniform stroke, grid and theming).
fn draw_glyph(scene: &mut Scene, transform: Affine, tile: Rect, glyph: Glyph, color: Color) {
    let icon = match glyph {
        Glyph::Image => ui::icons::Icon::Image,
        Glyph::Link => ui::icons::Icon::Link,
    };
    // Keep the ~20px visual size the cards were designed around.
    let side = 24.0_f64.min(tile.width()).min(tile.height());
    let r = Rect::from_center_size(tile.center(), (side, side));
    icon.draw_with_stroke(scene, transform, r, color, 1.6);
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
    fn advanced_is_collapsed_by_default() {
        let p = opened();
        assert!(!p.advanced_open, "advanced options must start hidden");
        // Advanced rects are not laid out while collapsed.
        assert_eq!(p.period_rect, Rect::ZERO);
        assert_eq!(p.alg_rects[0], Rect::ZERO);
    }

    #[test]
    fn paste_link_is_collapsed_until_chosen() {
        let mut p = opened();
        assert!(!p.uri_expanded);
        assert_eq!(p.uri_rect, Rect::ZERO);
        p.expand_uri();
        p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        assert!(p.uri_expanded);
        assert_ne!(p.uri_rect, Rect::ZERO, "uri field lays out once disclosed");
    }

    #[test]
    fn toggling_advanced_lays_out_the_fields() {
        let mut p = opened();
        p.toggle_advanced();
        p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        assert!(p.advanced_open);
        assert_ne!(p.period_rect, Rect::ZERO);
        assert_ne!(p.alg_rects[0], Rect::ZERO);
    }

    #[test]
    fn default_form_fits_a_tall_window_without_scrolling() {
        let mut p = AddAccountPanel::new();
        p.open();
        p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        assert_eq!(p.max_scroll, 0.0, "the calm default form fits without scrolling");
        assert!(p.save_rect.y1 <= p.modal.y1, "footer stays inside the modal");
        assert!(p.modal.y1 <= 800.0 - 16.0, "modal fits inside the window");
    }

    #[test]
    fn tall_form_scrolls_in_a_short_window_without_overflowing() {
        let win = Rect::new(0.0, 0.0, 800.0, 600.0);
        let mut p = AddAccountPanel::new();
        p.open();
        p.toggle_advanced(); // makes the form taller than a 600px window
        p.layout(win);
        assert!(p.max_scroll > 0.0, "advanced form must scroll in a short window");
        // The footer is pinned inside the modal, never off-screen.
        assert!(p.save_rect.y1 <= p.modal.y1);
        assert!(p.modal.y1 <= 600.0);
        // Reset to the top of the body (revealing Advanced auto-scrolled to
        // the bottom), so we can prove the scroll mechanic independently.
        p.scroll = 0.0;
        p.layout(win);
        let period_top_before = p.period_rect.y0;
        // Scrolling to the bottom brings the period field up into the body.
        p.scroll = p.max_scroll;
        p.layout(win);
        assert!(
            p.period_rect.y0 < period_top_before,
            "scrolling moves body content upward into view"
        );
        // Fully scrolled, the last field sits within the visible body band.
        assert!(p.period_rect.y1 <= p.body_clip.y1 + 1.0, "bottom of form reachable");
    }

    #[test]
    fn modal_reflows_from_the_current_window_on_every_layout() {
        // Guards against caching the size from the first frame: the same
        // instance must re-derive width + scroll when the window changes.
        let mut p = AddAccountPanel::new();
        p.open();
        p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        let wide_w = p.modal.width();
        assert_eq!(p.max_scroll, 0.0, "roomy window needs no scroll");
        p.layout(Rect::new(0.0, 0.0, 400.0, 420.0));
        assert!(p.modal.width() < wide_w, "modal narrows to the smaller window");
        assert!(p.max_scroll > 0.0, "a short window enables scrolling");
        // And back to roomy — it must recover, not stay narrow.
        p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        assert!((p.modal.width() - wide_w).abs() < 0.5, "modal widens back");
        assert_eq!(p.max_scroll, 0.0);
    }

    #[test]
    fn revealing_advanced_scrolls_its_fields_into_view() {
        // In a short window the expanded Advanced section is taller than the
        // fold; toggling it open must auto-scroll so the last field lands
        // inside the visible body band instead of below it.
        let win = Rect::new(0.0, 0.0, 800.0, 600.0);
        let mut p = AddAccountPanel::new();
        p.open();
        p.layout(win);
        p.toggle_advanced();
        p.layout(win);
        assert!(p.advanced_open);
        assert!(p.max_scroll > 0.0, "advanced form overflows a 600px window");
        assert_eq!(p.scroll, p.max_scroll, "toggle scrolls the new fields into view");
        assert_ne!(p.period_rect, Rect::ZERO, "period field is laid out");
        assert!(
            p.period_rect.y1 <= p.body_clip.y1 + 1.0,
            "the last advanced field is within the visible body, not below the fold"
        );
        assert!(
            p.period_rect.y0 >= p.body_clip.y0 - 1.0,
            "the period field is not clipped above the body top"
        );
    }

    #[test]
    fn revealing_advanced_is_a_one_shot_that_survives_resize() {
        // After the reveal-scroll, a later relayout (e.g. a resize) must not
        // re-force the scroll — the user keeps control of their position.
        let win = Rect::new(0.0, 0.0, 800.0, 600.0);
        let mut p = AddAccountPanel::new();
        p.open();
        p.toggle_advanced();
        p.layout(win);
        assert!(!p.reveal_advanced, "the reveal flag is consumed by layout");
        // Scroll back up, then relayout: the position is preserved.
        p.scroll = 0.0;
        p.layout(win);
        assert_eq!(p.scroll, 0.0, "a plain relayout does not re-trigger the reveal");
    }

    #[test]
    fn apply_uri_fills_fields_and_reveals_nondefault_params() {
        let mut p = opened();
        p.apply_uri("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&algorithm=SHA256&digits=8&period=45");
        assert_eq!(p.issuer.value(), "Acme");
        assert_eq!(p.label.value(), "bob");
        assert_eq!(p.secret.value(), "JBSWY3DPEHPK3PXP");
        assert_eq!(p.algorithm, Algorithm::Sha256);
        assert_eq!(p.digits, 8);
        assert_eq!(p.period.value(), "45");
        // Non-default crypto params should auto-reveal Advanced for review.
        assert!(p.advanced_open, "non-default params reveal advanced");
    }

    #[test]
    fn apply_uri_keeps_advanced_collapsed_for_plain_accounts() {
        let mut p = opened();
        p.apply_uri("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP");
        assert!(!p.advanced_open, "spec-default account stays calm");
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

    #[test]
    fn field_order_tracks_disclosure_state() {
        let mut p = opened();
        // Collapsed: just the three manual fields.
        assert_eq!(p.field_order(), vec![Field::Issuer, Field::Label, Field::Secret]);
        p.expand_uri();
        p.toggle_advanced();
        assert_eq!(
            p.field_order(),
            vec![Field::Uri, Field::Issuer, Field::Label, Field::Secret, Field::Period]
        );
    }

    // ---- pointer clicks through the real `handle_event` -----------------
    // The method cards, disclosure, chips and footer buttons are only
    // reachable through `handle_event`; these drive real synthetic pointer
    // events so each control's effect is verified end to end, not by calling
    // the internal handlers directly.

    use crate::test_events::{cursor_at, left_press};

    fn dispatch(p: &mut AddAccountPanel, ev: &WindowEvent) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = crate::test_events::no_mods();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut handled = false;
        p.handle_event(ev, 1.0, &mods, &mut doc, &root, &mut handled, node, &taffy);
    }

    #[test]
    fn clicking_the_scan_qr_card_emits_pick_image() {
        let mut p = opened();
        let at = p.image_card_rect.center();
        dispatch(&mut p, &cursor_at(at));
        dispatch(&mut p, &left_press());
        assert!(
            matches!(p.take_event(), Some(AddPanelEvent::PickImageForQr)),
            "the QR card routes to the host image picker"
        );
    }

    #[test]
    fn clicking_the_paste_link_card_discloses_the_uri_field() {
        let mut p = opened();
        assert!(!p.uri_expanded);
        let at = p.link_card_rect.center();
        dispatch(&mut p, &cursor_at(at));
        dispatch(&mut p, &left_press());
        assert!(p.uri_expanded, "the paste-link card discloses the uri row");
        assert_eq!(p.focused, Some(Field::Uri), "and focuses it");
    }

    #[test]
    fn clicking_algorithm_and_digit_chips_selects_them() {
        let mut p = opened();
        p.toggle_advanced();
        p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        assert_eq!(p.algorithm, Algorithm::Sha1, "defaults to SHA1");
        assert_eq!(p.digits, 6, "defaults to 6 digits");
        // Click the SHA512 chip (index 2).
        let alg = p.alg_rects[2].center();
        dispatch(&mut p, &cursor_at(alg));
        dispatch(&mut p, &left_press());
        assert_eq!(p.algorithm, Algorithm::Sha512, "clicking a chip selects the algorithm");
        // Click the 8-digit chip (index 1).
        let dig = p.digit_rects[1].center();
        dispatch(&mut p, &cursor_at(dig));
        dispatch(&mut p, &left_press());
        assert_eq!(p.digits, 8, "clicking a chip selects the digit count");
    }

    #[test]
    fn clicking_the_fill_button_parses_the_uri_field() {
        let mut p = opened();
        p.expand_uri();
        p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        p.uri.set_text("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP");
        let at = p.parse_rect.center();
        dispatch(&mut p, &cursor_at(at));
        dispatch(&mut p, &left_press());
        assert_eq!(p.issuer.value(), "Acme", "Fill parses the issuer out of the link");
        assert_eq!(p.secret.value(), "JBSWY3DPEHPK3PXP", "Fill parses the secret");
    }

    #[test]
    fn clicking_cancel_emits_cancel_and_closes() {
        let mut p = opened();
        let at = p.cancel_rect.center();
        dispatch(&mut p, &cursor_at(at));
        dispatch(&mut p, &left_press());
        assert!(!p.is_open(), "Cancel closes the modal");
        assert!(matches!(p.take_event(), Some(AddPanelEvent::Cancel)));
    }

    #[test]
    fn clicking_save_with_valid_fields_emits_save() {
        let mut p = opened();
        p.issuer.set_text("Acme");
        p.secret.set_text("JBSWY3DPEHPK3PXP");
        let at = p.save_rect.center();
        dispatch(&mut p, &cursor_at(at));
        dispatch(&mut p, &left_press());
        assert!(!p.is_open(), "a valid Save closes the modal");
        assert!(matches!(p.take_event(), Some(AddPanelEvent::Save(_))));
    }
}
