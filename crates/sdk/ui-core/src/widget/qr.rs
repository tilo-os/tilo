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
use zeroize::Zeroizing;

use qrcode::{EcLevel, QrCode};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Default quiet-zone size in modules (the standard requires ≥4).
pub const DEFAULT_QUIET_MODULES: usize = 4;
/// The dark modules' colour in [`svg`]: fixed, so a page's dark theme never
/// shows a black-on-black code a scanner cannot see.
const SVG_DARK: &str = "#121214";
/// The quiet zone and ground in [`svg`].
const SVG_LIGHT: &str = "#ffffff";

/// `payload` as SVG markup, for a page that shows the code as an image: the
/// same matrix the widget paints, the quiet zone around it, and one path of
/// dark modules in module units, so the markup scales to any size.
///
/// # Errors
///
/// The payload does not fit a code at [`EcLevel::M`]; the widget then shows
/// the same reason inline.
pub fn svg(payload: &str) -> Result<String, String> {
    use std::fmt::Write as _;

    let code = QrWidget::new(payload.as_bytes().to_vec());
    if let Some(error) = code.error() {
        return Err(error.to_owned());
    }
    let span = code.total_modules();
    let mut path = String::new();
    for y in 0..code.matrix_size {
        for x in 0..code.matrix_size {
            if code.module(x, y) {
                let _ = write!(
                    path,
                    "M{} {}h1v1h-1z",
                    x.saturating_add(code.quiet_modules),
                    y.saturating_add(code.quiet_modules)
                );
            }
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {span} {span}\" \
         shape-rendering=\"crispEdges\"><rect width=\"{span}\" height=\"{span}\" \
         fill=\"{SVG_LIGHT}\"/><path d=\"{path}\" fill=\"{SVG_DARK}\"/></svg>"
    ))
}

/// Holds the source bytes plus the cached encoded matrix. Re-encodes when
/// the payload changes via `set_payload` — the pairing-channel screen
/// calls it every few seconds as the broker rotates the nonce.
pub struct QrWidget {
    rect: Rect,
    test_id: String,
    alt: String,
    payload: Zeroizing<Vec<u8>>,
    secret: bool,
    /// Width × Width row-major matrix. `true` = dark module.
    matrix: Zeroizing<Vec<bool>>,
    /// Number of modules along one side of the encoded matrix (not
    /// counting the quiet zone).
    matrix_size: usize,
    quiet_modules: usize,
    error_correction: EcLevel,
    /// How wide each module is drawn, when the caller asked for a code dense
    /// enough that its squares must be counted rather than guessed at: the
    /// code then takes the room its payload needs instead of a fixed box.
    module_px: Option<u32>,
    /// Encoding error, if any. Surfaced inline (the widget paints a
    /// `"QR: <error>"` label) rather than panicking so a too-large
    /// payload doesn't kill the host app.
    error: Option<String>,

    /// Input or a new document value changed what this widget paints: it
    /// repaints once, and rests after the frame draws it.
    dirty: bool,
}

impl QrWidget {
    /// Encodes at `EcLevel::M` (~15% damage tolerance), the QR spec's
    /// medium tier — suitable for on-screen reads with a clean lens.
    pub fn new(payload: impl Into<Vec<u8>>) -> Self {
        let mut w = Self {
            dirty: true,
            rect: Rect::ZERO,
            test_id: "qr_widget".to_owned(),
            alt: "QR code".to_owned(),
            payload: Zeroizing::new(Vec::new()),
            secret: false,
            matrix: Zeroizing::new(Vec::new()),
            matrix_size: 0,
            quiet_modules: DEFAULT_QUIET_MODULES,
            error_correction: EcLevel::M,
            module_px: None,
            error: None,
        };
        w.set_payload(payload);
        w
    }

    /// Mark pairing material as private. A nonempty code participates in the
    /// host's existing screenshot, recording and screen-sharing exclusion.
    /// Its accessible name should describe the operation without the payload.
    #[must_use]
    pub fn with_secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// [`Self::with_secret`] after construction.
    pub fn set_secret(&mut self, secret: bool) {
        self.dirty = true;
        self.secret = secret;
    }

    /// Whether it marks private pairing material.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.secret
    }

    /// [`Self::with_alt`] after construction.
    pub fn set_alt(&mut self, alt: impl Into<String>) {
        self.dirty = true;
        self.alt = alt.into();
    }

    /// Give the generated image a stable address for accessibility,
    /// inspection, and headless scenarios.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = id.into();
        self
    }

    /// Draw each module `px` pixels across, and make the box as large as the
    /// payload needs: what a code carries can run to a kilobyte, and a
    /// kilobyte's modules in a fixed box are too small to tell apart on a
    /// screen. A code that fits the usual box keeps it without asking.
    #[must_use]
    pub fn with_module_px(mut self, px: u32) -> Self {
        self.dirty = true;
        self.module_px = Some(px.max(1));
        self
    }

    /// The width each module is drawn at, when the caller asked for one.
    #[must_use]
    pub fn module_px(&self) -> Option<u32> {
        self.module_px
    }

    /// Place the code at `rect`, for a painter that lays out by hand rather
    /// than through the widget tree.
    pub fn set_bounds(&mut self, rect: Rect) {
        self.rect = rect;
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
        self.dirty = true;
        self.payload = Zeroizing::new(payload.into());
        if self.payload.is_empty() {
            self.matrix = Zeroizing::new(Vec::new());
            self.matrix_size = 0;
            self.error = None;
            return;
        }
        match QrCode::with_error_correction_level(&self.payload, self.error_correction) {
            Ok(code) => {
                self.matrix_size = code.width();
                self.matrix = Zeroizing::new(
                    code.to_colors()
                        .into_iter()
                        .map(|c| matches!(c, qrcode::Color::Dark))
                        .collect(),
                );
                self.error = None;
            }
            Err(e) => {
                self.matrix_size = 0;
                self.matrix = Zeroizing::new(Vec::new());
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
    crate::described!();

    #[allow(
        clippy::cast_precision_loss,
        reason = "QR module counts are capped by the QR spec at 177"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // 240x240 unless the caller named a module size: then the box is as
        // large as the modules the code carries need, and a card gives it the
        // line that room takes. A code that could not be made keeps the usual
        // box, which is where its error is written.
        let side = if self.error.is_some() {
            240.0
        } else {
            self.module_px
                .and_then(|px| {
                    u32::try_from(self.total_modules())
                        .ok()
                        .map(|mods| mods.saturating_mul(px))
                })
                .map_or(240.0, |side| side as f32)
        };
        let style = Style {
            size: Size::from_lengths(side, side),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

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
        if self.secret && !self.payload.is_empty() {
            crate::secret_paint::note_secret_painted();
        }
        // Fixed white/black regardless of theme, so a dark theme never
        // paints a black-on-black QR a scanner can't see.
        let light = Color::from_rgb8(0xFF, 0xFF, 0xFF);
        let dark = Color::from_rgb8(0, 0, 0);

        if let Some(err) = &self.error {
            scene.fill(Fill::NonZero, transform, light, None, &self.rect);
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
        // Whole pixels per module, unless the caller named the width: a
        // module rounded to a fraction draws wider on one side than the other,
        // and a code of a thousand modules then reads as speckle.
        let module = match self.module_px {
            Some(px) => f64::from(px),
            None => (side / total).floor(),
        };
        if module <= 0.0 {
            return;
        }
        // The code is square; centre it in a non-square allocation so the
        // white surround stays symmetric instead of piling up on one side.
        let drawn = module * total;
        let origin_x = self.rect.x0 + (self.rect.width() - drawn) / 2.0;
        let origin_y = self.rect.y0 + (self.rect.height() - drawn) / 2.0;
        // The white is the code's own ground, as wide as the code: a slot
        // wider or taller than the code then shows it centred on the surface
        // rather than as a pale band around it.
        let ground = Rect::new(origin_x, origin_y, origin_x + drawn, origin_y + drawn);
        scene.fill(Fill::NonZero, transform, light, None, &ground);
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

    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty, self)
    }
    /// A code drawn at a named module size takes the room its payload needs,
    /// so a new payload lays it out again as well as paints it.
    fn is_layout_dirty(&self) -> bool {
        self.dirty && self.module_px.is_some()
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        crate::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "QrWidget"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};

        let mut node = AccessNode::new(Role::Image).with_name(self.alt.clone());
        node.state = node.state.protected(self.secret);
        if let Some(error) = &self.error {
            node = node.with_description(format!("QR encoding failed: {error}"));
        }
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page shows a pairing link as this markup: the code's own modules
    /// with the quiet zone around them, and nothing else to draw.
    #[test]
    fn a_page_shows_a_link_as_one_svg_path() {
        let markup = svg("HELLO").unwrap();
        assert!(
            markup.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 29 29\""),
            "{markup}"
        );
        assert_eq!(markup.matches("<path ").count(), 1);
        assert_eq!(markup.matches("<rect ").count(), 1);

        // 159 bytes is version 9 at level M (version 8 holds 150): 53 modules
        // a side, and the quiet zone around them.
        let link = format!(
            "https://tilo.coffee/#/@eu1.tilo.coffee/w/0123456789abcdef?pair={}",
            "ab".repeat(48)
        );
        let code = QrWidget::new(link.as_bytes().to_vec());
        assert!(code.error().is_none(), "{:?}", code.error());
        assert_eq!(code.matrix_size, 53);
        assert!(svg(&link).unwrap().contains("viewBox=\"0 0 61 61\""));
        let finder = [
            "1111111", "1000001", "1011101", "1011101", "1011101", "1000001", "1111111",
        ];
        for (left, top) in [(0, 0), (46, 0), (0, 46)] {
            for (row, expected) in finder.iter().enumerate() {
                let drawn: String = (0..7)
                    .map(|x| {
                        if code.module(left + x, top + row) {
                            '1'
                        } else {
                            '0'
                        }
                    })
                    .collect();
                assert_eq!(drawn, *expected, "finder at {left},{top}");
            }
        }
        assert!(svg("x".repeat(3000).as_str()).is_err());
    }

    #[test]
    fn private_pairing_qr_marks_real_paint_and_clears_when_cancelled() {
        use crate::secret_paint::SecretPaint;
        let mut code =
            QrWidget::new(b"sgnl://linkdevice?uuid=synthetic&pub_key=synthetic".to_vec())
                .with_secret()
                .with_alt("Link this device to Signal");
        let presence = PresenceCtx::empty();
        let mut renderer = TextRenderer::new();
        let painted = SecretPaint::begin();
        code.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut renderer,
            &Theme::dark(),
            &presence,
        );
        assert!(painted.finish());
        let semantics = code.accessibility().unwrap();
        assert_eq!(
            semantics.name.as_deref(),
            Some("Link this device to Signal")
        );
        assert!(semantics.description.is_none());
        assert!(semantics.value.is_none());
        assert!(semantics.state.protected);
        code.set_payload(Vec::new());
        assert_eq!(code.matrix_size, 0);
        assert!(code.matrix.is_empty());
        let painted = SecretPaint::begin();
        code.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut renderer,
            &Theme::light(),
            &presence,
        );
        assert!(!painted.finish());
    }

    #[test]
    fn public_qr_paint_keeps_capture_available() {
        use crate::secret_paint::SecretPaint;
        let mut code = QrWidget::new(b"https://example.test/public".to_vec());
        let painted = SecretPaint::begin();
        code.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        assert!(!painted.finish());
    }

    #[test]
    fn qr_matrix_encodes_payloads_with_a_quiet_zone() {
        {
            let w = QrWidget::new(b"hello".to_vec());
            assert!(w.error().is_none());
            assert!(w.matrix_size >= 21, "version-1 matrix is 21 modules");
        }

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

        {
            let mut w = QrWidget::new(b"first".to_vec());
            let first_size = w.matrix_size;
            w.set_payload(vec![b'X'; 200]);
            assert!(
                w.matrix_size >= first_size,
                "longer payload should not shrink the QR"
            );
        }

        {
            let w = QrWidget::new(b"x".to_vec());
            assert_eq!(w.total_modules(), w.matrix_size + 2 * DEFAULT_QUIET_MODULES);
        }

        {
            let w = QrWidget::new(b"x".to_vec());
            assert!(!w.module(usize::MAX, 0));
            assert!(!w.module(0, usize::MAX));
        }
    }

    #[test]
    fn a_dense_code_takes_the_room_its_modules_need() {
        let px = 4u32;
        let mut code = QrWidget::new(vec![b'i'; 1000]).with_module_px(px);
        assert!(code.error().is_none(), "an invitation-sized payload fits");
        assert_eq!(code.module_px(), Some(px));
        let want = u16::try_from(code.total_modules())
            .expect("the code's modules fit")
            .saturating_mul(u16::try_from(px).expect("the module width fits"));
        let mut taffy = TaffyTree::new();
        let node = code.build_layout(&mut taffy, &Theme::dark(), &mut HashMap::new());
        let laid = taffy.style(node).expect("the code is laid out").size;
        let want = taffy::Dimension::length(f32::from(want));
        assert_eq!(laid.width, want);
        assert_eq!(laid.height, want);

        // A code that fits the usual box keeps it without asking.
        let mut small = QrWidget::new(b"hello".to_vec());
        let node = small.build_layout(&mut taffy, &Theme::dark(), &mut HashMap::new());
        let laid = taffy.style(node).expect("the code is laid out").size;
        assert_eq!(laid.width, taffy::Dimension::length(240.0));
    }

    #[test]
    fn qr_image_semantics_stay_stable_through_encoding_failure() {
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
