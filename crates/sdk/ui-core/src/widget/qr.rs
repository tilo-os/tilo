//! QR-code widget.
//!
//! Wraps the `qrcode` crate and paints the resulting module matrix as a
//! grid of square Vello fills.
//!
//! - A configurable "quiet zone" of empty modules wraps the matrix so any
//!   consumer scanner accepts the result. Defaults to four modules per
//!   the QR spec.
//! - No animation, no caching layer. Encoding is a few hundred
//!   microseconds for the payload sizes we care about (~200-400 bytes),
//!   so re-encoding on every paint keeps the widget declarative ("show
//!   this payload").
//!
//! The identity-flow ceremony uses this widget to encode both the
//! pairing-channel handshake payload and the `tilo://invite/...` URL.

use std::collections::HashMap;

use qrcode::{EcLevel, QrCode};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Default quiet-zone size in modules (the standard requires ≥4).
pub const DEFAULT_QUIET_MODULES: usize = 4;

/// Holds the source bytes plus the cached encoded matrix. Re-encodes when
/// the payload changes via `set_payload` — the pairing-channel screen
/// calls it every few seconds as the broker rotates the nonce.
pub struct QrWidget {
    rect: Rect,
    test_id: String,
    alt: String,
    payload: Vec<u8>,
    /// Width × Width row-major matrix. `true` = dark module.
    matrix: Vec<bool>,
    /// Number of modules along one side of the encoded matrix (not
    /// counting the quiet zone).
    matrix_size: usize,
    quiet_modules: usize,
    error_correction: EcLevel,
    /// Encoding error, if any. Surfaced inline (the widget paints a
    /// `"QR: <error>"` label) rather than panicking so a too-large
    /// payload doesn't kill the host app.
    error: Option<String>,
}

impl QrWidget {
    /// Encodes at `EcLevel::M` (~15% damage tolerance), the QR spec's
    /// medium tier — suitable for on-screen reads with a clean lens.
    pub fn new(payload: impl Into<Vec<u8>>) -> Self {
        let mut w = Self {
            rect: Rect::ZERO,
            test_id: "qr_widget".to_owned(),
            alt: "QR code".to_owned(),
            payload: Vec::new(),
            matrix: Vec::new(),
            matrix_size: 0,
            quiet_modules: DEFAULT_QUIET_MODULES,
            error_correction: EcLevel::M,
            error: None,
        };
        w.set_payload(payload);
        w
    }

    /// Give the generated image a stable address for accessibility,
    /// inspection, and headless scenarios.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = id.into();
        self
    }

    /// Describe what scanning the code represents. A generic "QR code" name
    /// is supplied by default; applications should provide the useful payload
    /// meaning rather than forcing a screen-reader user to infer it visually.
    #[must_use]
    pub fn with_alt(mut self, alt: impl Into<String>) -> Self {
        self.alt = alt.into();
        self
    }

    /// The accessible description of the generated code.
    #[must_use]
    pub fn alt(&self) -> &str {
        &self.alt
    }

    /// A failure to encode (most commonly: payload too large for the
    /// chosen EC level) is stored in `self.error` for inline display.
    pub fn set_payload(&mut self, payload: impl Into<Vec<u8>>) {
        self.payload = payload.into();
        match QrCode::with_error_correction_level(&self.payload, self.error_correction) {
            Ok(code) => {
                self.matrix_size = code.width();
                self.matrix = code
                    .to_colors()
                    .into_iter()
                    .map(|c| matches!(c, qrcode::Color::Dark))
                    .collect();
                self.error = None;
            }
            Err(e) => {
                self.matrix_size = 0;
                self.matrix.clear();
                self.error = Some(format!("{e}"));
            }
        }
    }

    pub fn quiet_modules(&self) -> usize {
        self.quiet_modules
    }

    /// Matrix + 2× quiet zone. The QR spec caps `matrix_size` at 177
    /// (version 40) and `quiet_modules` is a widget-construction constant, so
    /// this can never realistically overflow; `saturating_*` keeps it
    /// panic-free even so.
    pub fn total_modules(&self) -> usize {
        self.matrix_size
            .saturating_add(self.quiet_modules.saturating_mul(2))
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// `(x, y)` within the matrix, no quiet-zone offset. Used by tests to
    /// inspect the encoded pattern without poking `matrix` directly.
    pub fn module(&self, x: usize, y: usize) -> bool {
        if x >= self.matrix_size || y >= self.matrix_size {
            return false;
        }
        let idx = y.saturating_mul(self.matrix_size).saturating_add(x);
        self.matrix.get(idx).copied().unwrap_or(false)
    }
}

impl Widget for QrWidget {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Default 240x240 box; callers can wrap us in a sized container.
        // The module side recomputes in draw from the resolved rect.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(240.0),
                height: Dimension::length(240.0),
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
    }

    // The QR spec caps `matrix_size` at 177 (version 40) and `quiet_modules`
    // is a small widget-construction constant, so every `usize -> f64` cast
    // below stays far below f64's 52-bit mantissa limit.
    #[allow(
        clippy::cast_precision_loss,
        reason = "QR module counts are capped by the QR spec at 177"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Fixed white/black regardless of theme, so a dark theme never
        // paints a black-on-black QR a scanner can't see.
        let light = Color::from_rgb8(0xFF, 0xFF, 0xFF);
        let dark = Color::from_rgb8(0, 0, 0);
        scene.fill(Fill::NonZero, transform, light, None, &self.rect);

        if let Some(err) = &self.error {
            let msg = format!("QR error: {err}");
            // Affine composition: pure `f64` matrix multiply, cannot panic
            // regardless of operand magnitude.
            let inset = f64::from(theme.tokens.spacing.s2);
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix math and cannot panic"
            )]
            let text_transform =
                transform * Affine::translate((self.rect.x0 + inset, self.rect.y0 + inset));
            text_renderer.draw_style(
                scene,
                &msg,
                theme.tokens.colors.text_dim,
                text_transform,
                None,
                &theme.text(TextRole::Caption),
            );
            return;
        }

        if self.matrix_size == 0 {
            return;
        }

        let total = self.total_modules() as f64;
        let side = (self.rect.width().min(self.rect.height())).max(0.0);
        let module = side / total;
        if module <= 0.0 {
            return;
        }
        // The code is square; centre it in a non-square allocation so the
        // white surround stays symmetric instead of piling up on one side.
        let origin_x = self.rect.x0 + (self.rect.width() - side) / 2.0;
        let origin_y = self.rect.y0 + (self.rect.height() - side) / 2.0;
        let quiet = self.quiet_modules as f64 * module;
        for y in 0..self.matrix_size {
            for x in 0..self.matrix_size {
                if !self.module(x, y) {
                    continue;
                }
                let px = origin_x + quiet + x as f64 * module;
                let py = origin_y + quiet + y as f64 * module;
                let cell = Rect::new(px, py, px + module, py + module);
                scene.fill(Fill::NonZero, transform, dark, None, &cell);
            }
        }
    }

    fn name(&self) -> &'static str {
        "QrWidget"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};

        let mut node = AccessNode::new(Role::Image).with_name(self.alt.clone());
        if let Some(error) = &self.error {
            node = node.with_description(format!("QR encoding failed: {error}"));
        }
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_matrix_encodes_payloads_with_a_quiet_zone() {
        // Case `encodes_short_payload`.
        {
            let w = QrWidget::new(b"hello".to_vec());
            assert!(w.error().is_none());
            assert!(w.matrix_size >= 21, "version-1 matrix is 21 modules");
        }

        // Case `encodes_pairing_payload_size`.
        {
            // The pairing-channel payload is JSON ~500-700 bytes.
            let blob = vec![b'A'; 600];
            let w = QrWidget::new(blob);
            assert!(
                w.error().is_none(),
                "600-byte payload must fit at default EC level: {:?}",
                w.error()
            );
        }

        // Case `set_payload_recomputes_matrix`.
        {
            let mut w = QrWidget::new(b"first".to_vec());
            let first_size = w.matrix_size;
            w.set_payload(vec![b'X'; 200]);
            assert!(
                w.matrix_size >= first_size,
                "longer payload should not shrink the QR"
            );
        }

        // Case `total_modules_includes_quiet_zone`.
        {
            let w = QrWidget::new(b"x".to_vec());
            assert_eq!(w.total_modules(), w.matrix_size + 2 * DEFAULT_QUIET_MODULES);
        }

        // Case `module_oob_returns_false`.
        {
            let w = QrWidget::new(b"x".to_vec());
            assert!(!w.module(usize::MAX, 0));
            assert!(!w.module(0, usize::MAX));
        }
    }

    #[test]
    fn qr_image_semantics_stay_stable_through_encoding_failure() {
        // Case `generated_code_has_stable_named_image_semantics`.
        {
            use crate::a11y::Role;

            let w = QrWidget::new(b"https://example.com".to_vec())
                .with_id("share-code")
                .with_alt("Share link for Project Atlas");
            assert_eq!(Widget::test_id(&w), Some("share-code"));
            assert_eq!(w.alt(), "Share link for Project Atlas");
            let node = Widget::accessibility(&w).expect("QR code is semantic content");
            assert_eq!(node.role, Role::Image);
            assert_eq!(node.name.as_deref(), Some("Share link for Project Atlas"));
        }

        // Case `encoding_failure_is_exposed_without_replacing_the_image_identity`.
        {
            use crate::a11y::Role;

            let w = QrWidget::new(vec![b'x'; 10_000]).with_id("oversized-code");
            assert!(w.error().is_some());
            let node = Widget::accessibility(&w).expect("failed output remains inspectable");
            assert_eq!(node.role, Role::Image);
            assert_eq!(node.name.as_deref(), Some("QR code"));
            assert!(
                node.description
                    .as_deref()
                    .is_some_and(|description| description.starts_with("QR encoding failed:"))
            );
        }
    }
}
