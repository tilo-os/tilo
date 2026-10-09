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
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_rich_text::{FieldOutcome, RichTextField, RichTextSource, RichTextSurface, SubmitKey};
use ui::TextRole;
use ui::framework::Dispatcher;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    BlockEditor, Button, ButtonFace, ButtonRole, ChipFace, ChipLead, ColorPicker,
    ColorPickerOutcome, ColorSwatch, ControlSize, CursorPlacement, NumberInput, Widget,
};

use crate::model::{Annotation, FontEntry, Settings};
use crate::reader_theme::ReaderPalette;
use crate::resource::Ebook;

/// The SDK editor a note or comment is written in, bound to the note's rich
/// text: each keystroke is an [`EbookAction::EditNoteText`] the library's
/// dispatcher applies, undone a word at a time. Enter asks to finish,
/// Shift+Enter breaks the line and Escape cancels.
///
/// [`EbookAction::EditNoteText`]: crate::actions::EbookAction::EditNoteText
pub(crate) struct NoteEditor {
    id: Uuid,
    host: tilo_rich_text::RichTextComposer<RichTextField>,
    /// The caret has been placed at the end of the note, once it was read.
    placed: bool,
}

impl NoteEditor {
    pub(crate) fn new(id: Uuid, hint: &str, library: Option<&Dispatcher<Ebook>>) -> Self {
        let mut field =
            RichTextField::view_only(move |doc| crate::doc::BooksDoc::annotation_text(doc, id));
        if let Some(library) = library {
            field.attach(crate::actions::note_text_dispatcher(library, id));
        }
        let mut surface = RichTextSurface::field(field)
            .with_editor(
                BlockEditor::new()
                    .with_wrap_lines(true)
                    .with_compact_paragraph_spacing(true)
                    .with_block_handles(false),
            )
            .with_placeholder(hint)
            .with_submit(SubmitKey::Enter);
        surface.set_focus(true);
        Self {
            id,
            host: tilo_rich_text::RichTextComposer::new(surface),
            placed: false,
        }
    }

    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    /// Read the note from the document, placing the caret at its end the
    /// first time.
    pub(crate) fn sync(&mut self, state: &AutoCommit, root: &ObjId) {
        self.host.sync(state, root);
        if !self.placed && self.host.widget().source().revision().is_some() {
            self.placed = true;
            self.host
                .widget_mut()
                .editor_mut()
                .focus_block(usize::MAX, CursorPlacement::End);
        }
    }

    /// Route one event to the editor; returns whether the person finished
    /// (Enter) or cancelled (Escape).
    pub(crate) fn handle_event(&mut self, ctx: &mut InputEventCtx<'_>) -> Option<FieldOutcome> {
        self.sync(ctx.state.doc(), ctx.root);
        self.host.handle_event(
            ctx.event,
            ctx.scale_factor,
            &ctx.modifiers,
            &mut ctx.state,
            ctx.root,
            ctx.handled,
        );
        self.sync(ctx.state.doc(), ctx.root);
        self.host.widget_mut().take_outcome()
    }

    pub(crate) fn layout(&mut self, rect: Rect, theme: &Theme, text: &mut TextRenderer) -> f64 {
        self.host.layout(rect, theme, text)
    }

    pub(crate) fn rect(&self) -> Rect {
        self.host.rect()
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.host.draw(
            scene,
            transform,
            text,
            theme,
            &ui::presence::PresenceCtx::empty(),
        );
    }

    pub(crate) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.host.widget().accessibility()
    }

    /// An assistive technology's action on the text (focus, set value).
    pub(crate) fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool {
        self.sync(state, root);
        self.host.widget_mut().perform_action(action, state, root)
    }
}

/// Native event services shared by the reader's retained input widgets.
///
/// Keeping this contract at the toolkit boundary prevents each component from
/// mirroring the complete [`Widget::handle_event`] argument list.
pub(crate) struct InputEventCtx<'a> {
    pub event: &'a WindowEvent,
    pub scale_factor: f64,
    pub modifiers: Modifiers,
    pub state: ui::widget::Edits<'a>,
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
            state: self.state.reborrow(),
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
                    f64::from(Settings::READING_SCALE_MIN) * 100.0,
                    f64::from(Settings::READING_SCALE_MAX) * 100.0,
                )
                .with_step(10.0)
                .with_precision(0)
                .with_suffix("%")
                .with_id("ebook_text_size")
                .compact()
                .control_size(ControlSize::Small),
            Self::LineSpacing => NumberInput::new(value)
                .with_range(
                    f64::from(Settings::SPACING_MIN),
                    f64::from(Settings::SPACING_MAX),
                )
                .with_step(0.1)
                .with_precision(2)
                .with_id("ebook_line_spacing")
                .compact()
                .control_size(ControlSize::Small),
            // A font's size against the reading size, as a percentage.
            Self::FontMult(id) => NumberInput::new(value)
                .with_range(
                    f64::from(FontEntry::MULT_MIN) * 100.0,
                    f64::from(FontEntry::MULT_MAX) * 100.0,
                )
                .with_step(5.0)
                .with_precision(0)
                .with_suffix("%")
                .with_id(&format!("ebook_font_multiplier_{id}"))
                .compact()
                .control_size(ControlSize::Small),
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
        state: &mut ui::widget::Edits<'_>,
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

    /// The width `field` needs to show every value it can hold beside its
    /// steppers, as the shared input measures it.
    pub(crate) fn preferred_width(
        &self,
        field: &NumberField,
        value: f64,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        match self.controls.iter().find(|control| control.field == *field) {
            Some(control) => control.input.preferred_width(theme, text_renderer),
            None => field.input(value).preferred_width(theme, text_renderer),
        }
    }

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
                &mut ui::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: &ctx.modifiers,
                    state: ctx.state.reborrow(),
                    path: ctx.root,
                    handled: &mut local_handled,
                    focus_target: &mut focus_target,
                    node_id: ctx.node_id,
                    taffy: ctx.taffy,
                },
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
    Save,
    Delete(Uuid),
    Focused,
}

/// On-page note editor: the SDK rich-text editor bound to the note, in a
/// card with Delete and Save.
///
/// The component owns geometry, drawing and hit-testing; the editor writes
/// the note as the person types.
pub(crate) struct NoteComposer {
    editor: Option<NoteEditor>,
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
    excerpt_y: f64,
    input_rect: Rect,
    delete: Rect,
    save: Rect,
}

impl Default for NoteComposer {
    fn default() -> Self {
        Self {
            editor: None,
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
        let Some(editor) = self.editor.as_ref() else {
            return Vec::new();
        };
        let id = editor.id();
        let mut input = editor
            .accessibility()
            .unwrap_or_else(|| AccessNode::new(Role::MultilineTextInput));
        input.name = Some("Note text".into());
        input = input
            .with_bounds(editor.rect())
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
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) -> Option<NoteSemanticOutcome> {
        let editor = self.editor.as_mut()?;
        let id = editor.id();
        if target == format!("books.note.input:{id}") {
            return editor
                .perform_action(action, state, root)
                .then_some(NoteSemanticOutcome::Focused);
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

    /// Open the editor on note `id`; it shows the note on the next sync.
    pub(crate) fn open(&mut self, id: Uuid, library: Option<&Dispatcher<Ebook>>) {
        self.editor = Some(NoteEditor::new(id, "Write a note", library));
    }

    pub(crate) fn close(&mut self) {
        self.editor = None;
        self.card = Rect::ZERO;
        self.delete = Rect::ZERO;
        self.save = Rect::ZERO;
    }

    pub(crate) fn id(&self) -> Option<Uuid> {
        self.editor.as_ref().map(NoteEditor::id)
    }

    pub(crate) fn is_open(&self) -> bool {
        self.editor.is_some()
    }

    /// Bring the open note's text up to date with the document.
    pub(crate) fn sync(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(editor) = self.editor.as_mut() {
            editor.sync(state, root);
        }
    }

    pub(crate) fn hit_test(&self, point: Point) -> Option<NoteAction> {
        let id = self.id()?;
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

    /// Whether the person finished the note (Enter or Escape).
    pub(crate) fn handle_event(&mut self, ctx: &mut InputEventCtx<'_>) -> bool {
        let Some(editor) = self.editor.as_mut() else {
            return false;
        };
        if let WindowEvent::CursorMoved { position, .. } = ctx.event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        editor.handle_event(ctx).is_some()
    }

    /// Geometry for the card, its header, excerpt, input box and buttons,
    /// computed once and shared by the drawing helpers.
    fn compute_note_layout(
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
        anchor_y: Option<f64>,
        annotation: &Annotation,
    ) -> NoteLayout {
        let spacing = &theme.tokens.spacing;
        let rhythm = &spacing.rhythm;
        let margin = f64::from(spacing.s3);
        let width =
            f64::from(theme.tokens.metrics.panel_width_utility).min(bounds.width() - 2.0 * margin);
        let pad = f64::from(rhythm.section);
        let inner_width = ui::num::f32_of_f64((width - pad * 2.0).max(1.0));
        let excerpt = if annotation.excerpt.trim().is_empty() {
            "Selected passage".to_string()
        } else {
            format!("\u{201C}{}\u{201D}", annotation.excerpt)
        };
        let excerpt_style = theme.text(TextRole::Body).with_italic(true);
        let (_, excerpt_height) =
            text_renderer.measure_style(&excerpt, Some(inner_width), &excerpt_style);
        let header = ChipFace::height(theme);
        let input_height = 3.0 * f64::from(theme.text(TextRole::Body).line_height_px())
            + 2.0 * f64::from(spacing.s2);
        let button_h = ControlSize::Regular.height(theme);
        let gap = f64::from(rhythm.related);
        let height = pad
            + header
            + gap
            + f64::from(excerpt_height)
            + gap
            + input_height
            + f64::from(rhythm.section)
            + button_h
            + pad;
        let center = f64::midpoint(bounds.x0, bounds.x1);
        let x0 = (center - width / 2.0).clamp(
            bounds.x0 + margin,
            (bounds.x1 - margin - width).max(bounds.x0 + margin),
        );
        let top_limit = bounds.y0 + margin;
        let bottom_limit = bounds.y1 - margin;
        let offset = f64::from(rhythm.group);
        let y0 = match anchor_y {
            Some(y) if y + offset + height <= bottom_limit => y + offset,
            Some(y) if y - gap - height >= top_limit => y - gap - height,
            _ => (bottom_limit - height).max(top_limit),
        };
        let card = Rect::new(x0, y0, x0 + width, y0 + height);
        let left = card.x0 + pad;
        let excerpt_y = card.y0 + pad + header + gap;
        let input_y = excerpt_y + f64::from(excerpt_height) + gap;
        let input_rect = Rect::new(left, input_y, card.x1 - pad, input_y + input_height);
        let button_y = input_rect.y1 + f64::from(rhythm.section);
        let save_w = ButtonFace::new(Rect::ZERO, "Save")
            .role(ButtonRole::Primary)
            .intrinsic_width(text_renderer, theme);
        let delete_w = ButtonFace::new(Rect::ZERO, "Delete note")
            .role(ButtonRole::Destructive)
            .intrinsic_width(text_renderer, theme);
        NoteLayout {
            card,
            left,
            pad,
            inner_width,
            excerpt,
            excerpt_y,
            input_rect,
            delete: Rect::new(left, button_y, left + delete_w, button_y + button_h),
            save: Rect::new(
                card.x1 - pad - save_w,
                button_y,
                card.x1 - pad,
                button_y + button_h,
            ),
        }
    }

    /// The floating card, the kind chip, the excerpt and the (already laid
    /// out) shared text input.
    fn draw_note_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &NoteLayout,
        annotation: &Annotation,
    ) {
        ui::widget::floating_surface::paint(scene, transform, layout.card, theme);
        let on_panel = theme.on_ground(ui::chrome::panel_ground(theme));
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, &on_panel);
        let y = layout.card.y0 + layout.pad;
        let lead = ChipLead::Dot(ReaderPalette::highlight_swatch(annotation.color));
        let chip_w = ChipFace::intrinsic_width("Note", lead, false, ctx.text, &on_panel);
        let chip = Rect::new(
            layout.left,
            y,
            layout.left + chip_w,
            y + ChipFace::height(&on_panel),
        );
        ChipFace::new(chip, "Note")
            .lead(lead)
            .paint(ctx.scene, ctx.transform, ctx.text, &on_panel);
        let style = on_panel.text(TextRole::Body).with_italic(true);
        ctx.text(
            &layout.excerpt,
            &style,
            on_panel.text_dim,
            Point::new(layout.left, layout.excerpt_y),
            Some(layout.inner_width),
        );

        if let Some(editor) = self.editor.as_mut() {
            editor.layout(layout.input_rect, &on_panel, text_renderer);
            editor.draw(scene, transform, text_renderer, &on_panel);
        }
    }

    /// Delete and Save under the input box.
    fn draw_note_buttons(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &NoteLayout,
    ) {
        self.delete = layout.delete;
        self.save = layout.save;
        let on_panel = theme.on_ground(ui::chrome::panel_ground(theme));
        Button::paint(
            scene,
            transform,
            text_renderer,
            &on_panel,
            &ButtonFace::new(self.delete, "Delete note")
                .role(ButtonRole::Destructive)
                .hovered(self.delete.contains(self.cursor)),
        );
        Button::paint(
            scene,
            transform,
            text_renderer,
            &on_panel,
            &ButtonFace::new(self.save, "Save")
                .role(ButtonRole::Primary)
                .hovered(self.save.contains(self.cursor)),
        );
    }

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
        if self.id() != Some(annotation.id) {
            return;
        }
        let layout = Self::compute_note_layout(text_renderer, theme, bounds, anchor_y, annotation);
        self.card = layout.card;
        self.draw_note_chrome(scene, transform, text_renderer, theme, &layout, annotation);
        self.draw_note_buttons(scene, transform, text_renderer, theme, &layout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::TaffyTree;
    use winit::event::ElementState;
    use winit::keyboard::{Key, NamedKey};

    fn key(logical_key: Key) -> WindowEvent {
        let text = match &logical_key {
            Key::Character(text) => Some(text.clone()),
            _ => None,
        };
        let event = ui::synthetic_input::key_event(logical_key, text, ElementState::Pressed);
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    /// The note composer writes the note as it is typed and Enter finishes; hit testing keeps the card modal and tap-away saves.
    #[test]
    fn note_composer_edits_through_shared_input_and_keeps_its_card_modal() {
        // --- phase: note_composer_writes_the_note_as_typed_and_enter_finishes ---
        {
            let mut doc = AutoCommit::new();
            let note = crate::model::Annotation::new("book:x", crate::model::AnnoKind::Note, 0, 0)
                .with_note("hello");
            let id = note.id;
            crate::doc::BooksDoc::upsert_annotation(&mut doc, &note);
            doc.commit();
            let library = crate::resource::library_dispatcher_for_test();
            let mut composer = NoteComposer::default();
            composer.open(id, Some(&library));
            let root = automerge::ROOT;
            composer.sync(&doc, &root);
            let taffy = TaffyTree::new();
            let modifiers = Modifiers::default();
            let press = |composer: &mut NoteComposer, doc: &mut AutoCommit, key: WindowEvent| {
                let mut handled = false;
                composer.handle_event(&mut InputEventCtx {
                    event: &key,
                    scale_factor: 1.0,
                    modifiers,
                    state: ui::widget::Edits::lend(doc),
                    root: &root,
                    node_id: NodeId::new(0),
                    taffy: &taffy,
                    handled: &mut handled,
                })
            };
            for ch in ["w", "o", "r", "l", "d"] {
                assert!(!press(
                    &mut composer,
                    &mut doc,
                    key(Key::Character(ch.into()))
                ));
            }
            let stored = |doc: &AutoCommit| {
                crate::doc::BooksDoc::list_annotations(doc)
                    .into_iter()
                    .find(|annotation| annotation.id == id)
                    .unwrap()
                    .note
            };
            assert_eq!(
                stored(&doc),
                "helloworld",
                "typed at the end, written as typed"
            );
            assert!(press(
                &mut composer,
                &mut doc,
                key(Key::Named(NamedKey::Enter))
            ));
            assert_eq!(
                stored(&doc),
                "helloworld",
                "Enter finishes, it does not break"
            );
        }

        // --- phase: note_hit_testing_keeps_card_modal_and_tap_away_saves ---
        {
            let id = Uuid::new_v4();
            let mut composer = NoteComposer::default();
            composer.open(id, Some(&crate::resource::library_dispatcher_for_test()));
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
}

/// What a scheme colour pick edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickTarget {
    /// A new scheme derived from the chosen background.
    NewScheme,
    /// One colour slot (page, text, accent) of an existing scheme.
    Slot(String, usize),
}

/// A confirmed scheme colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColorEdit {
    pub(crate) target: PickTarget,
    pub(crate) rgb: [u8; 3],
}

/// What the colour sheet did with an event or semantic action it claimed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ColorSheetResponse {
    /// The sheet consumed it without confirming a colour.
    Consumed,
    /// The sheet closed with this confirmed edit.
    Confirmed(ColorEdit),
}

impl ColorSheetResponse {
    fn from_edit(edit: Option<ColorEdit>) -> Self {
        edit.map_or(Self::Consumed, Self::Confirmed)
    }
}

/// Quick picks offered under the picker: saturated accents, then paper and
/// ink tones for page and text colours.
const SCHEME_SWATCHES: [([u8; 3], &str); 10] = [
    ([0xEF, 0x44, 0x44], "Red"),
    ([0xF5, 0x9E, 0x0B], "Amber"),
    ([0xFA, 0xCC, 0x15], "Yellow"),
    ([0x10, 0xB9, 0x81], "Green"),
    ([0x3B, 0x82, 0xF6], "Blue"),
    ([0xA7, 0x8B, 0xFA], "Violet"),
    ([0xF6, 0xEC, 0xD8], "Cream"),
    ([0xFC, 0xFC, 0xFA], "Paper white"),
    ([0x2A, 0x2A, 0x2E], "Charcoal"),
    ([0x14, 0x18, 0x22], "Ink"),
];

/// The modal sheet the colour-scheme manager picks colours in: the toolkit's
/// [`ColorPicker`] in confirm mode, with the scheme swatches, over a scrim.
///
/// The picker owns HSV state, dragging, swatches, *Cancel*/*Done*, keyboard
/// and semantics; this adapter owns only the sheet geometry and maps the
/// picker's outcome onto the scheme slot it edits.
#[derive(Default)]
pub(crate) struct SchemeColorSheet {
    open: Option<(PickTarget, ColorPicker)>,
    panel: Rect,
    cursor: Point,
}

impl SchemeColorSheet {
    fn title(target: &PickTarget) -> &'static str {
        match target {
            PickTarget::NewScheme => "Page colour for the new scheme",
            PickTarget::Slot(_, 0) => "Page colour",
            PickTarget::Slot(_, 1) => "Text colour",
            PickTarget::Slot(..) => "Accent colour",
        }
    }

    pub(crate) fn open(&mut self, target: PickTarget, rgb: [u8; 3]) {
        let picker = ColorPicker::new(Color::from_rgb8(rgb[0], rgb[1], rgb[2]))
            .with_swatches(
                SCHEME_SWATCHES
                    .iter()
                    .map(|(c, name)| ColorSwatch::named(Color::from_rgb8(c[0], c[1], c[2]), *name)),
            )
            .confirming(Self::title(&target));
        self.open = Some((target, picker));
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Close without committing anything.
    pub(crate) fn cancel(&mut self) {
        self.open = None;
    }

    /// Map the picker's outcome onto the edit it confirms, closing the sheet
    /// on either outcome. `None` while the picker has no outcome or when it
    /// was cancelled.
    fn resolve(&mut self) -> Option<ColorEdit> {
        let outcome = self.open.as_mut()?.1.take_outcome()?;
        let (target, _) = self.open.take()?;
        match outcome {
            ColorPickerOutcome::Confirmed(color) => Some(ColorEdit {
                target,
                rgb: ui::color::to_rgb8(color),
            }),
            ColorPickerOutcome::Cancelled => None,
        }
    }

    /// The sheet as one modal dialog around the picker's own semantics.
    pub(crate) fn accessibility_nodes(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let Some((_, picker)) = self.open.as_ref() else {
            return Vec::new();
        };
        vec![
            AccessNode::new(Role::Dialog)
                .with_name("Choose colour")
                .with_bounds(self.panel)
                .with_state(AccessState::new().modal(true))
                .with_children(picker.accessibility().into_iter().collect()),
        ]
    }

    /// Route a semantic action published by the picker. `None` when the
    /// target is not the sheet's; otherwise whether the action closed the
    /// sheet with a confirmed edit.
    pub(crate) fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) -> Option<ColorSheetResponse> {
        let (_, picker) = self.open.as_mut()?;
        if !target.starts_with("color_picker") {
            return None;
        }
        if !picker.perform_semantic_action(target, action, state, root) {
            return None;
        }
        Some(ColorSheetResponse::from_edit(self.resolve()))
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        let Some((_, picker)) = self.open.as_mut() else {
            return;
        };
        let pad = f64::from(theme.tokens.spacing.s6);
        let max_width = f64::from(theme.tokens.metrics.panel_width_utility)
            .max(picker.preferred_size().0)
            + 2.0 * pad;
        let spec = ui::widget::modal_dialog::ModalSpec::new("")
            .closable(false)
            .max_width(max_width / f64::from(theme.text_scale));
        let width = ui::widget::modal_dialog::body_width(viewport, spec.max_width, theme);
        let spec = spec.body_height(picker.preferred_height(width));
        let layout = ui::widget::modal_dialog::layout(viewport, &spec, text_renderer, theme);
        self.panel = layout.card;
        ui::widget::modal_dialog::paint_backdrop(
            scene,
            transform,
            text_renderer,
            theme,
            &layout,
            &spec,
        );
        picker.apply_layout_external(layout.body);
        picker.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::presence::PresenceCtx::empty(),
        );
    }

    /// While open the sheet is modal: every pointer and key event is its.
    /// A press on the scrim outside the panel cancels; `Escape` cancels and
    /// `Enter` confirms. Returns `None` when closed (the event is not the
    /// sheet's), else whether this event produced a confirmed edit.
    pub(crate) fn handle_event(
        &mut self,
        ctx: &mut InputEventCtx<'_>,
    ) -> Option<ColorSheetResponse> {
        let (_, picker) = self.open.as_mut()?;
        let closing = match ctx.event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !self.panel.contains(self.cursor) && !picker.wants_pointer_capture() => {
                picker.cancel();
                true
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(key @ (NamedKey::Escape | NamedKey::Enter)),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if *key == NamedKey::Enter {
                    picker.confirm();
                } else {
                    picker.cancel();
                }
                true
            }
            _ => false,
        };
        if !closing {
            let mut focus_target = None;
            let mut local_handled = false;
            picker.handle_event(
                ctx.event,
                &mut ui::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: &ctx.modifiers,
                    state: ctx.state.reborrow(),
                    path: ctx.root,
                    handled: &mut local_handled,
                    focus_target: &mut focus_target,
                    node_id: ctx.node_id,
                    taffy: ctx.taffy,
                },
            );
            if focus_target.is_some() {
                picker.set_focus(true);
            }
        }
        *ctx.handled = true;
        Some(ColorSheetResponse::from_edit(self.resolve()))
    }

    #[cfg(test)]
    pub(crate) fn picker(&self) -> Option<&ColorPicker> {
        self.open.as_ref().map(|(_, picker)| picker)
    }
}
