//! Shared-input adapters for Books' immediate-mode reader surface.
//!
//! The reader still owns pagination and page chrome, but editable values do
//! not need a second text-editing implementation.  These components own the
//! shared widgets, their externally-computed geometry, event routing, and the
//! small domain signals consumed by `BooksApp`.

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, NumberInput, TextInput, Widget};

use crate::model::{Annotation, FontEntry, Settings};
use crate::paint;
use crate::reader_theme::ReaderPalette;

/// Native event services shared by the reader's retained input widgets.
///
/// Keeping this contract at the toolkit boundary prevents each component from
/// mirroring the complete [`Widget::handle_event`] argument list.
pub(crate) struct InputEventCtx<'a> {
    pub event: &'a WindowEvent,
    pub scale_factor: f64,
    pub modifiers: Modifiers,
    pub state: &'a mut AutoCommit,
    pub root: &'a ObjId,
    pub node_id: NodeId,
    pub taffy: &'a TaffyTree<()>,
    pub handled: &'a mut bool,
}

impl InputEventCtx<'_> {
    pub(crate) fn reborrow<'a>(&'a mut self, handled: &'a mut bool) -> InputEventCtx<'a> {
        InputEventCtx {
            event: self.event,
            scale_factor: self.scale_factor,
            modifiers: self.modifiers,
            state: self.state,
            root: self.root,
            node_id: self.node_id,
            taffy: self.taffy,
            handled,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum NumberField {
    FontScale,
    LineSpacing,
    FontMult(String),
}

impl NumberField {
    /// The widest strings this field can display, in the shared
    /// `NumberInput`'s own `"{value} {suffix}"` form, so a caller can size
    /// the value slot once from a measurement instead of a fixed width that
    /// clips wider digit runs. Two digit variants cover fonts whose widest
    /// digit is `0` and those where it is `8`.
    pub(crate) fn widest_displays(&self) -> [&'static str; 2] {
        match self {
            Self::FontScale => ["000 %", "888 %"],
            Self::LineSpacing => ["0.00", "8.88"],
            Self::FontMult(_) => ["0.00 ×", "8.88 ×"],
        }
    }

    fn semantic_target(&self) -> String {
        match self {
            Self::FontScale => "books.number.text-size".into(),
            Self::LineSpacing => "books.number.line-spacing".into(),
            Self::FontMult(id) => format!("books.number.font-scale:{id}"),
        }
    }

    fn accessible_name(&self) -> String {
        match self {
            Self::FontScale => "Text size".into(),
            Self::LineSpacing => "Line spacing".into(),
            Self::FontMult(id) => format!("Size adjustment for {id}"),
        }
    }

    fn input(&self, value: f64) -> NumberInput {
        match self {
            Self::FontScale => NumberInput::new(value)
                .with_range(
                    f64::from(Settings::FONT_MIN) * 100.0,
                    f64::from(Settings::FONT_MAX) * 100.0,
                )
                .with_step(10.0)
                .with_precision(0)
                .with_suffix("%")
                .with_id("ebook_text_size"),
            Self::LineSpacing => NumberInput::new(value)
                .with_range(
                    f64::from(Settings::SPACING_MIN),
                    f64::from(Settings::SPACING_MAX),
                )
                .with_step(0.1)
                .with_precision(2)
                .with_id("ebook_line_spacing"),
            Self::FontMult(id) => NumberInput::new(value)
                .with_range(
                    f64::from(FontEntry::MULT_MIN),
                    f64::from(FontEntry::MULT_MAX),
                )
                .with_step(0.05)
                .with_precision(2)
                .with_suffix("×")
                .with_id(&format!("ebook_font_multiplier_{id}")),
        }
    }
}

struct NumberControl {
    field: NumberField,
    input: NumberInput,
    active: bool,
}

/// Persistent shared numeric inputs used by the reading popover and font rows.
#[derive(Default)]
pub(crate) struct NumberControls {
    controls: Vec<NumberControl>,
}

impl NumberControls {
    pub(crate) fn accessibility_nodes(&self) -> Vec<ui::a11y::AccessNode> {
        self.controls
            .iter()
            .filter(|control| control.active)
            .filter_map(|control| {
                let mut node = control.input.accessibility()?;
                node.name = Some(control.field.accessible_name());
                Some(
                    node.with_bounds(control.input.get_rect())
                        .with_action_target(control.field.semantic_target()),
                )
            })
            .collect()
    }

    pub(crate) fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> Option<(NumberField, Option<f64>)> {
        let control = self
            .controls
            .iter_mut()
            .find(|control| control.active && control.field.semantic_target() == target)?;
        if !control.input.perform_action(action, state, root) {
            return None;
        }
        Some((control.field.clone(), control.input.take_changed()))
    }

    pub(crate) fn begin_frame(&mut self) {
        for control in &mut self.controls {
            control.active = false;
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The numeric-control painter takes field identity, current value, bounds, and the scene, transform, text renderer, and theme."
    )]
    pub(crate) fn draw(
        &mut self,
        field: &NumberField,
        value: f64,
        rect: Rect,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let index = self
            .controls
            .iter()
            .position(|control| control.field == *field)
            .unwrap_or_else(|| {
                self.controls.push(NumberControl {
                    input: field.input(value),
                    field: field.clone(),
                    active: false,
                });
                self.controls.len() - 1
            });
        let Some(control) = self.controls.get_mut(index) else {
            return;
        };
        control.active = true;
        if !control.input.is_editing() {
            control.input.set_value(value);
        }
        control.input.apply_layout_external(rect);
        control.input.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::presence::PresenceCtx::empty(),
        );
    }

    /// Route an event to controls painted in the current frame and return all
    /// committed values. The app remains responsible only for domain mapping.
    pub(crate) fn handle_event(&mut self, ctx: &mut InputEventCtx<'_>) -> Vec<(NumberField, f64)> {
        let mut changes = Vec::new();
        for control in self.controls.iter_mut().filter(|control| control.active) {
            let mut local_handled = false;
            let mut focus_target = None;
            control.input.handle_event(
                ctx.event,
                ctx.scale_factor,
                &ctx.modifiers,
                ctx.state,
                ctx.root,
                &mut local_handled,
                &mut focus_target,
                ctx.node_id,
                ctx.taffy,
            );
            if focus_target.is_some() {
                control.input.set_focus(true);
            }
            if let Some(value) = control.input.take_changed() {
                changes.push((control.field.clone(), value));
            }
            *ctx.handled |= local_handled;
        }
        changes
    }

    #[cfg(test)]
    pub(crate) fn rect(&self, field: &NumberField) -> Option<Rect> {
        self.controls
            .iter()
            .find(|control| control.active && &control.field == field)
            .map(|control| control.input.get_rect())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NoteAction {
    Swallow,
    Save,
    Delete(Uuid),
}

pub(crate) enum NoteSemanticOutcome {
    Changed(String),
    Save,
    Delete(Uuid),
    Focused,
}

/// On-page note editor backed by the toolkit's multiline `TextInput`.
///
/// The component owns text state, selection/clipboard/IME behavior, geometry,
/// drawing, and hit-testing. It emits content changes and semantic actions;
/// only CRDT persistence remains in the app domain layer.
pub(crate) struct NoteComposer {
    id: Option<Uuid>,
    input: TextInput,
    card: Rect,
    delete: Rect,
    save: Rect,
    cursor: Point,
}

/// Geometry computed once per `NoteComposer::draw` call and shared by its
/// chrome/button drawing helpers.
struct NoteLayout {
    card: Rect,
    left: f64,
    pad: f64,
    inner_width: f32,
    excerpt: String,
    input_rect: Rect,
}

impl Default for NoteComposer {
    fn default() -> Self {
        Self {
            id: None,
            input: TextInput::local()
                .with_id("ebook_note_composer")
                .with_font_size(13.5)
                .with_plain_chrome()
                .multiline(true)
                .with_enter_submits(true)
                .with_placeholder("Write a note…"),
            card: Rect::ZERO,
            delete: Rect::ZERO,
            save: Rect::ZERO,
            cursor: Point::ZERO,
        }
    }
}

impl NoteComposer {
    pub(crate) fn accessibility_nodes(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let Some(id) = self.id else {
            return Vec::new();
        };
        let mut input = self
            .input
            .accessibility()
            .unwrap_or_else(|| AccessNode::new(Role::MultilineTextInput));
        input.name = Some("Note text".into());
        input = input
            .with_bounds(self.input.get_rect())
            .with_action_target(format!("books.note.input:{id}"));
        let button = |name: &str, target: String, bounds: Rect| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_bounds(bounds)
                .with_state(AccessState::new().focusable(true))
                .with_action_target(target)
        };
        vec![
            AccessNode::new(Role::Dialog)
                .with_name("Edit note")
                .with_state(AccessState::new().modal(true))
                .with_children(vec![
                    input,
                    button(
                        "Delete note",
                        format!("books.note.delete:{id}"),
                        self.delete,
                    ),
                    button("Save note", format!("books.note.save:{id}"), self.save),
                ]),
        ]
    }

    pub(crate) fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> Option<NoteSemanticOutcome> {
        let id = self.id?;
        if target == format!("books.note.input:{id}") {
            if self.input.perform_action(action, state, root) {
                if let Some(text) = self.input.take_dirty() {
                    return Some(NoteSemanticOutcome::Changed(text));
                }
                return Some(NoteSemanticOutcome::Focused);
            }
            return None;
        }
        if !matches!(action, ui::a11y::Action::Activate) {
            return None;
        }
        if target == format!("books.note.save:{id}") {
            Some(NoteSemanticOutcome::Save)
        } else if target == format!("books.note.delete:{id}") {
            Some(NoteSemanticOutcome::Delete(id))
        } else {
            None
        }
    }

    pub(crate) fn open(&mut self, id: Uuid, text: &str) {
        self.id = Some(id);
        self.input.set_text(text);
        self.input.set_focus(true);
    }

    pub(crate) fn close(&mut self) {
        self.id = None;
        self.input.set_focus(false);
        self.card = Rect::ZERO;
        self.delete = Rect::ZERO;
        self.save = Rect::ZERO;
    }

    pub(crate) fn id(&self) -> Option<Uuid> {
        self.id
    }

    pub(crate) fn is_open(&self) -> bool {
        self.id.is_some()
    }

    pub(crate) fn hit_test(&self, point: Point) -> Option<NoteAction> {
        let id = self.id?;
        if self.delete.contains(point) {
            Some(NoteAction::Delete(id))
        } else if self.save.contains(point) {
            Some(NoteAction::Save)
        } else if self.card.contains(point) {
            Some(NoteAction::Swallow)
        } else {
            Some(NoteAction::Save)
        }
    }

    /// Returns a changed text value and whether Enter requested Save.
    pub(crate) fn handle_event(&mut self, ctx: &mut InputEventCtx<'_>) -> (Option<String>, bool) {
        if self.id.is_none() {
            return (None, false);
        }
        if let WindowEvent::CursorMoved { position, .. } = ctx.event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        let mut focus_target = None;
        self.input.handle_event(
            ctx.event,
            ctx.scale_factor,
            &ctx.modifiers,
            ctx.state,
            ctx.root,
            ctx.handled,
            &mut focus_target,
            ctx.node_id,
            ctx.taffy,
        );
        if focus_target.is_some() {
            self.input.set_focus(true);
        }
        (self.input.take_dirty(), self.input.take_submit())
    }

    /// Geometry for the card, header excerpt and input box — pure so it can
    /// be computed once and shared by the chrome/button drawing helpers.
    fn compute_note_layout(
        text_renderer: &mut TextRenderer,
        bounds: Rect,
        anchor_y: Option<f64>,
        annotation: &Annotation,
    ) -> NoteLayout {
        let width = 344.0_f64.min(bounds.width() - 24.0);
        let pad = 16.0;
        let inner_width = (width - pad * 2.0) as f32;
        let excerpt = if annotation.excerpt.trim().is_empty() {
            "Selected passage".to_string()
        } else {
            format!("“{}”", annotation.excerpt)
        };
        let (_, excerpt_height) = text_renderer.measure(&excerpt, 12.0, Some(inner_width));
        let input_height = 74.0;
        let height = 34.0 + f64::from(excerpt_height) + 10.0 + input_height + 16.0 + 32.0 + pad;
        let center = f64::midpoint(bounds.x0, bounds.x1);
        let x0 = (center - width / 2.0).clamp(
            bounds.x0 + 12.0,
            (bounds.x1 - 12.0 - width).max(bounds.x0 + 12.0),
        );
        let top_limit = bounds.y0 + 62.0;
        let bottom_limit = bounds.y1 - 16.0;
        let y0 = match anchor_y {
            Some(y) if y + 22.0 + height <= bottom_limit => y + 22.0,
            Some(y) if y - 14.0 - height >= top_limit => y - 14.0 - height,
            _ => (bottom_limit - height).max(top_limit),
        };
        let card = Rect::new(x0, y0, x0 + width, y0 + height);
        let left = card.x0 + pad;
        let input_y = card.y0 + pad + 26.0 + f64::from(excerpt_height) + 10.0;
        let input_rect = Rect::new(left, input_y, card.x1 - pad, input_y + input_height);
        NoteLayout {
            card,
            left,
            pad,
            inner_width,
            excerpt,
            input_rect,
        }
    }

    /// Shadow, card background/border, the "Note" header, excerpt and the
    /// (already laid-out) shared text input.

    fn draw_note_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &NoteLayout,
        annotation: &Annotation,
    ) {
        let colors = &theme.tokens.colors;
        // A floating Panel: elevation, ground and the derived Panel border.
        // The focus ring belongs to the text input inside, not to the card.
        crate::chrome::panel(scene, transform, theme, layout.card);

        let y = layout.card.y0 + layout.pad;
        paint::circle(
            scene,
            transform,
            Point::new(layout.left + 6.0, y + 7.0),
            6.0,
            ReaderPalette::highlight_swatch(annotation.color),
        );
        text_renderer.draw_text_weighted(
            scene,
            "Note",
            13.5,
            colors.text_primary,
            transform * Affine::translate((layout.left + 20.0, y)),
            None,
            TextWeight::Semibold,
        );
        let excerpt_y = y + 26.0;
        text_renderer.draw_text(
            scene,
            &layout.excerpt,
            12.0,
            colors.text_dim,
            transform * Affine::translate((layout.left, excerpt_y)),
            Some(layout.inner_width),
        );

        self.input.apply_layout_external(layout.input_rect);
        self.input.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::presence::PresenceCtx::empty(),
        );
    }

    /// Delete/Save buttons beneath the input box.
    fn draw_note_buttons(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &NoteLayout,
    ) {
        let button_y = layout.input_rect.y1 + 16.0;
        self.delete = Rect::new(layout.left, button_y, layout.left + 76.0, button_y + 32.0);
        self.save = Rect::new(
            layout.card.x1 - layout.pad - 84.0,
            button_y,
            layout.card.x1 - layout.pad,
            button_y + 32.0,
        );
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(self.delete, "Delete")
                .danger()
                .radius(16.0)
                .text_size(12.5)
                .hovered(self.delete.contains(self.cursor)),
        );
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(self.save, "Save")
                .primary()
                .radius(16.0)
                .text_size(13.0)
                .hovered(self.save.contains(self.cursor)),
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The note editor combines render context with viewport bounds, optional anchor position, and the annotation being edited."
    )]
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
        anchor_y: Option<f64>,
        annotation: &Annotation,
    ) {
        if self.id != Some(annotation.id) {
            return;
        }
        if !self.input.is_focused() && self.input.value() != annotation.note {
            self.input.set_text(&annotation.note);
        }
        let layout = Self::compute_note_layout(text_renderer, bounds, anchor_y, annotation);
        self.card = layout.card;
        self.draw_note_chrome(scene, transform, text_renderer, theme, &layout, annotation);
        self.draw_note_buttons(scene, transform, text_renderer, theme, &layout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::TaffyTree;
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{Key, NamedKey, PhysicalKey};

    fn key(logical_key: Key) -> WindowEvent {
        let text = match &logical_key {
            Key::Character(text) => Some(text.clone()),
            _ => None,
        };
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        event.logical_key = logical_key;
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = text;
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    #[test]
    fn note_composer_uses_shared_selection_editing_and_submit() {
        let mut composer = NoteComposer::default();
        composer.open(Uuid::new_v4(), "hello");
        composer.input.select_all();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let taffy = TaffyTree::new();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let (changed, submitted) = composer.handle_event(&mut InputEventCtx {
            event: &key(Key::Character("world".into())),
            scale_factor: 1.0,
            modifiers,
            state: &mut doc,
            root: &root,
            node_id: NodeId::new(0),
            taffy: &taffy,
            handled: &mut handled,
        });
        assert_eq!(changed.as_deref(), Some("world"));
        assert!(!submitted);
        let (_, submitted) = composer.handle_event(&mut InputEventCtx {
            event: &key(Key::Named(NamedKey::Enter)),
            scale_factor: 1.0,
            modifiers,
            state: &mut doc,
            root: &root,
            node_id: NodeId::new(0),
            taffy: &taffy,
            handled: &mut handled,
        });
        assert!(submitted);
    }

    #[test]
    fn note_hit_testing_keeps_card_modal_and_tap_away_saves() {
        let id = Uuid::new_v4();
        let mut composer = NoteComposer::default();
        composer.open(id, "");
        composer.card = Rect::new(10.0, 10.0, 200.0, 160.0);
        composer.delete = Rect::new(20.0, 110.0, 80.0, 140.0);
        composer.save = Rect::new(110.0, 110.0, 180.0, 140.0);
        assert_eq!(
            composer.hit_test(Point::new(40.0, 120.0)),
            Some(NoteAction::Delete(id))
        );
        assert_eq!(
            composer.hit_test(Point::new(140.0, 120.0)),
            Some(NoteAction::Save)
        );
        assert_eq!(
            composer.hit_test(Point::new(90.0, 60.0)),
            Some(NoteAction::Swallow)
        );
        assert_eq!(composer.hit_test(Point::ZERO), Some(NoteAction::Save));
    }
}
