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
//! pairing-channel handshake payload and the `qos://invite/...` URL.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use qrcode::{EcLevel, QrCode};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Default quiet-zone size in modules (the standard requires ≥4).
pub const DEFAULT_QUIET_MODULES: usize = 4;

/// Holds the source bytes plus the cached encoded matrix. Re-encodes when
/// the payload changes via [`set_payload`] — the pairing-channel screen
/// calls it every few seconds as the broker rotates the nonce.
pub struct QrWidget {
    rect: Rect,
    payload: Vec<u8>,
    /// Width × Width row-major matrix. `true` = dark module.
    matrix: Vec<bool>,
    /// Number of modules along one side of the encoded matrix (not
    /// counting the quiet zone).
    matrix_size: usize,
    quiet_modules: usize,
    error_correction: EcLevel,
    /// Encoding error, if any. Surfaced inline (the widget paints a
    /// "QR: <error>" label) rather than panicking so a too-large
    /// payload doesn't kill the host app.
    error: Option<String>,
}

impl QrWidget {
    /// Encodes at `EcLevel::M` (~15% damage tolerance), the QR spec's
    /// medium tier — suitable for on-screen reads with a clean lens.
    pub fn new(payload: impl Into<Vec<u8>>) -> Self {
        let mut w = Self {
            rect: Rect::ZERO,
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

    /// Matrix + 2× quiet zone.
    pub fn total_modules(&self) -> usize {
        self.matrix_size + 2 * self.quiet_modules
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
        self.matrix[y * self.matrix_size + x]
    }
}

impl Widget for QrWidget {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

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
            text_renderer.draw_text(
                scene,
                &msg,
                12.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((self.rect.x0 + 8.0, self.rect.y0 + 8.0)),
                None,
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
        let quiet = self.quiet_modules as f64 * module;
        for y in 0..self.matrix_size {
            for x in 0..self.matrix_size {
                if !self.module(x, y) {
                    continue;
                }
                let px = self.rect.x0 + quiet + x as f64 * module;
                let py = self.rect.y0 + quiet + y as f64 * module;
                let cell = Rect::new(px, py, px + module, py + module);
                scene.fill(Fill::NonZero, transform, dark, None, &cell);
            }
        }
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
        "QrWidget"
    }

    fn test_id(&self) -> Option<&str> {
        Some("qr_widget")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_short_payload() {
        let w = QrWidget::new(b"hello".to_vec());
        assert!(w.error().is_none());
        assert!(w.matrix_size >= 21, "version-1 matrix is 21 modules");
    }

    #[test]
    fn encodes_pairing_payload_size() {
        // The pairing-channel payload is JSON ~500-700 bytes.
        let blob = vec![b'A'; 600];
        let w = QrWidget::new(blob);
        assert!(
            w.error().is_none(),
            "600-byte payload must fit at default EC level: {:?}",
            w.error()
        );
    }

    #[test]
    fn set_payload_recomputes_matrix() {
        let mut w = QrWidget::new(b"first".to_vec());
        let first_size = w.matrix_size;
        w.set_payload(vec![b'X'; 200]);
        assert!(
            w.matrix_size >= first_size,
            "longer payload should not shrink the QR"
        );
    }

    #[test]
    fn total_modules_includes_quiet_zone() {
        let w = QrWidget::new(b"x".to_vec());
        assert_eq!(w.total_modules(), w.matrix_size + 2 * DEFAULT_QUIET_MODULES);
    }

    #[test]
    fn module_oob_returns_false() {
        let w = QrWidget::new(b"x".to_vec());
        assert!(!w.module(usize::MAX, 0));
        assert!(!w.module(0, usize::MAX));
    }
}
