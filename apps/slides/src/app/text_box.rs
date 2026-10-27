//! The selected text box's words on the canvas: the shared rich-text
//! editor embedded where the box is drawn, opened by the press that opens
//! the box, driven by mouse and finger, and laid out so the box grows to
//! its words.

use super::{
    AutoCommit, Chrome, DeckRecord, Dispatcher, Element, ElementKind, ElementState, Embedded,
    FieldOutcome, Mode, Modifiers, MouseButton, Palette, Point, PresenceCtx, Rect, RichTextField,
    RichTextSurface, RoutedEvent, SlidesApp, SplitLayout, SubmitKey, TextRenderer, Theme, Widget,
    WindowEvent, actions, render,
};
use ui::paint::PaintCtx;

/// The words of the selected text box on the canvas: the SDK editor,
/// embedded where the box is drawn and laid out at the slide's size, editing
/// the element's rich-text body, each edit a field edit of the deck at the
/// body's path. It is laid out while the box is only selected, so the press
/// that opens the box lands in its words; it draws and takes the keyboard
/// only while the person edits them.
/// The key of the words' node under their box's ([`SlidesApp::words_access_node`]).
pub(super) const WORDS_ACCESS_KEY: &str = "slides.words";

pub(super) struct TextBoxEditor {
    pub(super) slide_id: String,
    pub(super) element_id: String,
    pub(super) host: Embedded<RichTextSurface<RichTextField>>,
    /// The person is editing the words: the editor has the keyboard.
    pub(super) focused: bool,
    /// How tall the words came to when last laid out, in slide units;
    /// `None` before the first layout.
    pub(super) words_h: Option<f64>,
}

impl TextBoxEditor {
    pub(super) fn open(
        doc: &AutoCommit,
        slide_id: &str,
        element: &Element,
        deck: &Dispatcher<DeckRecord>,
    ) -> Self {
        let body = crate::deck::body_object(doc, slide_id, &element.id);
        let field = RichTextField::view_only(move |doc| {
            body.clone()
                .filter(|body| tilo_doc::model::open(doc, body).is_ok())
        })
        .with_presence(render::words_presence_key);
        let surface = RichTextSurface::field(field)
            .with_editor(render::text_box_editor(element))
            .with_submit(SubmitKey::ModEnter);
        let mut editor = Self {
            slide_id: slide_id.to_owned(),
            element_id: element.id.clone(),
            host: Embedded::new(surface),
            focused: false,
            words_h: None,
        };
        editor.attach(deck);
        editor.host.widget_mut().present_from(doc);
        editor
    }

    /// Edit through the deck's dispatcher.
    pub(super) fn attach(&mut self, deck: &Dispatcher<DeckRecord>) {
        let (slide, element) = (self.slide_id.clone(), self.element_id.clone());
        self.host.widget_mut().source_mut().attach(
            deck.child::<tilo_document::RichText>(&actions::slide_resource_id(&slide), move |op| {
                actions::edit_text_action(&slide, &element, op)
            }),
        );
    }

    /// Give the words the keyboard, the caret after the last of them (a
    /// press handed on next puts it where the person pressed).
    pub(super) fn focus(&mut self) {
        let surface = self.host.widget_mut();
        surface.set_focus(true);
        surface
            .editor_mut()
            .focus_block(usize::MAX, ui::widget::CursorPlacement::End);
        self.focused = true;
    }

    /// Done with the words: the box stays selected.
    pub(super) fn blur(&mut self) {
        self.host.widget_mut().set_focus(false);
        self.focused = false;
    }
}

impl SlidesApp {
    /// Start editing the selected text box's words; `select_all` selects
    /// them all once the editor opens.
    pub(super) fn begin_text_edit(&mut self, select_all: bool) {
        self.editing_text = true;
        self.editing_notes = false;
        self.text_edit_select_all = select_all;
    }

    /// Open the selected text box for editing when `event` would type into
    /// it: a printable key without a command modifier, or text from the
    /// input method. Its words are selected first, so the typing replaces
    /// them. The event then reaches the box's editor like any other key.
    pub(super) fn begin_typing_into_selection(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
    ) {
        if self.mode != Mode::Edit
            || self.editing_text
            || self.editing_notes
            || !self
                .selected_element()
                .is_some_and(|e| e.kind == ElementKind::Text)
        {
            return;
        }
        let m = modifiers.state();
        let types = match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                key.state == ElementState::Pressed
                    && !(m.control_key() || m.super_key() || m.alt_key())
                    && key
                        .text
                        .as_ref()
                        .is_some_and(|text| text.chars().any(|c| !c.is_control()))
            }
            WindowEvent::Ime(
                winit::event::Ime::Commit(text) | winit::event::Ime::Preedit(text, _),
            ) => !text.is_empty(),
            _ => false,
        };
        if types {
            self.begin_text_edit(true);
        }
    }

    /// Stop editing the text box: the box stays selected, its editor
    /// without the keyboard.
    pub(super) fn end_text_edit(&mut self) {
        self.editing_text = false;
        self.text_drag = false;
        self.opening_press = None;
        if let Some(editor) = self.text_editor.as_mut() {
            editor.blur();
        }
    }

    /// The selected text box and its slide, when one is selected in the
    /// editor.
    pub(super) fn selected_text_box(&self) -> Option<(String, Element)> {
        if self.mode != Mode::Edit {
            return None;
        }
        let slide = self.slide()?;
        let element = slide.element(self.sel.as_deref()?)?;
        (element.kind == ElementKind::Text).then(|| (slide.id.clone(), element.clone()))
    }

    /// The text box being edited and its slide, when there is one.
    pub(super) fn editing_text_box(&self) -> Option<(String, Element)> {
        if !self.editing_text {
            return None;
        }
        self.selected_text_box()
    }

    /// Open the editor of the selected text box (closing one of a box no
    /// longer selected), give it the keyboard while the box is being edited
    /// and take it back when editing ended, and bring it up to date with the
    /// document.
    pub(super) fn ensure_text_editor(&mut self, doc: &AutoCommit) {
        let Some((slide_id, element)) = self.selected_text_box() else {
            self.text_editor = None;
            self.text_drag = false;
            return;
        };
        let current = self
            .text_editor
            .as_ref()
            .is_some_and(|editor| editor.slide_id == slide_id && editor.element_id == element.id);
        if !current {
            self.text_drag = false;
            self.text_editor = Some(TextBoxEditor::open(
                doc,
                &slide_id,
                &element,
                &self.dispatch,
            ));
        }
        let editing = self.editing_text;
        let select_all = std::mem::take(&mut self.text_edit_select_all);
        if let Some(editor) = self.text_editor.as_mut() {
            if editing && !editor.focused {
                editor.focus();
            } else if !editing && editor.focused {
                editor.blur();
                self.text_drag = false;
            }
            editor.host.sync(doc, &automerge::ROOT);
            if editing && select_all {
                editor.host.widget_mut().editor_mut().select_all();
            }
        }
    }

    /// Hand the press that opened the selected text box to its editor, so
    /// the caret lands where the person pressed and a double-click selects
    /// the word there, as in any text field. A box opened by the same press
    /// that selected it has no layout yet: its caret stays after its words.
    pub(super) fn open_at_press(&mut self, routed: &mut RoutedEvent<'_, '_>) {
        let Some((at, presses)) = self.opening_press.take() else {
            return;
        };
        if self.editing_text_box().is_none() {
            return;
        }
        self.ensure_text_editor(routed.state);
        let Some(editor) = self.text_editor.as_mut() else {
            return;
        };
        if editor.words_h.is_none() {
            return;
        }
        let scale = routed.scale_factor;
        let mut events = vec![ui::synthetic_input::cursor_moved(
            at.x * scale,
            at.y * scale,
        )];
        for press in 0..presses {
            if press > 0 {
                events.push(ui::synthetic_input::mouse_release());
            }
            events.push(ui::synthetic_input::mouse_press());
        }
        // Each replayed event is its own: one consumed press must not
        // swallow the release and second press that make a double-click.
        for event in &events {
            let mut consumed = false;
            editor.host.handle_event(
                event,
                scale,
                &Modifiers::default(),
                routed.state,
                routed.path,
                &mut consumed,
            );
        }
        editor.host.sync(routed.state.doc(), routed.path);
        // The press is still held: a drag before its release selects words.
        self.text_drag = true;
    }

    /// Give the text box being edited its keyboard and pointer events.
    /// Returns whether the event was the box's.
    pub(super) fn route_text_edit(
        &mut self,
        routed: &mut RoutedEvent<'_, '_>,
        handled: &mut bool,
    ) -> bool {
        if self.editing_text_box().is_none() {
            return false;
        }
        let keyboard = matches!(
            routed.event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        );
        // The words' menu, while open, takes every pointer event.
        let menu_open = self.words_menu_open();
        let pointer = menu_open
            && matches!(
                routed.event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
            )
            || match routed.event {
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left | MouseButton::Right,
                    ..
                } => self.text_edit_box.contains(self.last_mouse),
                WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => self.text_drag,
                _ => false,
            };
        // The pointer's position always reaches the box, so a press lands
        // where it was last seen.
        let moved = matches!(routed.event, WindowEvent::CursorMoved { .. });
        if !keyboard && !pointer && !moved {
            return false;
        }
        self.ensure_text_editor(routed.state);
        let Some(editor) = self.text_editor.as_mut() else {
            return false;
        };
        let mut consumed = false;
        editor.host.handle_event(
            routed.event,
            routed.scale_factor,
            &routed.modifiers,
            routed.state,
            routed.path,
            &mut consumed,
        );
        editor.host.sync(routed.state.doc(), routed.path);
        let outcome = editor.host.widget_mut().take_outcome();
        match routed.event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !menu_open => self.text_drag = true,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => self.text_drag = false,
            _ => {}
        }
        if outcome.is_some() {
            // Escape or Ctrl+Enter: done with the words, the box stays selected.
            debug_assert!(matches!(
                outcome,
                Some(FieldOutcome::Submit | FieldOutcome::Cancel)
            ));
            self.end_text_edit();
            consumed = true;
        }
        if keyboard || pointer {
            // Keys the editor left alone (global undo) fall through to the host.
            *handled |= consumed || pointer;
            return true;
        }
        false
    }

    /// Whether the words being edited have their menu open: then it is
    /// modal, and takes every pointer and key event.
    pub(super) fn words_menu_open(&self) -> bool {
        self.editing_text_box().is_some()
            && self
                .text_editor
                .as_ref()
                .is_some_and(|editor| editor.host.modal_capture().is_some())
    }

    /// Whether `event` is the words' to take ahead of the canvas's own
    /// menus: a secondary press on the words being edited opens their
    /// menu (cut, copy, paste, formatting) rather than the box's, and an
    /// open words' menu takes every pointer event.
    pub(super) fn words_take_menu_event(&self, event: &WindowEvent) -> bool {
        let secondary_press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            }
        );
        self.editing_text_box().is_some()
            && (self.words_menu_open()
                || (secondary_press && self.text_edit_box.contains(self.last_mouse)))
    }

    /// Paint what the words being edited float over everything (their
    /// menu), above the canvas, at the reader's own scale.
    pub(super) fn draw_words_overlay(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::vello::kurbo::Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.editing_text {
            return;
        }
        if let Some(editor) = self.text_editor.as_mut() {
            editor
                .host
                .draw_overlay(scene, transform, text, theme, presence);
        }
    }

    /// Lay the selected text box's editor out at the slide's size, where
    /// the box is drawn, in the ink the box's words are drawn in: the words
    /// it shows while they are edited, and the geometry the press that
    /// opens the box lands in. Called before the canvas is painted, so the
    /// box's frame follows the words of the same frame.
    pub(super) fn layout_text_editor(
        &mut self,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
        pal: &Palette,
    ) {
        let Some((_, element)) = self.selected_text_box() else {
            return;
        };
        let Some(ink) = self
            .slide()
            .map(|slide| render::text_ink(&pal.on(slide.background), slide, &element))
        else {
            return;
        };
        // The words lay out against the deck's look, as the canvas draws
        // them, the same while they are edited.
        let words_theme = render::words_theme(pal.look, theme);
        let theme = &*words_theme;
        let Some(editor) = self
            .text_editor
            .as_mut()
            .filter(|editor| editor.element_id == element.id)
        else {
            return;
        };
        let surface = editor.host.widget_mut();
        surface.editor_mut().set_body_size(element.font_size as f32);
        surface
            .editor_mut()
            .set_text_align(render::text_align(element.align));
        editor.host.set_text_color(Some(ink));
        let words_h = editor
            .host
            .layout(render::text_box_rect(&element), theme, tr);
        editor.host.set_transform(render::text_box_transform(
            chrome.slide_dest,
            chrome.scale,
            &element,
            words_h,
        ));
        editor.words_h = Some(words_h);
        self.text_edit_box = self.element_frame(chrome.slide_dest, chrome.scale, &element);
    }

    /// Paint the words of the text box being edited, through its editor.
    pub(super) fn draw_text_editor(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = ctx;
        let (t, theme) = (*t, *theme);
        if !self.editing_text {
            return;
        }
        let Some((_, element)) = self.selected_text_box() else {
            return;
        };
        let words_theme = render::words_theme(self.deck.look, theme);
        let theme = &*words_theme;
        if let Some(editor) = self
            .text_editor
            .as_mut()
            .filter(|editor| editor.element_id == element.id && editor.words_h.is_some())
        {
            editor.host.draw(scene, t, tr, theme, presence);
        }
    }

    /// Where element `el` stands on the canvas: its box, grown down to hold
    /// the words of a text box that outgrew it. A text box grows with what
    /// is written in it, as in any presentation editor; selecting, moving
    /// and resizing take the box as it shows.
    pub(super) fn element_frame(&self, dest: Rect, s: f64, el: &Element) -> Rect {
        let mut frame = render::element_dest(dest, s, el);
        if el.kind == ElementKind::Text {
            let words = self
                .text_editor
                .as_ref()
                .filter(|editor| self.editing_text && editor.element_id == el.id)
                .and_then(|editor| editor.words_h)
                .or_else(|| render::text_height(&el.id));
            if let Some(words) = words {
                frame.y1 = frame.y1.max(frame.y0 + words * s);
            }
        }
        frame
    }

    /// The words of box `element` while they are edited, as the shared
    /// editor describes them to assistive technology, in the canvas's space:
    /// the box's node holds them, keyed [`WORDS_ACCESS_KEY`].
    pub(super) fn words_access_node(&self, element: &str) -> Option<ui::a11y::AccessNode> {
        let editor = self
            .text_editor
            .as_ref()
            .filter(|editor| editor.focused && editor.element_id == element)?;
        self.editing_text_box()?;
        Some(
            editor
                .host
                .accessibility()?
                .with_hashed_key(WORDS_ACCESS_KEY),
        )
    }

    /// Whether a finger landing at `p` grabs something on the canvas: an
    /// element, a handle of the selection, the words being edited. Nothing
    /// modal is open over it. Its drag then moves, resizes or selects as
    /// the mouse's does ([`ui::widget::Widget::touch_claim`]).
    pub(super) fn canvas_takes_finger(&self, p: Point) -> bool {
        if self.mode != Mode::Edit
            || self.help_overlay.is_visible()
            || self.ctx_menu.is_open()
            || self.layout_picker.is_open()
            || self.fill_picker_open
            || self.page_sidebar.menu_open()
        {
            return false;
        }
        let chrome = self.compute_chrome();
        let flap_open = (chrome.layout == SplitLayout::Overlay && self.rail_open())
            || (chrome.inspector_layout == SplitLayout::Overlay && self.inspector_open());
        if flap_open
            || !chrome.slide_dest.inflate(12.0, 12.0).contains(p)
            || chrome.buttons.iter().any(|button| button.rect.contains(p))
        {
            return false;
        }
        if self.editing_text {
            return self.text_edit_box.contains(p) || self.element_at(&chrome, p).is_some();
        }
        let on_handle = self.selected_element().is_some_and(|el| {
            self.handles_of(chrome.slide_dest, chrome.scale, el)
                .iter()
                .any(|(_, rect)| rect.contains(p))
        });
        on_handle || self.element_at(&chrome, p).is_some()
    }

    /// Whether a finger held at `p` picks up a slide in the list to move
    /// it: the list is shown in the editor, and nothing modal is open.
    pub(super) fn rail_takes_held_finger(&self, p: Point) -> bool {
        if self.mode != Mode::Edit
            || self.help_overlay.is_visible()
            || self.ctx_menu.is_open()
            || self.layout_picker.is_open()
            || self.fill_picker_open
        {
            return false;
        }
        self.compute_chrome().rail_viewport.contains(p) && self.page_sidebar.lifts_by_hold(p)
    }
}

/// Whether `kind` is a bulleted list, or a numbered one when `ordered`.
pub(super) fn listed(kind: &ui::widget::BlockKind, ordered: bool) -> bool {
    if ordered {
        matches!(kind, ui::widget::BlockKind::Numbered { .. })
    } else {
        matches!(kind, ui::widget::BlockKind::Bullet { .. })
    }
}

/// Whether `mark` covers every word of `blocks` (and there are words).
pub(super) fn mark_covers(
    blocks: &[ui::widget::BlockContent],
    mark: &ui::widget::marks::Mark,
) -> bool {
    let mut words = blocks
        .iter()
        .filter(|block| !block.text.is_empty())
        .peekable();
    words.peek().is_some()
        && words.all(|block| {
            ui::widget::marks::range_fully_covered(&block.marks, &(0..block.text.len()), mark)
        })
}

/// Where the person is in the words they edit (the shared editor's caret or
/// selection), published to collaborators as Slides' own, so their canvas
/// shows it in the same box.
impl ui::presence::capability::PublishesPresence for SlidesApp {
    fn presence_payload(
        &self,
    ) -> Option<(
        ui::presence::PresenceKey,
        ui::collaboration::PresencePayload,
    )> {
        self.editing_editor()?.host.presence_payload()
    }

    fn shared_selection(&self) -> Option<ui::shared_selection::SharedSelection> {
        self.editing_editor()?.host.shared_selection()
    }
}

impl SlidesApp {
    /// The editor of the words being edited, read-only.
    fn editing_editor(&self) -> Option<&TextBoxEditor> {
        self.editing_text_box()?;
        self.text_editor.as_ref().filter(|editor| editor.focused)
    }
}
