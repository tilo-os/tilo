//! Screen 2b — recovery-share verification.
//!
//! After screen 2 shows the 24 words, screen 2b challenges the user
//! to type three of them back. Three positions are picked at random
//! when the screen is shown for the first time; the user types each
//! word into the matching slot; the "Finish setup" button activates
//! when all three slots match (case-insensitive, whitespace
//! trimmed).
//!
//! The challenge re-seeds when the user clicks "Pick different blanks" —
//! lets them retry without losing earlier progress.
//!
//! Each blank is the shared [`TextInput`], embedded in its grid cell: the
//! sheet places it, the screen forwards it the events it does not claim
//! (Enter checks, PageUp/PageDown scroll), and every blank hangs off the
//! screen's layout node as a child of its own — in reading order, so the
//! host's Tab walks the grid by ascending word position and a screen
//! reader hears "Word 12" rather than "text input".

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::ScreenOutcome;
use crate::widget::identity_flow::sheet::{
    Align, BodyScroll, GRID_SHEET_WIDTH, Resolved, Role, Rows, SheetSpec, WordGrid, edited,
    field_leaf, paint_text, paragraph_height, place_field, resolve, screen_style,
    with_layout_renderer,
};
use crate::widget::modal_dialog::ModalAction;
use crate::widget::text_input::TextInput;
use tilo_identity_ceremony::RecoveryChallenge;

const TITLE: &str = "Confirm your recovery words";
const EXPLANATION: &str =
    "Compare with your written list. Fill in the highlighted blanks to confirm.";
const MISMATCH: &str = "One or more words didn't match. Try again or pick new words.";
const RESEED_LABEL: &str = "Pick different blanks";
const FINISH_LABEL: &str = "Finish setup";

/// Action-row indices, in authoring order.
const ACTION_RESEED: usize = 0;
const ACTION_FINISH: usize = 1;

/// The mnemonic is 24 words; the grid never shows more.
const WORD_COUNT: usize = 24;

/// `test_id`s of the three blanks, in challenge-slot (storage) order.
pub const BLANK_INPUT_IDS: [&str; 3] = [
    "identity_recovery_blank_0",
    "identity_recovery_blank_1",
    "identity_recovery_blank_2",
];

/// One field per slot: named by the position it asks for, so a screen
/// reader says "Word 12" rather than "text input", and addressable by its
/// slot's `test_id`.
fn blank_inputs(challenge: Option<&RecoveryChallenge>) -> [TextInput; 3] {
    std::array::from_fn(|slot| {
        let id = BLANK_INPUT_IDS
            .get(slot)
            .copied()
            .unwrap_or("identity_recovery_blank");
        let name = challenge
            .and_then(|c| c.positions.get(slot))
            .map_or_else(|| "Recovery word".to_string(), |pos| format!("Word {pos}"));
        TextInput::local().with_id(id).with_accessible_name(name)
    })
}

/// Every rect the screen paints and hit-tests, from one pass over the rows.
#[derive(Clone, Debug, Default)]
struct Plan {
    explanation: Rect,
    grid: Rect,
    /// One editable cell per challenge slot, in storage order.
    slots: [Rect; 3],
    error: Option<Rect>,
    bottom: f64,
}

/// Screen 2b: type-3-words verification.
pub struct RecoveryVerifyScreen {
    rect: Rect,
    mnemonic: String,
    challenge: Option<RecoveryChallenge>,
    /// One shared text input per slot, in storage order, so the user gets
    /// the whole editing kit — IME, clipboard, selection, accessibility.
    editors: [TextInput; 3],
    /// Each slot's own layout node, minted by `build_layout`.
    slot_nodes: [Option<NodeId>; 3],
    /// Each blank's text as last seen, so an edit is a change of text and
    /// a caret move is not (see [`edited`]).
    last_answers: [String; 3],
    /// Whether the last submit produced a mismatch — drives the inline
    /// error message.
    last_attempt_failed: bool,
    slot_rects: [Rect; 3],
    continue_rect: Rect,
    reseed_rect: Rect,
    last_cursor: Point,
    /// The theme `build_layout` saw, so `apply_layout` can resolve the rows
    /// without waiting for a paint.
    layout_theme: Option<Theme>,
    scroll: BodyScroll,
    body_height: f64,
}

impl Default for RecoveryVerifyScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryVerifyScreen {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            mnemonic: String::new(),
            challenge: None,
            editors: blank_inputs(None),
            slot_nodes: [None; 3],
            last_answers: [const { String::new() }; 3],
            last_attempt_failed: false,
            slot_rects: [Rect::ZERO; 3],
            continue_rect: Rect::ZERO,
            reseed_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            layout_theme: None,
            scroll: BodyScroll::default(),
            body_height: 0.0,
        }
    }

    /// Inject the mnemonic the verify step quizzes against; resets the challenge.
    pub fn seed_mnemonic(&mut self, mnemonic: String) {
        self.mnemonic = mnemonic;
        self.challenge = RecoveryChallenge::from_mnemonic(&self.mnemonic).ok();
        self.reset_blanks();
    }

    pub fn challenge(&self) -> Option<&RecoveryChallenge> {
        self.challenge.as_ref()
    }

    /// Pick a fresh 3-word challenge.
    pub fn reseed(&mut self) {
        self.challenge = RecoveryChallenge::from_mnemonic(&self.mnemonic).ok();
        self.reset_blanks();
    }

    /// Inject a deterministic challenge, for reproducible test ordering.
    pub fn seed_challenge(&mut self, positions: [usize; 3]) {
        self.challenge = RecoveryChallenge::at_positions(&self.mnemonic, positions).ok();
        self.reset_blanks();
    }

    /// Fresh, empty blanks named for the current challenge's positions.
    /// Keyboard focus stays on the slot that held it, so "Pick different
    /// blanks" leaves the user typing into the same blank — the widgets
    /// are replaced in place, so the host's focused node still names it.
    fn reset_blanks(&mut self) {
        let focused = self.focused_slot();
        self.editors = blank_inputs(self.challenge.as_ref());
        if let Some(editor) = focused.and_then(|slot| self.editors.get_mut(slot)) {
            editor.set_focus(true);
        }
        self.last_answers = [const { String::new() }; 3];
        self.last_attempt_failed = false;
    }

    pub fn set_answer(&mut self, slot: usize, value: impl Into<String>) {
        if let (Some(editor), Some(last)) =
            (self.editors.get_mut(slot), self.last_answers.get_mut(slot))
        {
            let value = value.into();
            editor.set_text(&value);
            editor.set_cursor_pos(value.len());
            *last = value;
            self.last_attempt_failed = false;
        }
    }

    pub fn answer(&self, slot: usize) -> Option<&str> {
        self.editors.get(slot).map(TextInput::value)
    }

    pub fn last_attempt_failed(&self) -> bool {
        self.last_attempt_failed
    }

    /// True when all three slots match the challenge.
    pub fn is_correct(&self) -> bool {
        match &self.challenge {
            None => false,
            Some(c) => {
                let [r0, r1, r2] = &self.editors;
                c.check(&[r0.value(), r1.value(), r2.value()])
            }
        }
    }

    /// The slot whose field holds keyboard focus, if any.
    fn focused_slot(&self) -> Option<usize> {
        self.editors.iter().position(Widget::is_focused)
    }

    /// Slots by ascending word position — the order the eye reads the
    /// grid, the order `children()` exposes them in, and so the order the
    /// host's Tab walks them.
    fn visual_order(&self) -> [usize; 3] {
        let mut order = [0, 1, 2];
        if let Some(c) = &self.challenge {
            order.sort_by_key(|&slot| c.positions.get(slot).copied().unwrap_or(usize::MAX));
        }
        order
    }

    /// Move focus to `slot`: its field takes it, the others drop it, and
    /// the host learns which node now holds it.
    fn focus_slot(&mut self, slot: usize, focus_target: &mut Option<NodeId>) {
        for (i, editor) in self.editors.iter_mut().enumerate() {
            editor.set_focus(i == slot);
        }
        if let Some(node) = self.slot_nodes.get(slot).copied().flatten() {
            *focus_target = Some(node);
        }
    }

    /// Hand `event` to every blank, each answering for its own node. Each
    /// field gates itself — keys and IME act only on the focused one, a
    /// press only on the hovered one — while a release, a right-click or a
    /// pointer move reaches the field it belongs to even after focus has
    /// moved on, so a drag ends and a menu opens where the pointer is.
    fn forward_all(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        for slot in 0..self.editors.len() {
            let node = self
                .slot_nodes
                .get(slot)
                .copied()
                .flatten()
                .unwrap_or(ctx.node_id);
            if let Some(editor) = self.editors.get_mut(slot) {
                editor.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
        }
    }

    /// An edit in any blank — typed, pasted, IME-committed or set through
    /// an accessibility action — clears the mismatch line, as retyping did.
    /// A caret or selection move alone does not.
    fn note_edits(&mut self) {
        let mut changed = false;
        for (editor, last) in self.editors.iter_mut().zip(self.last_answers.iter_mut()) {
            changed |= edited(editor, last);
        }
        if changed {
            self.last_attempt_failed = false;
        }
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
        // What the screen claims for itself; `None` hands the event to the
        // fields. A press on a blank is both: the screen focuses it, the
        // field places its caret, the screen restates the focus.
        let (claimed, pressed_slot) = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                (None, None)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.left_press(ctx.handled, ctx.focus_target),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => (
                self.key_press(logical_key, *ctx.modifiers, ctx.handled, ctx.focus_target),
                None,
            ),
            WindowEvent::MouseWheel { delta, .. } => {
                if self.scroll.wheel(delta) {
                    *ctx.handled = true;
                }
                (Some(ScreenOutcome::Stay), None)
            }
            _ => (None, None),
        };
        let outcome = if let Some(outcome) = claimed {
            outcome
        } else {
            self.forward_all(event, ctx);
            if let Some(slot) = pressed_slot {
                self.focus_slot(slot, ctx.focus_target);
                *ctx.handled = true;
            }
            ScreenOutcome::Stay
        };
        self.note_edits();
        outcome
    }

    /// A left press: on a blank (focus it and report it, so the press then
    /// reaches the fields for caret placement), on an action, or on nothing.
    fn left_press(
        &mut self,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) -> (Option<ScreenOutcome>, Option<usize>) {
        if let Some(slot) = self
            .slot_rects
            .iter()
            .position(|r| r.contains(self.last_cursor))
        {
            self.focus_slot(slot, focus_target);
            return (None, Some(slot));
        }
        if self.reseed_rect.contains(self.last_cursor) {
            self.reseed();
            *handled = true;
            return (Some(ScreenOutcome::Stay), None);
        }
        if self.continue_rect.contains(self.last_cursor) {
            *handled = true;
            if self.is_correct() {
                return (Some(ScreenOutcome::Advance), None);
            }
            self.last_attempt_failed = true;
        }
        (Some(ScreenOutcome::Stay), None)
    }

    /// A key press the screen owns: PageUp/PageDown scroll the body; Enter
    /// checks; Tab, when a host without focus traversal lets it through,
    /// walks the blanks in reading order (a host with traversal never
    /// delivers it — `children()` gives it the same order). `None` for
    /// every other key, which is the focused blank's.
    fn key_press(
        &mut self,
        logical_key: &Key,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) -> Option<ScreenOutcome> {
        if self.scroll.key(logical_key, self.body_height) {
            *handled = true;
            return Some(ScreenOutcome::Stay);
        }
        match logical_key {
            Key::Named(NamedKey::Tab) => {
                if let (Some(slot), Some(c)) = (self.focused_slot(), self.challenge.as_ref()) {
                    let next = next_slot_in_visual_order(
                        slot,
                        &c.positions,
                        modifiers.state().shift_key(),
                    );
                    self.focus_slot(next, focus_target);
                    *handled = true;
                }
                Some(ScreenOutcome::Stay)
            }
            Key::Named(NamedKey::Enter) => {
                *handled = true;
                if self.is_correct() {
                    return Some(ScreenOutcome::Advance);
                }
                self.last_attempt_failed = true;
                Some(ScreenOutcome::Stay)
            }
            _ => None,
        }
    }

    /// One geometry pass for both `apply_layout` and `draw`; the blanks'
    /// targets are the parts of them inside the visible body.
    fn resolve(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) -> Resolved<Plan> {
        let actions = self.actions();
        let spec = SheetSpec {
            title: TITLE,
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
        // An accessibility `SetValue` lands outside the event path; the
        // layout pass that follows it is where the edit is noticed.
        self.note_edits();
        // Each blank takes its whole grid cell (the body clip trims its
        // paint); its pointer target is the part inside the visible body.
        for (editor, rect) in self.editors.iter_mut().zip(resolved.plan.slots.iter()) {
            place_field(editor, *rect);
        }
        for (slot, rect) in self.slot_rects.iter_mut().zip(resolved.plan.slots.iter()) {
            *slot = BodyScroll::visible(body, *rect);
        }
        self.reseed_rect = resolved.sheet.action(ACTION_RESEED);
        self.continue_rect = resolved.sheet.action(ACTION_FINISH);
        resolved
    }

    fn actions(&self) -> [ModalAction<'static>; 2] {
        [
            ModalAction::secondary(RESEED_LABEL)
                .hovered(self.reseed_rect.contains(self.last_cursor)),
            ModalAction::primary(FINISH_LABEL)
                .enabled(self.is_correct())
                .hovered(self.continue_rect.contains(self.last_cursor)),
        ]
    }

    /// Walk the body rows once: explanation, the same six control-tall
    /// grid rows the share screen lays out (so the two read as one surface
    /// with three blanks punched in), then the mismatch line if any.
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
        let grid = rows.row(WordGrid::height(WORD_COUNT, control_h));
        let cells = WordGrid::new(grid, control_h, text_renderer, theme);
        let mut slots = [Rect::ZERO; 3];
        if let Some(c) = &self.challenge {
            for (slot, pos1) in slots.iter_mut().zip(c.positions.iter()) {
                // Positions are one-based; the editable blank is the word
                // column of its cell.
                let (_, word) = cells.cell(pos1.saturating_sub(1), theme);
                *slot = word;
            }
        }
        let error = self.last_attempt_failed.then(|| {
            rows.gap(sp.s4);
            rows.row(paragraph_height(
                text_renderer,
                theme,
                Role::Error,
                MISMATCH,
                width,
            ))
        });
        Plan {
            explanation,
            grid,
            slots,
            error,
            bottom: rows.bottom(),
        }
    }

    /// The grid: every cell numbered, twenty-one words read-only and the
    /// three challenged ones as editable fields in the same cells.
    fn draw_word_grid(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        plan: &Plan,
        control_h: f64,
    ) {
        let grid = WordGrid::new(plan.grid, control_h, text_renderer, theme);
        let positions = self.challenge.as_ref().map_or([0, 0, 0], |c| c.positions);
        let words: Vec<String> = self
            .mnemonic
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        for i in 0..WORD_COUNT {
            let (ordinal, cell) = grid.cell(i, theme);
            let pos1 = i.saturating_add(1);
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                ordinal,
                &format!("{pos1}."),
                Role::Label,
                Align::Start,
                false,
            );
            match positions.iter().position(|&p| p == pos1) {
                Some(slot) => {
                    self.draw_blank_slot(scene, transform, text_renderer, theme, presence, slot);
                }
                None => paint_text(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    cell,
                    words.get(i).map_or("", String::as_str),
                    Role::Value,
                    Align::Start,
                    false,
                ),
            }
        }
    }

    /// The verdict edge a blank wears when it is not being edited: accent
    /// once it matches, danger after a failed attempt while it still holds
    /// a wrong word, nothing otherwise.
    fn verdict(&self, slot: usize, theme: &Theme) -> Option<Color> {
        let editor = self.editors.get(slot)?;
        if editor.is_focused() {
            return None;
        }
        let answer = editor.value();
        let matches = self
            .challenge
            .as_ref()
            .is_some_and(|c| answer.trim().eq_ignore_ascii_case(&c.expected_word(slot)));
        if matches {
            Some(theme.tokens.colors.intent.inverted)
        } else if self.last_attempt_failed && !answer.is_empty() {
            Some(theme.tokens.colors.semantic.danger)
        } else {
            None
        }
    }

    /// The editable blank at `slot`: the shared field, seated in its grid
    /// cell by `resolve` (the hit target is the visible part of it), plus
    /// its verdict edge.
    fn draw_blank_slot(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        slot: usize,
    ) {
        let verdict = self.verdict(slot, theme);
        let Some(editor) = self.editors.get_mut(slot) else {
            return;
        };
        editor.draw(scene, transform, text_renderer, theme, presence);
        if let Some(edge) = verdict {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                edge,
                None,
                &RoundedRect::from_rect(editor.get_rect(), f64::from(theme.tokens.radii.control)),
            );
        }
    }
}

/// Given `slot` and the challenge's `positions` array (storage order, NOT
/// visual order), return the slot index Tab should land on — the slot
/// whose position number is next-larger (Tab) or next-smaller
/// (Shift+Tab), wrapping at the ends.
///
/// Example: positions = [13, 12, 23], slot = 0 (position 13). Visual
/// order is [12, 13, 23] → Tab goes to 23 (slot 2), Shift+Tab to 12 (slot 1).
pub fn next_slot_in_visual_order(slot: usize, positions: &[usize; 3], backward: bool) -> usize {
    let Some(&current_pos) = positions.get(slot) else {
        return slot;
    };
    if backward {
        // Largest position smaller than current; wrap to largest.
        positions
            .iter()
            .enumerate()
            .filter(|(_, p)| **p < current_pos)
            .max_by_key(|(_, p)| **p)
            .map(|(i, _)| i)
            .or_else(|| {
                positions
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, p)| **p)
                    .map(|(i, _)| i)
            })
            .unwrap_or(slot)
    } else {
        // Smallest position larger than current; wrap to smallest.
        positions
            .iter()
            .enumerate()
            .filter(|(_, p)| **p > current_pos)
            .min_by_key(|(_, p)| **p)
            .map(|(i, _)| i)
            .or_else(|| {
                positions
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, p)| **p)
                    .map(|(i, _)| i)
            })
            .unwrap_or(slot)
    }
}

impl Widget for RecoveryVerifyScreen {
    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = Some(theme.clone());
        // One child node per blank, in reading order, so the host's Tab
        // walks the grid the way the eye does; placed by the sheet, not by
        // taffy (`Display::None`), but present so focus and accessibility
        // walks line up with `children()`.
        let order = self.visual_order();
        let mut leaves = Vec::with_capacity(order.len());
        for slot in order {
            let leaf = field_leaf(taffy);
            if let Some(editor) = self.editors.get_mut(slot) {
                node_map.insert(leaf, std::ptr::from_mut::<dyn Widget>(editor));
            }
            if let Some(node) = self.slot_nodes.get_mut(slot) {
                *node = Some(leaf);
            }
            leaves.push(leaf);
        }
        taffy.widget_node(screen_style(), &leaves, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
        // Identity ceremonies, invites and joins are security surfaces:
        // every look-alike letter is set apart.
        text_renderer.security_surface(|text_renderer| {
            let resolved = self.resolve(theme, text_renderer);
            let actions = self.actions();
            let spec = SheetSpec {
                title: TITLE,
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
                EXPLANATION,
                Role::Body,
                Align::Start,
                true,
            );
            self.draw_word_grid(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                plan,
                control_h,
            );
            if let Some(rect) = plan.error {
                paint_text(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    rect,
                    MISMATCH,
                    Role::Error,
                    Align::Start,
                    true,
                );
            }
            scene.pop_layer();
            sheet.paint_actions(scene, transform, text_renderer, theme, &spec);
        });
    }

    fn name(&self) -> &'static str {
        "RecoveryVerifyScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_recovery_verify")
    }

    /// The blanks, in reading order — the order `build_layout` minted
    /// their nodes in, which every focus and accessibility walk zips against.
    fn children(&self) -> Vec<&dyn Widget> {
        self.visual_order()
            .iter()
            .filter_map(|&slot| self.editors.get(slot).map(|e| e as &dyn Widget))
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let order = self.visual_order();
        let mut slots: Vec<Option<&mut TextInput>> = self.editors.iter_mut().map(Some).collect();
        order
            .iter()
            .filter_map(|&slot| slots.get_mut(slot).and_then(Option::take))
            .map(|e| e as &mut (dyn Widget + '_))
            .collect()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        let editor = self.editors.get(self.focused_slot()?)?;
        Widget::ime_cursor_rect(editor)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        let editor = self.editors.get(self.focused_slot()?)?;
        Widget::ime_purpose(editor)
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

    fn fixture_mnemonic() -> String {
        "ability abandon able about above absent absorb abstract absurd \
         abuse access accident account accuse achieve acid acoustic acquire \
         across act action actor actress actual"
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn recovery_verify_blanks_walk_and_lay_out_in_reading_order() {
        fn tab_walks_slots_in_ascending_position_order_not_storage_order() {
            // Positions stored in whatever order Fisher-Yates picked
            // ([13, 12, 23], not ascending) — Tab must still walk visually.
            let positions = [13usize, 12, 23];

            assert_eq!(
                next_slot_in_visual_order(0, &positions, false),
                2,
                "Tab from pos 13 should land on pos 23",
            );
            assert_eq!(
                next_slot_in_visual_order(0, &positions, true),
                1,
                "Shift+Tab from pos 13 should land on pos 12",
            );
            assert_eq!(
                next_slot_in_visual_order(2, &positions, false),
                1,
                "Tab from largest pos should wrap to smallest",
            );
            assert_eq!(
                next_slot_in_visual_order(1, &positions, true),
                2,
                "Shift+Tab from smallest pos should wrap to largest",
            );
        }

        /// Tab reaches the blanks through the host's focus order, which is the
        /// `children()` order and the node order: ascending word position, not
        /// storage order. Each blank is named for the word it asks for.
        fn children_and_nodes_walk_the_blanks_in_reading_order() {
            let mut s = RecoveryVerifyScreen::new();
            s.seed_mnemonic(fixture_mnemonic());
            s.seed_challenge([13, 12, 23]);
            let ids: Vec<&str> = s.children().iter().filter_map(|c| c.test_id()).collect();
            assert_eq!(
                ids,
                vec![BLANK_INPUT_IDS[1], BLANK_INPUT_IDS[0], BLANK_INPUT_IDS[2]],
                "children walk positions 12, 13, 23"
            );
            let names: Vec<String> = s
                .children()
                .iter()
                .filter_map(|c| c.accessibility()?.name)
                .collect();
            assert_eq!(names, vec!["Word 12", "Word 13", "Word 23"]);

            let mut taffy = TaffyTree::<()>::new();
            let mut node_map = HashMap::new();
            let node = s.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            let leaves = taffy.children(node).unwrap();
            assert_eq!(leaves.len(), 3, "one node per blank");
            assert_eq!(
                leaves[0],
                s.slot_nodes[1].unwrap(),
                "first Tab stop is word 12"
            );
            assert_eq!(leaves[1], s.slot_nodes[0].unwrap());
            assert_eq!(leaves[2], s.slot_nodes[2].unwrap());
            for leaf in &leaves {
                assert!(
                    node_map.contains_key(leaf),
                    "every blank is registered for the host"
                );
            }
            assert!(
                s.children().iter().all(|c| c.is_focusable()),
                "every blank is a Tab stop"
            );
        }

        /// The blanks sit in the cells the share screen shows the same words
        /// in: one control tall, in the word column of their grid cell.
        fn plan_places_each_blank_in_its_words_grid_cell() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let body = Rect::new(100.0, 200.0, 740.0, 200.0);
            let mut s = RecoveryVerifyScreen::new();
            s.seed_mnemonic(fixture_mnemonic());
            s.seed_challenge([1, 12, 24]);
            let plan = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            assert!((plan.grid.height() - 6.0 * 34.0).abs() < 1e-9);
            let grid = WordGrid::new(plan.grid, 34.0, &mut tr, &theme);
            for (slot, pos1) in [1usize, 12, 24].iter().enumerate() {
                let (_, word) = grid.cell(pos1 - 1, &theme);
                assert_eq!(plan.slots[slot], word, "slot {slot} sits in cell {pos1}");
                assert_eq!(plan.slots[slot].height(), 34.0);
            }
            assert!(plan.slots[0].y0 < plan.slots[1].y0 && plan.slots[1].y0 < plan.slots[2].y0);
            assert!(plan.error.is_none());
            s.last_attempt_failed = true;
            let failed = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            assert!(failed.error.is_some(), "a failed attempt gets its own row");
        }

        tab_walks_slots_in_ascending_position_order_not_storage_order();
        children_and_nodes_walk_the_blanks_in_reading_order();
        plan_places_each_blank_in_its_words_grid_cell();
    }

    #[test]
    fn recovery_verify_challenge_answers_and_retry_state() {
        fn challenge_is_some_after_seed() {
            let mut s = RecoveryVerifyScreen::new();
            s.seed_mnemonic(fixture_mnemonic());
            assert!(s.challenge().is_some());
        }

        fn is_correct_true_for_matching_answers() {
            let mut s = RecoveryVerifyScreen::new();
            s.seed_mnemonic(fixture_mnemonic());
            s.seed_challenge([1, 12, 24]);
            let words: Vec<String> = fixture_mnemonic()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            s.set_answer(0, &words[0]);
            s.set_answer(1, &words[11]);
            s.set_answer(2, &words[23]);
            assert!(s.is_correct());
        }

        fn is_correct_false_for_wrong_word() {
            let mut s = RecoveryVerifyScreen::new();
            s.seed_mnemonic(fixture_mnemonic());
            s.seed_challenge([1, 12, 24]);
            s.set_answer(0, "nope");
            s.set_answer(1, "abstract");
            s.set_answer(2, "actual");
            assert!(!s.is_correct());
        }

        fn reseed_clears_answers_and_picks_new_challenge() {
            let mut s = RecoveryVerifyScreen::new();
            s.seed_mnemonic(fixture_mnemonic());
            s.set_answer(0, "stale");
            s.set_answer(1, "stale");
            s.set_answer(2, "stale");
            s.focus_slot(1, &mut None);
            s.reseed();
            for i in 0..3 {
                assert!(s.answer(i).unwrap().is_empty());
            }
            assert!(s.challenge().is_some());
            assert_eq!(
                s.focused_slot(),
                Some(1),
                "picking different blanks keeps the keyboard in the same slot"
            );
        }

        fn failed_attempt_flag_sticky_until_user_types() {
            let mut s = RecoveryVerifyScreen::new();
            s.seed_mnemonic(fixture_mnemonic());
            s.seed_challenge([1, 12, 24]);
            s.set_answer(0, "wrong");
            s.set_answer(1, "wrong");
            s.set_answer(2, "wrong");
            assert!(!s.is_correct());
            // Manually flip the flag the way the click handler would.
            s.last_attempt_failed = true;
            assert!(s.last_attempt_failed());
            // Editing any slot clears the flag.
            s.set_answer(0, "");
            assert!(!s.last_attempt_failed());
        }

        challenge_is_some_after_seed();
        is_correct_true_for_matching_answers();
        is_correct_false_for_wrong_word();
        reseed_clears_answers_and_picks_new_challenge();
        failed_attempt_flag_sticky_until_user_types();
    }
}
