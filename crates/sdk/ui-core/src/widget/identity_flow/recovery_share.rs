//! Screen 2 — recovery-share storage.
//!
//! Displays the 24-word BIP39 mnemonic in a numbered 6×4 grid. The
//! user must explicitly tick "I've written it down" before the
//! continue button activates.
//!
//! The screen never invents recovery words. In single-device bootstrap mode
//! no independent recovery artifact exists, so the ceremony presents an
//! unavailable state until a 2-of-N DKG has produced a share.

use std::collections::HashMap;
use std::fmt::Write as _;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
#[cfg(test)]
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::text_selection::clipboard_copy;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::sheet::{
    Align, BodyScroll, GRID_SHEET_WIDTH, Resolved, Role, Rows, SheetSpec, WordGrid, check_box_rect,
    paint_check_row, paint_text, paragraph_height, resolve, with_layout_renderer,
};
use crate::widget::identity_flow::{IdentityRecord, ScreenOutcome};
use crate::widget::modal_dialog::ModalAction;

const TITLE_READY: &str = "Your recovery words";
const TITLE_UNAVAILABLE: &str = "Recovery is not ready yet";
const EXPLANATION_READY: &str =
    "Write these 24 words down. They restore your identity if you lose every device.";
const EXPLANATION_UNAVAILABLE: &str = "Pair another trusted device first. Tilo will show real \
     recovery words only after the secure setup completes.";
const ACK_LABEL: &str = "I've written them down somewhere safe";
const COPY_LABEL: &str = "Copy to clipboard";
const CONTINUE_LABEL: &str = "Continue";

/// Action-row indices, in authoring order.
const ACTION_COPY: usize = 0;
const ACTION_CONTINUE: usize = 1;

/// The mnemonic is 24 words; the grid never shows more.
const WORD_COUNT: usize = 24;

/// Every rect the screen paints and hit-tests, from one pass over the rows.
#[derive(Clone, Debug, Default)]
struct Plan {
    explanation: Rect,
    /// The word grid's area, when there are words to show.
    grid: Option<Rect>,
    ack_row: Rect,
    clipboard: Option<Rect>,
    bottom: f64,
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
    /// The theme `build_layout` saw, so `apply_layout` can resolve the rows
    /// without waiting for a paint.
    layout_theme: Option<Theme>,
    scroll: BodyScroll,
    body_height: f64,
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
            layout_theme: None,
            scroll: BodyScroll::default(),
            body_height: 0.0,
        }
    }

    pub fn mnemonic(&self) -> &str {
        &self.mnemonic
    }

    /// Public metadata can never prove that a usable recovery secret exists.
    /// Clear the screen until the live threshold ceremony supplies genuine
    /// words via [`Self::set_mnemonic`].
    pub fn seed_from_record(&mut self, _record: &IdentityRecord) {
        self.mnemonic.clear();
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
        let words: Vec<&str> = self.mnemonic.split_whitespace().take(WORD_COUNT).collect();
        let mut formatted = String::new();
        for (row, chunk) in words.chunks(4).enumerate() {
            if row > 0 {
                formatted.push('\n');
            }
            let start = row.saturating_mul(4).saturating_add(1);
            let _ = write!(formatted, "{start:>2} {}", chunk.join(" "));
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
    pub fn handle_event_screen(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> ScreenOutcome {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = pos;
                let copy_h = self.copy_rect.contains(pos);
                let cont_h = self.continue_rect.contains(pos);
                if copy_h != self.copy_hovered || cont_h != self.continue_hovered {
                    self.copy_hovered = copy_h;
                    self.continue_hovered = cont_h;
                    *ctx.handled = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if !self.mnemonic.is_empty() && self.copy_rect.contains(self.last_cursor) {
                    *ctx.handled = true;
                    self.copy_to_clipboard();
                    return ScreenOutcome::Stay;
                }
                if self.ack_hit_rect.contains(self.last_cursor) {
                    *ctx.handled = true;
                    self.acknowledged = !self.acknowledged;
                    return ScreenOutcome::Stay;
                }
                if self.continue_rect.contains(self.last_cursor) && self.is_continue_enabled() {
                    *ctx.handled = true;
                    return ScreenOutcome::Advance;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.scroll.wheel(delta) {
                    *ctx.handled = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.scroll.key(logical_key, self.body_height) {
                    *ctx.handled = true;
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
        }
    }

    /// One geometry pass for both `apply_layout` and `draw`; the
    /// acknowledgement row's target is the part of it inside the body, so
    /// once it scrolls under the action row it cannot take Continue's click.
    fn resolve(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) -> Resolved<Plan> {
        let actions = self.actions();
        let spec = SheetSpec {
            title: self.title(),
            max_width: GRID_SHEET_WIDTH,
            actions: &actions,
        };
        let mut scroll = self.scroll;
        let resolved = resolve(
            self.rect,
            &spec,
            &mut scroll,
            text_renderer,
            theme,
            |rows, control_h, tr, theme| {
                let plan = self.plan(rows, control_h, tr, theme);
                let bottom = plan.bottom;
                (plan, bottom)
            },
        );
        self.scroll = scroll;
        let body = resolved.sheet.body();
        self.body_height = body.height();
        self.ack_hit_rect = BodyScroll::visible(body, resolved.plan.ack_row);
        self.ack_rect = BodyScroll::visible(body, check_box_rect(resolved.plan.ack_row, theme));
        self.copy_rect = resolved.sheet.action(ACTION_COPY);
        self.continue_rect = resolved.sheet.action(ACTION_CONTINUE);
        resolved
    }

    fn title(&self) -> &'static str {
        if self.mnemonic.is_empty() {
            TITLE_UNAVAILABLE
        } else {
            TITLE_READY
        }
    }

    fn explanation(&self) -> &'static str {
        if self.mnemonic.is_empty() {
            EXPLANATION_UNAVAILABLE
        } else {
            EXPLANATION_READY
        }
    }

    fn actions(&self) -> [ModalAction<'static>; 2] {
        [
            ModalAction::secondary(COPY_LABEL)
                .enabled(!self.mnemonic.is_empty())
                .hovered(self.copy_hovered),
            ModalAction::primary(CONTINUE_LABEL)
                .enabled(self.is_continue_enabled())
                .hovered(self.continue_hovered),
        ]
    }

    /// Walk the body rows once: explanation, the word grid (one control
    /// row per grid row, so the verify screen's blanks fit the same
    /// cells), the acknowledgement row, any clipboard status.
    fn plan(
        &self,
        mut rows: Rows,
        control_h: f64,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Plan {
        let sp = &theme.tokens.spacing;
        let width = rows.width();
        let explanation = rows.row(paragraph_height(
            text_renderer,
            theme,
            Role::Body,
            self.explanation(),
            width,
        ));
        let word_count = self.words().len().min(WORD_COUNT);
        let grid = (word_count > 0).then(|| {
            rows.gap(sp.s6);
            rows.row(WordGrid::height(word_count, control_h))
        });
        rows.gap(sp.s6);
        let ack_row = rows.row(control_h);
        let clipboard = self.clipboard_message.as_deref().map(|msg| {
            rows.gap(sp.s2);
            rows.row(paragraph_height(
                text_renderer,
                theme,
                Role::Caption,
                msg,
                width,
            ))
        });
        Plan {
            explanation,
            grid,
            ack_row,
            clipboard,
            bottom: rows.bottom(),
        }
    }

    /// The 24-word grid: ordinal in the label role, word in the value
    /// role, each on its cell's centre line.
    fn draw_word_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        control_h: f64,
    ) {
        let grid = WordGrid::new(area, control_h, text_renderer, theme);
        for (i, word) in self.words().iter().take(WORD_COUNT).enumerate() {
            let (ordinal, cell) = grid.cell(i, theme);
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                ordinal,
                &format!("{}.", i.saturating_add(1)),
                Role::Label,
                Align::Start,
                false,
            );
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                cell,
                word,
                Role::Value,
                Align::Start,
                false,
            );
        }
    }
}

impl Widget for RecoveryShareScreen {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = Some(theme.clone());
        // The screen owns the viewport, so the sheet centres on the window
        // rather than inside a fixed box at the origin.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
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
        if let Some(theme) = self.layout_theme.clone() {
            with_layout_renderer(&theme, |tr| {
                self.resolve(&theme, tr);
            });
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let resolved = self.resolve(theme, text_renderer);
        let actions = self.actions();
        let spec = SheetSpec {
            title: self.title(),
            max_width: GRID_SHEET_WIDTH,
            actions: &actions,
        };
        let sheet = &resolved.sheet;
        let plan = &resolved.plan;
        let control_h = sheet.control_height(theme);

        sheet.paint_frame(scene, transform, text_renderer, theme, &spec);
        sheet.clip_body(scene, transform);
        paint_text(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            plan.explanation,
            self.explanation(),
            Role::Body,
            Align::Start,
            true,
        );
        if let Some(area) = plan.grid {
            self.draw_word_grid(scene, transform, text_renderer, theme, area, control_h);
        }
        let ack_hovered = self.ack_hit_rect.contains(self.last_cursor);
        paint_check_row(
            scene,
            transform,
            text_renderer,
            theme,
            plan.ack_row,
            self.acknowledged,
            ack_hovered,
            ACK_LABEL,
        );
        if let (Some(rect), Some(msg)) = (plan.clipboard, self.clipboard_message.as_deref()) {
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                msg,
                Role::Caption,
                Align::Start,
                true,
            );
        }
        scene.pop_layer();
        sheet.paint_actions(scene, transform, text_renderer, theme, &spec);
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
    use getrandom::{SysRng, rand_core::UnwrapErr};

    #[test]
    fn recovery_share_gates_continue_and_never_fakes_words() {
        fn metadata_never_generates_fake_recovery_words() {
            let mut rng = UnwrapErr(SysRng);
            let id =
                threshold_identity::ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
            let record = IdentityRecord::from_threshold_identity(
                &id,
                "Ada",
                [1, 2, 3],
                crate::time::SystemTime::UNIX_EPOCH,
            )
            .unwrap();
            let mut screen = RecoveryShareScreen::new();
            screen.seed_from_record(&record);
            assert!(screen.mnemonic().is_empty());
            assert!(!screen.is_continue_enabled());
        }

        fn continue_disabled_until_acknowledged() {
            let mut s = RecoveryShareScreen::new();
            s.set_mnemonic("a b c d e f g h i j k l m n o p q r s t u v w x");
            assert!(!s.is_continue_enabled());
            s.set_acknowledged(true);
            assert!(s.is_continue_enabled());
        }

        fn continue_disabled_without_mnemonic() {
            let mut s = RecoveryShareScreen::new();
            s.set_acknowledged(true);
            assert!(!s.is_continue_enabled());
        }

        fn words_round_trips_through_whitespace() {
            let mut s = RecoveryShareScreen::new();
            s.set_mnemonic(" alpha  beta  gamma ");
            assert_eq!(s.words(), vec!["alpha", "beta", "gamma"]);
        }

        metadata_never_generates_fake_recovery_words();
        continue_disabled_until_acknowledged();
        continue_disabled_without_mnemonic();
        words_round_trips_through_whitespace();
    }

    #[test]
    fn recovery_share_layout_reserves_rows_at_every_text_scale() {
        /// With words the grid is six control-tall rows; without them there is
        /// no grid and the acknowledgement row follows the explanation directly.
        fn plan_reserves_six_control_rows_for_twenty_four_words() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let body = Rect::new(100.0, 200.0, 740.0, 200.0);
            let mut s = RecoveryShareScreen::new();
            let bare = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            assert!(bare.grid.is_none(), "no words, no grid");
            assert_eq!(bare.ack_row.height(), 34.0);

            s.set_mnemonic("a b c d e f g h i j k l m n o p q r s t u v w x");
            let full = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            let grid = full.grid.expect("24 words fill a grid");
            assert!((grid.height() - 6.0 * 34.0).abs() < 1e-9);
            assert_eq!((grid.x0, grid.x1), (body.x0, body.x1));
            assert!(
                (full.ack_row.y0 - grid.y1 - f64::from(theme.tokens.spacing.s6)).abs() < 1e-9,
                "one group gap between the grid and the acknowledgement"
            );
            assert!((full.bottom - full.ack_row.y1).abs() < 1e-9);
        }

        /// At a large text scale the body outruns the card. The acknowledgement
        /// row must then be clipped to the body, never under the action row —
        /// and reachable by scrolling, since Continue depends on it.
        fn at_two_x_text_the_ack_row_never_overlaps_the_actions_and_scrolls_into_view() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut s = RecoveryShareScreen::new();
            s.set_mnemonic("a b c d e f g h i j k l m n o p q r s t u v w x");
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map = HashMap::new();
            let node = s.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(1280.0),
                        height: taffy::AvailableSpace::Definite(420.0),
                    },
                )
                .expect("layout");
            s.apply_layout(&taffy, node, Point::ORIGIN);
            assert!(
                s.scroll.max() > 0.0,
                "24 words at 2× do not fit a 420 px window"
            );
            let actions = Rect::new(
                s.copy_rect.x0.min(s.continue_rect.x0),
                s.copy_rect.y0.min(s.continue_rect.y0),
                s.copy_rect.x1.max(s.continue_rect.x1),
                s.copy_rect.y1.max(s.continue_rect.y1),
            );
            assert!(
                s.ack_hit_rect.intersect(actions).is_zero_area(),
                "the acknowledgement target {:?} must not reach the action row {actions:?}",
                s.ack_hit_rect
            );
            // Scroll to the bottom: the row is now inside the body and clickable.
            assert!(s.scroll.key(&Key::Named(NamedKey::PageDown), s.body_height));
            while s.scroll.key(&Key::Named(NamedKey::PageDown), s.body_height) {}
            s.apply_layout(&taffy, node, Point::ORIGIN);
            assert!(
                s.ack_hit_rect.area() > 0.0,
                "scrolled to the end, the row is a target"
            );
            assert!(s.ack_hit_rect.intersect(actions).is_zero_area());
            assert!(s.ack_hit_rect.y1 <= s.continue_rect.y0);
        }

        plan_reserves_six_control_rows_for_twenty_four_words();
        at_two_x_text_the_ack_row_never_overlaps_the_actions_and_scrolls_into_view();
    }
}
