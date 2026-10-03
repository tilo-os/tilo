//! Screen 1 — first-run identity creation.
//!
//! Layout, top-to-bottom inside the shared sheet (`super::sheet`):
//!
//! ```text
//! +----------------------------------------+
//! | Tilo — create your identity             |
//! |                                        |
//! | [explanation paragraph]                |
//! |                                        |
//! | Display name                           |
//! | [text input                          ] |
//! |                                        |
//! | Accent colour                          |
//! | [#][#][#][#][#][#][#][#]               |
//! |                                        |
//! |                   [Create my identity] |
//! +----------------------------------------+
//! ```
//!
//! Submit is disabled while the display name is empty (whitespace only
//! counts as empty). On click: mints a fresh identity, wraps it in an
//! `IdentityRecord`, saves it via the `IdentityStore`, then completes the
//! single-device ceremony. Recovery-word screens are reserved for a genuine
//! threshold share produced by the multi-device ceremony; the production
//! runtime backs up a single-device identity as an encrypted portable file.
//!
//! The form validation + submit pipeline is exposed as plain methods so
//! tests can drive the screen without rendering; `submit_to_store` is the
//! load-bearing path, also used by `handle_event_screen` on a real click.
//!
//! The display-name field is the shared [`TextInput`], embedded the way
//! `ModalPrompt` embeds one: the sheet's row plan places it
//! (`place_field`), the screen forwards it the events it does not claim
//! for itself (Enter submits, PageUp/PageDown scroll), and it hangs off the
//! screen's layout node as a child of its own so the host's focus order,
//! the accessibility tree and the headless driver all reach it by name.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use getrandom::{SysRng, rand_core::UnwrapErr};
use taffy::{Dimension, Display, NodeId, Size, Style, TaffyTree};
use threshold_identity::ThresholdIdentity;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::sheet::{
    Align, BodyScroll, FORM_SHEET_WIDTH, Resolved, Role, Rows, SheetSpec, edited, line_height,
    paint_text, paragraph_height, place_field, resolve, with_layout_renderer,
};
use crate::widget::identity_flow::{
    IdentityFlowError, IdentityRecord, IdentityStore, ScreenOutcome,
};
use crate::widget::modal_dialog::ModalAction;
use crate::widget::text_input::TextInput;

/// Default accent-tile palette. Picked so each tile reads as a
/// distinct hue at a small swatch size. Stored as `[r, g, b]`.
pub const ACCENT_PALETTE: [[u8; 3]; 8] = [
    [248, 113, 113], // rose
    [251, 146, 60],  // amber
    [250, 204, 21],  // gold
    [134, 239, 172], // mint
    [56, 189, 248],  // sky
    [129, 140, 248], // indigo
    [192, 132, 252], // violet
    [244, 114, 182], // pink
];

const TITLE: &str = "Create your Tilo identity";
const EXPLANATION: &str =
    "Your identity signs every action you take and lets you join shared channels.";
const NAME_LABEL: &str = "Display name";
const NAME_PLACEHOLDER: &str = "Your name…";
const ACCENT_LABEL: &str = "Accent colour";
const SUBMIT_LABEL: &str = "Create my identity";
/// `test_id` of the display-name field — the handle the headless driver,
/// the inspector and assistive technology address it by.
pub const NAME_INPUT_ID: &str = "identity_name_input";

/// Narrows a small layout count/index (tile counts and positions, always
/// within the fixed 8-entry [`ACCENT_PALETTE`]) to `f64`. Bounded far
/// below `f64`'s precision limit, so this never loses precision in
/// practice.
#[allow(
    clippy::cast_precision_loss,
    reason = "tile counts/indices are bounded to ACCENT_PALETTE's fixed small length"
)]
fn small_f64(v: usize) -> f64 {
    v as f64
}

/// Every rect the screen paints and hit-tests, resolved from one pass over
/// the body rows. Built twice per frame: once unplaced to learn the body's
/// height, once inside the sheet that height produced.
#[derive(Clone, Debug, Default)]
struct Plan {
    explanation: Rect,
    name_label: Rect,
    input: Rect,
    accent_label: Rect,
    tiles: Vec<Rect>,
    error: Option<Rect>,
    bottom: f64,
}

pub struct CreateIdentityScreen {
    rect: Rect,
    /// The display-name field: the shared text input, embedded. Use
    /// `display_name()` / `set_display_name()` to read/write.
    name: TextInput,
    /// The field's own layout node, minted by `build_layout`, so a click in
    /// the field hands the host that node as the focus target.
    name_node: Option<NodeId>,
    /// The field's text as last seen, so an edit is a change of text and a
    /// caret move is not (see [`edited`]).
    last_name: String,
    selected_accent: usize,
    submit_hovered: bool,
    submit_rect: Rect,
    /// Accent-tile rects, resolved by the last `draw`.
    tile_rects: Vec<Rect>,
    /// Text-input field rect, resolved by the last `draw`.
    input_rect: Rect,
    /// Error from the last submit attempt, if any.
    submit_error: Option<String>,
    /// The most recent record produced by a successful submit.
    last_record: Option<IdentityRecord>,
    /// Last cursor position (set by `CursorMoved`) so the next
    /// `MouseInput` can hit-test against rects.
    last_cursor: Point,
    /// The theme `build_layout` saw, so `apply_layout` can resolve the rows
    /// without waiting for a paint.
    layout_theme: Option<Theme>,
    scroll: BodyScroll,
    body_height: f64,
}

impl Default for CreateIdentityScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl CreateIdentityScreen {
    pub fn new() -> Self {
        // The first-run screen has one field; it starts focused so the
        // first keystroke is the name, as the modal prompt's field does.
        let mut name = TextInput::local()
            .with_id(NAME_INPUT_ID)
            .with_placeholder(NAME_PLACEHOLDER)
            .with_accessible_name(NAME_LABEL);
        name.set_focus(true);
        Self {
            rect: Rect::ZERO,
            name,
            name_node: None,
            last_name: String::new(),
            selected_accent: 0,
            submit_hovered: false,
            submit_rect: Rect::ZERO,
            tile_rects: Vec::new(),
            input_rect: Rect::ZERO,
            submit_error: None,
            last_record: None,
            last_cursor: Point::ZERO,
            layout_theme: None,
            scroll: BodyScroll::default(),
            body_height: 0.0,
        }
    }

    /// Live display-name input — useful for tests.
    pub fn display_name(&self) -> &str {
        self.name.value()
    }

    pub fn set_display_name(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.name.set_text(&name);
        self.name.set_cursor_pos(name.len());
        self.last_name = name;
        self.submit_error = None;
    }

    pub fn selected_accent_color(&self) -> [u8; 3] {
        ACCENT_PALETTE
            .get(self.selected_accent)
            .copied()
            .unwrap_or(ACCENT_PALETTE[0])
    }

    pub fn set_accent_index(&mut self, idx: usize) {
        if idx < ACCENT_PALETTE.len() {
            self.selected_accent = idx;
        }
    }

    pub fn is_submit_enabled(&self) -> bool {
        !self.name.value().trim().is_empty()
    }

    pub fn last_record(&self) -> Option<&IdentityRecord> {
        self.last_record.as_ref()
    }

    pub fn submit_error(&self) -> Option<&str> {
        self.submit_error.as_deref()
    }

    /// On success the new `IdentityRecord` is cached and returned. On
    /// error `submit_error` is set so the UI can surface it inline.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if the display name is
    /// empty (whitespace-only counts as empty), or propagates whatever
    /// [`ThresholdIdentity::bootstrap_single_device`],
    /// [`IdentityRecord::from_threshold_identity`] or `store.save` fail
    /// with.
    pub fn submit_to_store(
        &mut self,
        store: &dyn IdentityStore,
    ) -> Result<IdentityRecord, IdentityFlowError> {
        if !self.is_submit_enabled() {
            return Err(IdentityFlowError::DecodeFailed(
                "display name must not be empty".to_string(),
            ));
        }
        let mut rng = UnwrapErr(SysRng);
        let id = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
        let record = IdentityRecord::from_threshold_identity(
            &id,
            self.name.value().trim(),
            self.selected_accent_color(),
            crate::time::wall(),
        )?;
        store.save(&record)?;
        self.last_record = Some(record.clone());
        self.submit_error = None;
        Ok(record)
    }

    /// Returns `ScreenOutcome::Advance` only on a successful submit.
    // Mirrors `Widget::handle_event`'s parameter list on purpose --
    // this is invoked from the trait impl, forwarding the same event,
    // scale, modifiers, CRDT handle, and out-params the screen host
    // received; bundling it would mean bundling the trait method too.
    pub fn handle_event_screen(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
        store: &dyn IdentityStore,
    ) -> ScreenOutcome {
        let handled = &mut *ctx.handled;
        // The field answers for its own node, so a click in it moves the
        // host's focus to the field rather than to the screen.
        let field_node = self.name_node.unwrap_or(ctx.node_id);
        let mut forward = |name: &mut TextInput, handled: &mut bool| {
            name.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state: ctx.state,
                    path: ctx.path,
                    handled,
                    focus_target: ctx.focus_target,
                    node_id: field_node,
                    taffy: ctx.taffy,
                },
            );
        };
        // Set after the match, once `forward` has released `focus_target`:
        // a click in the field tells the host the field's node holds focus
        // even when the field already believed it did (it starts focused).
        let mut claim_focus = false;
        let outcome = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = pos;
                let hovering = self.submit_rect.contains(pos) && self.is_submit_enabled();
                if hovering != self.submit_hovered {
                    self.submit_hovered = hovering;
                    *handled = true;
                }
                forward(&mut self.name, handled);
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => match self.press(handled, store) {
                Some(outcome) => outcome,
                // The field, through the part of it inside the visible body:
                // a row scrolled under the action row takes no click.
                None if self.input_rect.contains(self.last_cursor) => {
                    self.name.set_focus(true);
                    forward(&mut self.name, handled);
                    claim_focus = true;
                    *handled = true;
                    ScreenOutcome::Stay
                }
                None => ScreenOutcome::Stay,
            },
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.scroll.key(logical_key, self.body_height) {
                    *handled = true;
                    return ScreenOutcome::Stay;
                }
                // Enter submits; everything else is the field's.
                if matches!(logical_key, Key::Named(NamedKey::Enter)) {
                    return self.submit_if_enabled(handled, store);
                }
                forward(&mut self.name, handled);
                ScreenOutcome::Stay
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.scroll.wheel(delta) {
                    *handled = true;
                }
                ScreenOutcome::Stay
            }
            // Key releases, IME preedit/commit, modifiers: the field's.
            _ => {
                forward(&mut self.name, handled);
                ScreenOutcome::Stay
            }
        };
        if claim_focus {
            *ctx.focus_target = Some(field_node);
        }
        // An edit clears a stale submit error, as retyping the name did; a
        // caret or selection move alone does not.
        if edited(&mut self.name, &mut self.last_name) {
            self.submit_error = None;
        }
        outcome
    }

    /// A left press on an accent tile or the submit button; `None` when it
    /// landed on neither, so the caller can offer it to the field.
    fn press(&mut self, handled: &mut bool, store: &dyn IdentityStore) -> Option<ScreenOutcome> {
        if let Some(i) = self
            .tile_rects
            .iter()
            .position(|r| r.contains(self.last_cursor))
        {
            self.selected_accent = i;
            *handled = true;
            return Some(ScreenOutcome::Stay);
        }
        if self.submit_rect.contains(self.last_cursor) && self.is_submit_enabled() {
            return Some(self.submit_if_enabled(handled, store));
        }
        None
    }

    /// Submit through the store when the name allows it: `Advance` on
    /// success, `Stay` with the error surfaced inline otherwise.
    fn submit_if_enabled(
        &mut self,
        handled: &mut bool,
        store: &dyn IdentityStore,
    ) -> ScreenOutcome {
        if !self.is_submit_enabled() {
            return ScreenOutcome::Stay;
        }
        *handled = true;
        match self.submit_to_store(store) {
            Ok(_) => ScreenOutcome::Advance,
            Err(e) => {
                self.submit_error = Some(format!("{e}"));
                ScreenOutcome::Stay
            }
        }
    }

    /// One geometry pass for both `apply_layout` and `draw`: the rects the
    /// screen paints are the rects it hit-tests, clipped to the visible body
    /// so a scrolled-out row can never take a button's click.
    fn resolve(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) -> Resolved<Plan> {
        let actions = self.actions();
        let spec = SheetSpec {
            title: TITLE,
            max_width: FORM_SHEET_WIDTH,
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
        // The widget takes the whole row (the body clip trims its paint);
        // its pointer target is the part inside the visible body.
        place_field(&mut self.name, resolved.plan.input);
        self.input_rect = BodyScroll::visible(body, resolved.plan.input);
        self.tile_rects = resolved
            .plan
            .tiles
            .iter()
            .map(|r| BodyScroll::visible(body, *r))
            .collect();
        self.submit_rect = resolved.sheet.action(0);
        resolved
    }

    fn actions(&self) -> [ModalAction<'static>; 1] {
        [ModalAction::primary(SUBMIT_LABEL)
            .enabled(self.is_submit_enabled())
            .hovered(self.submit_hovered)]
    }

    /// Walk the body rows once. Related rows (a label and its field) sit
    /// `spacing.s1` apart; groups are separated by `spacing.s6`.
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
            EXPLANATION,
            width,
        ));
        rows.gap(sp.s6);
        let name_label = rows.row(line_height(text_renderer, theme, Role::Label));
        rows.gap(sp.s1);
        let input = rows.row(control_h);
        rows.gap(sp.s6);
        let accent_label = rows.row(line_height(text_renderer, theme, Role::Label));
        rows.gap(sp.s2);
        // Tiles line up under their label rather than centring on the card
        // while the label stays left — one reading edge for the group.
        let tile = f64::from(sp.s8);
        let tile_gap = f64::from(sp.s2);
        let tile_row = rows.row(tile);
        let tiles = (0..ACCENT_PALETTE.len())
            .map(|i| {
                let x = tile_row.x0 + small_f64(i) * (tile + tile_gap);
                Rect::new(x, tile_row.y0, x + tile, tile_row.y1)
            })
            .collect();
        let error = self.submit_error.as_deref().map(|err| {
            rows.gap(sp.s4);
            rows.row(paragraph_height(
                text_renderer,
                theme,
                Role::Error,
                err,
                width,
            ))
        });
        Plan {
            explanation,
            name_label,
            input,
            accent_label,
            tiles,
            error,
            bottom: rows.bottom(),
        }
    }

    /// The accent-colour tiles: `radii.control` swatches, the chosen one ringed
    /// in the card's ink a hairline outside its edge.
    fn draw_accent_tiles(&self, scene: &mut Scene, transform: Affine, theme: &Theme, plan: &Plan) {
        let radius = f64::from(theme.tokens.radii.control);
        let ring_gap = f64::from(theme.tokens.spacing.s1) / 2.0;
        for (i, r) in plan.tiles.iter().enumerate() {
            // `tiles` is built 1:1 with `ACCENT_PALETTE`, so this is
            // structurally in range — checked rather than indexed so the
            // invariant can't silently regress into a panic.
            let Some(&[r_, g_, b_]) = ACCENT_PALETTE.get(i) else {
                continue;
            };
            let fill = Color::from_rgb8(r_, g_, b_);
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &RoundedRect::from_rect(*r, radius),
            );
            if i == self.selected_accent {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    Role::Value.color(theme),
                    None,
                    &RoundedRect::from_rect(r.inflate(ring_gap, ring_gap), radius + ring_gap),
                );
            }
        }
    }
}

impl Widget for CreateIdentityScreen {
    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = Some(theme.clone());
        // The field is a child node of its own — placed by the sheet, not
        // by taffy (`Display::None`), but present in the tree so the host's
        // focus order and the accessibility tree line up with `children()`.
        let field = taffy.leaf_node(Style {
            display: Display::None,
            ..Default::default()
        });
        node_map.insert(field, std::ptr::from_mut::<dyn Widget>(&mut self.name));
        self.name_node = Some(field);
        // The screen owns the viewport, so the sheet centres on the window
        // rather than inside a fixed box at the origin.
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[field],
        );
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
        // Hit rects are ready before the first paint and after every
        // state change that reshapes the body.
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
        presence: &PresenceCtx,
    ) {
        let resolved = self.resolve(theme, text_renderer);
        let actions = self.actions();
        let spec = SheetSpec {
            title: TITLE,
            max_width: FORM_SHEET_WIDTH,
            actions: &actions,
        };
        let sheet = &resolved.sheet;
        let plan = &resolved.plan;

        sheet.paint_frame(scene, transform, text_renderer, theme, &spec);
        sheet.clip_body(scene, transform);
        paint_text(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            plan.explanation,
            EXPLANATION,
            Role::Body,
            Align::Start,
            true,
        );
        paint_text(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            plan.name_label,
            NAME_LABEL,
            Role::Label,
            Align::Start,
            false,
        );
        self.name
            .draw(scene, transform, text_renderer, theme, presence);
        paint_text(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            plan.accent_label,
            ACCENT_LABEL,
            Role::Label,
            Align::Start,
            false,
        );
        self.draw_accent_tiles(scene, transform, theme, plan);
        if let (Some(rect), Some(err)) = (plan.error, self.submit_error.as_deref()) {
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                err,
                Role::Error,
                Align::Start,
                true,
            );
        }
        scene.pop_layer();
        sheet.paint_actions(scene, transform, text_renderer, theme, &spec);
    }

    fn name(&self) -> &'static str {
        "CreateIdentityScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_create")
    }

    /// The field is the screen's one child, in the order `build_layout`
    /// minted its node — the contract every focus and accessibility walk
    /// zips against.
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.name]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.name]
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.name)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Widget::ime_purpose(&self.name)
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
    use crate::widget::identity_flow::MemoryIdentityStore;
    use automerge::AutoCommit;

    #[test]
    fn create_identity_submit_validates_name_and_stores_record() {
        fn submit_disabled_until_name_provided() {
            let s = CreateIdentityScreen::new();
            assert!(!s.is_submit_enabled(), "empty name must disable submit");
        }

        fn submit_disabled_for_whitespace_only_name() {
            let mut s = CreateIdentityScreen::new();
            s.set_display_name("   \t  ");
            assert!(
                !s.is_submit_enabled(),
                "whitespace must be treated as empty"
            );
        }

        fn submit_to_store_creates_record() {
            let mut s = CreateIdentityScreen::new();
            s.set_display_name("alice");
            s.set_accent_index(2);
            let store = MemoryIdentityStore::new();
            let rec = s.submit_to_store(&store).expect("submit must succeed");
            assert_eq!(rec.display_name, "alice");
            assert_eq!(rec.avatar_color, ACCENT_PALETTE[2]);
            // Persisted into the store.
            let loaded = store.load().unwrap().unwrap();
            assert_eq!(loaded.display_name, "alice");
            // Cached on the screen for the container to pick up.
            assert!(s.last_record().is_some());
        }

        fn submit_with_empty_name_errors() {
            let mut s = CreateIdentityScreen::new();
            let store = MemoryIdentityStore::new();
            let err = s.submit_to_store(&store).unwrap_err();
            assert!(matches!(err, IdentityFlowError::DecodeFailed(_)));
            assert!(s.last_record().is_none());
        }

        fn accent_index_clamps_to_palette_bound() {
            let mut s = CreateIdentityScreen::new();
            s.set_accent_index(99);
            // Out-of-range index leaves the previous selection in place.
            assert_eq!(s.selected_accent, 0);
            s.set_accent_index(3);
            assert_eq!(s.selected_accent, 3);
        }

        submit_disabled_until_name_provided();
        submit_disabled_for_whitespace_only_name();
        submit_to_store_creates_record();
        submit_with_empty_name_errors();
        accent_index_clamps_to_palette_bound();
    }

    #[test]
    fn create_identity_layout_and_shared_field_input() {
        /// The rows the screen paints are the rows it hit-tests, laid on the
        /// spacing scale: label → field one `s1` apart, groups `s6` apart, the
        /// tiles under their label rather than centred on the card.
        fn plan_stacks_rows_on_the_spacing_scale_under_one_reading_edge() {
            let mut s = CreateIdentityScreen::new();
            s.submit_error = Some("boom".to_string());
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let body = Rect::new(100.0, 200.0, 620.0, 200.0);
            let plan = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            let sp = &theme.tokens.spacing;
            assert_eq!(plan.explanation.y0, body.y0);
            assert!((plan.name_label.y0 - plan.explanation.y1 - f64::from(sp.s6)).abs() < 1e-9);
            assert!((plan.input.y0 - plan.name_label.y1 - f64::from(sp.s1)).abs() < 1e-9);
            assert_eq!(plan.input.height(), 34.0);
            assert_eq!(plan.input.x0, body.x0);
            assert_eq!(plan.input.x1, body.x1);
            assert!((plan.accent_label.y0 - plan.input.y1 - f64::from(sp.s6)).abs() < 1e-9);
            let first = plan.tiles.first().expect("eight tiles");
            assert_eq!(
                first.x0, plan.accent_label.x0,
                "tiles start on the label's edge"
            );
            assert_eq!(first.width(), f64::from(sp.s8));
            for pair in plan.tiles.windows(2) {
                assert!((pair[1].x0 - pair[0].x1 - f64::from(sp.s2)).abs() < 1e-9);
            }
            let error = plan.error.expect("an error row when there is an error");
            assert!(error.y0 > first.y1);
            assert!((plan.bottom - error.y1).abs() < 1e-9);
        }

        /// The name field is the shared input, embedded: a click in its row
        /// focuses it and hands the host its own node, typed keys land in its
        /// value (and enable submit), Enter submits through the store, and the
        /// field is the screen's addressable child.
        fn clicking_and_typing_drive_the_shared_field_and_enter_submits() {
            use crate::widget::identity_flow::test_events as ev;
            let theme = Theme::dark();
            let store = MemoryIdentityStore::new();
            let mut s = CreateIdentityScreen::new();
            s.name.set_focus(false);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map = HashMap::new();
            let node = s.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(1280.0),
                        height: taffy::AvailableSpace::Definite(800.0),
                    },
                )
                .expect("layout");
            s.apply_layout(&taffy, node, Point::ORIGIN);
            let field_node = s.name_node.expect("the field has a node");
            assert!(
                node_map.contains_key(&field_node),
                "registered for the host"
            );
            assert_eq!(
                s.children()
                    .iter()
                    .filter_map(|c| c.test_id())
                    .collect::<Vec<_>>(),
                vec![NAME_INPUT_ID]
            );

            let mut doc = AutoCommit::new();
            let mut send = |s: &mut CreateIdentityScreen, e: &WindowEvent| {
                let mut handled = false;
                let mut target = None;
                let outcome = s.handle_event_screen(
                    e,
                    &mut crate::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &winit::event::Modifiers::default(),
                        state: &mut doc,
                        path: &automerge::ROOT,
                        handled: &mut handled,
                        focus_target: &mut target,
                        node_id: node,
                        taffy: &taffy,
                    },
                    &store,
                );
                (outcome, target)
            };
            let centre = s.input_rect.center();
            send(&mut s, &ev::cursor_at(centre));
            let (_, target) = send(&mut s, &ev::left_press());
            assert!(s.name.is_focused(), "a click in the row focuses the field");
            assert_eq!(
                target,
                Some(field_node),
                "and hands the host the field's node"
            );
            assert!(!s.is_submit_enabled());
            send(&mut s, &ev::char("B"));
            send(&mut s, &ev::char("o"));
            assert_eq!(
                s.display_name(),
                "Bo",
                "typed keys land in the shared field"
            );
            assert!(s.is_submit_enabled());
            let (outcome, _) = send(&mut s, &ev::named(NamedKey::Enter));
            assert!(matches!(outcome, ScreenOutcome::Advance), "Enter submits");
            assert_eq!(s.last_record().map(|r| r.display_name.as_str()), Some("Bo"));
        }

        /// The pointer targets exist after the layout pass alone — a click
        /// before the first paint hits the same rects the paint will draw.
        fn hit_rects_are_resolved_by_apply_layout_without_a_paint() {
            let theme = Theme::dark();
            let mut s = CreateIdentityScreen::new();
            s.set_display_name("Alice");
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map = HashMap::new();
            let node = s.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(1280.0),
                        height: taffy::AvailableSpace::Definite(800.0),
                    },
                )
                .expect("layout");
            s.apply_layout(&taffy, node, Point::ORIGIN);
            assert!(s.submit_rect.area() > 0.0, "the submit button is a target");
            assert!(s.input_rect.area() > 0.0, "the name field is a target");
            assert_eq!(s.tile_rects.len(), ACCENT_PALETTE.len());
            assert!(
                s.input_rect.y1 < s.submit_rect.y0,
                "the field sits above the action row"
            );
        }

        plan_stacks_rows_on_the_spacing_scale_under_one_reading_edge();
        clicking_and_typing_drive_the_shared_field_and_enter_submits();
        hit_rects_are_resolved_by_apply_layout_without_a_paint();
    }
}
