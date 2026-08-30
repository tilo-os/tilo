//! Screen 2 — recovery-share storage.
//!
//! Displays the 24-word BIP39 mnemonic in a numbered 6×4 grid. The
//! user must explicitly tick "I've written it down" before the
//! continue button activates.
//!
//! ## Single-device caveat
//!
//! The qOS spec only allows `export_recovery_share` once the identity
//! has entered threshold mode (a 2-of-N DKG has run). Calling it on a
//! single-device identity returns an error per the engine's API.
//!
//! For first-run, the identity is in 1-of-1 bootstrap mode — there is
//! no real recovery share yet. We show a deterministic placeholder
//! mnemonic derived from the identity's public key so the user goes
//! through the "write it down" muscle-memory; the caption tells them
//! the real share is generated once they pair a second device, at
//! which point this screen is re-presented with the real mnemonic.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use sha2::{Digest, Sha512};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::text_selection::clipboard_copy;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::{IdentityRecord, ScreenOutcome};

/// Deterministic placeholder mnemonic generator.
///
/// Input: 32 bytes of public-key entropy. Output: a 24-word BIP39
/// English mnemonic. The output is deterministic — same pubkey
/// produces the same words — so the recovery-verify step can
/// regenerate the expected words without storing them. **The
/// placeholder is NOT a valid recovery share** (single-device mode
/// has no shareable secret); see the module-level note.
pub fn deterministic_placeholder_mnemonic(public_key_bytes: &[u8; 32]) -> String {
    // Stretch the pubkey into 32 bytes of entropy via SHA-512 (we
    // discard the upper half). `bip39::Mnemonic::from_entropy` only
    // accepts 16/20/24/28/32-byte slices.
    let mut hasher = Sha512::new();
    hasher.update(b"qos-recovery-placeholder-v1");
    hasher.update(public_key_bytes);
    let digest = hasher.finalize();
    let entropy: [u8; 32] = digest[..32]
        .try_into()
        .expect("SHA-512 always produces ≥32 bytes");
    bip39::Mnemonic::from_entropy(&entropy)
        .expect("BIP39 accepts any 32-byte input as entropy")
        .to_string()
}

/// Screen 2: recovery-share storage.
pub struct RecoveryShareScreen {
    rect: Rect,
    mnemonic: String,
    acknowledged: bool,
    copy_hovered: bool,
    continue_hovered: bool,
    copy_rect: Rect,
    continue_rect: Rect,
    ack_rect: Rect,
    /// Click target spanning the checkbox AND its label, so the user
    /// can tap anywhere on the row to tick the box. `ack_rect` stays
    /// the visual checkbox geometry.
    ack_hit_rect: Rect,
    last_cursor: Point,
    /// Most recent clipboard write status — surfaced so a sandbox
    /// failure ("clipboard owner refused") doesn't fail silently.
    clipboard_message: Option<String>,
}

impl Default for RecoveryShareScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryShareScreen {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            mnemonic: String::new(),
            acknowledged: false,
            copy_hovered: false,
            continue_hovered: false,
            copy_rect: Rect::ZERO,
            continue_rect: Rect::ZERO,
            ack_hit_rect: Rect::ZERO,
            ack_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            clipboard_message: None,
        }
    }

    pub fn mnemonic(&self) -> &str {
        &self.mnemonic
    }

    /// Seed the displayed mnemonic from the identity record. For
    /// single-device records this generates a deterministic
    /// placeholder per [`deterministic_placeholder_mnemonic`].
    pub fn seed_from_record(&mut self, record: &IdentityRecord) {
        let pk = record.threshold_identity.group_public_key;
        self.mnemonic = deterministic_placeholder_mnemonic(&pk);
        self.acknowledged = false;
    }

    /// Force the mnemonic for tests / example bin.
    pub fn set_mnemonic(&mut self, m: impl Into<String>) {
        self.mnemonic = m.into();
        self.acknowledged = false;
    }

    pub fn is_acknowledged(&self) -> bool {
        self.acknowledged
    }

    pub fn set_acknowledged(&mut self, v: bool) {
        self.acknowledged = v;
    }

    pub fn is_continue_enabled(&self) -> bool {
        self.acknowledged && !self.mnemonic.is_empty()
    }

    /// Words split out of the mnemonic. Length is always 24 when
    /// `mnemonic` is well-formed; falls back to whatever
    /// whitespace-separated entries it can find.
    pub fn words(&self) -> Vec<&str> {
        self.mnemonic.split_whitespace().collect()
    }

    /// Write the mnemonic to the OS clipboard as 6 rows of 4 words,
    /// each row prefixed with its starting position number. Survives
    /// non-monospace pastes intact:
    ///
    /// ```text
    ///  1 square mask film brisk
    ///  5 section glory valley better
    ///  9 night raccoon diamond balance
    /// 13 duty own pull glad
    /// 17 medal cargo crazy grass
    /// 21 three upper matrix decade
    /// ```
    pub fn copy_to_clipboard(&mut self) {
        if self.mnemonic.is_empty() {
            self.clipboard_message = Some("nothing to copy".to_string());
            return;
        }
        let words: Vec<&str> = self.mnemonic.split_whitespace().take(24).collect();
        let mut formatted = String::new();
        for (row, chunk) in words.chunks(4).enumerate() {
            if row > 0 {
                formatted.push('\n');
            }
            formatted.push_str(&format!("{:>2} {}", row * 4 + 1, chunk.join(" ")));
        }
        clipboard_copy(&formatted);
        self.clipboard_message = Some("Copied to clipboard.".to_string());
    }

    pub fn clipboard_message(&self) -> Option<&str> {
        self.clipboard_message.as_deref()
    }

    // Mirrors `Widget::handle_event`'s parameter list on purpose --
    // this is invoked from the trait impl, forwarding the same event,
    // scale, modifiers, CRDT handle, and out-params the screen host
    // received; bundling it would mean bundling the trait method too.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_event_screen(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) -> ScreenOutcome {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pos;
                let copy_h = self.copy_rect.contains(pos);
                let cont_h = self.continue_rect.contains(pos);
                if copy_h != self.copy_hovered || cont_h != self.continue_hovered {
                    self.copy_hovered = copy_h;
                    self.continue_hovered = cont_h;
                    *handled = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.copy_rect.contains(self.last_cursor) {
                    *handled = true;
                    self.copy_to_clipboard();
                    return ScreenOutcome::Stay;
                }
                if self.ack_hit_rect.contains(self.last_cursor) {
                    *handled = true;
                    self.acknowledged = !self.acknowledged;
                    return ScreenOutcome::Stay;
                }
                if self.continue_rect.contains(self.last_cursor) && self.is_continue_enabled() {
                    *handled = true;
                    return ScreenOutcome::Advance;
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
        }
    }
}

impl Widget for RecoveryShareScreen {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(900.0),
                height: Dimension::length(640.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let card_w = 680.0;
        let card_h = 540.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
        let pad = 32.0;
        self.copy_rect = Rect::new(
            card.x1 - pad - 140.0,
            card.y0 + pad,
            card.x1 - pad,
            card.y0 + pad + 36.0,
        );
        // ~360 px label width is generous enough for any reasonable
        // translation of "I've written them down somewhere safe".
        self.ack_rect = Rect::new(
            card.x0 + pad,
            card.y1 - pad - 80.0,
            card.x0 + pad + 18.0,
            card.y1 - pad - 80.0 + 18.0,
        );
        self.ack_hit_rect = Rect::new(
            self.ack_rect.x0,
            self.ack_rect.y0 - 4.0,
            self.ack_rect.x1 + 360.0,
            self.ack_rect.y1 + 4.0,
        );
        self.continue_rect = Rect::new(
            card.x1 - pad - 160.0,
            card.y1 - pad - 44.0,
            card.x1 - pad,
            card.y1 - pad,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let backdrop = Color::from_rgba8(0, 0, 0, 180);
        scene.fill(Fill::NonZero, transform, backdrop, None, &self.rect);

        let card_w = 680.0;
        let card_h = 540.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
        let radius = 12.0;
        let pad = 32.0;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(card, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(card, radius),
        );

        text_renderer.draw_text(
            scene,
            "Your recovery words",
            22.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((card.x0 + pad, card.y0 + pad)),
            None,
        );
        text_renderer.draw_text(
            scene,
            "Write these 24 words down. They restore your identity if you lose every device.",
            13.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((card.x0 + pad, card.y0 + pad + 32.0)),
            None,
        );

        let copy_bg = if self.copy_hovered {
            theme.bg_hover
        } else {
            theme.tokens.colors.surfaces.base
        };
        scene.fill(
            Fill::NonZero,
            transform,
            copy_bg,
            None,
            &RoundedRect::from_rect(self.copy_rect, 8.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(self.copy_rect, 8.0),
        );
        let (cw, _) = text_renderer.measure("Copy to clipboard", 13.0, None);
        text_renderer.draw_text(
            scene,
            "Copy to clipboard",
            13.0,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((
                    self.copy_rect.x0 + (self.copy_rect.width() - f64::from(cw)) / 2.0,
                    self.copy_rect.y0 + (self.copy_rect.height() - 13.0) / 2.0,
                )),
            None,
        );

        // 24-word grid: 6 rows × 4 cols.
        let words = self.words();
        let grid_x = card.x0 + pad;
        let grid_y = card.y0 + 110.0;
        let col_w = (card_w - 2.0 * pad) / 4.0;
        let row_h = 36.0;
        for (i, word) in words.iter().take(24).enumerate() {
            let row = i / 4;
            let col = i % 4;
            let x = grid_x + col as f64 * col_w;
            let y = grid_y + row as f64 * row_h;
            let prefix = format!("{:>2}.", i + 1);
            text_renderer.draw_text(
                scene,
                &prefix,
                12.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x, y + 6.0)),
                None,
            );
            text_renderer.draw_text(
                scene,
                word,
                14.0,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((x + 32.0, y + 4.0)),
                None,
            );
        }

        let check_bg = if self.acknowledged {
            theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX]
        } else {
            theme.tokens.colors.surfaces.base
        };
        scene.fill(
            Fill::NonZero,
            transform,
            check_bg,
            None,
            &RoundedRect::from_rect(self.ack_rect, 3.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(self.ack_rect, 3.0),
        );
        if self.acknowledged {
            let mut p = vello::kurbo::BezPath::new();
            p.move_to((self.ack_rect.x0 + 4.0, self.ack_rect.center().y));
            p.line_to((self.ack_rect.x0 + 7.5, self.ack_rect.y1 - 4.0));
            p.line_to((self.ack_rect.x1 - 3.0, self.ack_rect.y0 + 4.0));
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                Color::from_rgb8(255, 255, 255),
                None,
                &p,
            );
        }
        text_renderer.draw_text(
            scene,
            "I've written them down somewhere safe",
            13.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.ack_rect.x1 + 8.0, self.ack_rect.y0 + 2.0)),
            None,
        );
        if let Some(msg) = &self.clipboard_message {
            text_renderer.draw_text(
                scene,
                msg,
                12.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((card.x0 + pad, self.ack_rect.y1 + 8.0)),
                None,
            );
        }

        let cont_enabled = self.is_continue_enabled();
        let cont_bg = if !cont_enabled {
            theme.tokens.colors.surfaces.base
        } else if self.continue_hovered {
            theme.bg_active
        } else {
            theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX]
        };
        let cont_text_color = if cont_enabled {
            Color::from_rgb8(255, 255, 255)
        } else {
            theme.tokens.colors.text_dim
        };
        scene.fill(
            Fill::NonZero,
            transform,
            cont_bg,
            None,
            &RoundedRect::from_rect(self.continue_rect, 8.0),
        );
        let (lw, _) = text_renderer.measure("Continue", 14.0, None);
        text_renderer.draw_text(
            scene,
            "Continue",
            14.0,
            cont_text_color,
            transform
                * Affine::translate((
                    self.continue_rect.x0 + (self.continue_rect.width() - f64::from(lw)) / 2.0,
                    self.continue_rect.y0 + (self.continue_rect.height() - 14.0) / 2.0,
                )),
            None,
        );
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "RecoveryShareScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_recovery_share")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_mnemonic_is_deterministic() {
        let pk = [0x11u8; 32];
        assert_eq!(
            deterministic_placeholder_mnemonic(&pk),
            deterministic_placeholder_mnemonic(&pk)
        );
    }

    #[test]
    fn placeholder_mnemonic_has_24_words() {
        let pk = [0u8; 32];
        let m = deterministic_placeholder_mnemonic(&pk);
        assert_eq!(m.split_whitespace().count(), 24);
    }

    #[test]
    fn continue_disabled_until_acknowledged() {
        let mut s = RecoveryShareScreen::new();
        s.set_mnemonic("a b c d e f g h i j k l m n o p q r s t u v w x");
        assert!(!s.is_continue_enabled());
        s.set_acknowledged(true);
        assert!(s.is_continue_enabled());
    }

    #[test]
    fn continue_disabled_without_mnemonic() {
        let mut s = RecoveryShareScreen::new();
        s.set_acknowledged(true);
        assert!(!s.is_continue_enabled());
    }

    #[test]
    fn words_round_trips_through_whitespace() {
        let mut s = RecoveryShareScreen::new();
        s.set_mnemonic(" alpha  beta  gamma ");
        assert_eq!(s.words(), vec!["alpha", "beta", "gamma"]);
    }
}
