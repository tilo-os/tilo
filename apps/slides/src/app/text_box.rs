//! The selected text box's words on the canvas: the shared rich-text
//! editor embedded where the box is drawn, opened by the press that opens
//! the box, driven by mouse and finger, and laid out so the box grows to
//! its words.

use super::{
    Affine, AutoCommit, Chrome, DeckRecord, Dispatcher, Element, ElementKind, ElementState,
    Embedded, FieldOutcome, Mode, Modifiers, MouseButton, Palette, Point, PresenceCtx, Rect,
    RichTextField, RichTextSurface, RoutedEvent, Scene, SlidesApp, SplitLayout, SubmitKey,
    TextRenderer, Theme, Widget, WindowEvent, actions, render,
};

/// The words of the selected text box on the canvas: the SDK editor,
/// embedded where the box is drawn and laid out at the slide's size, editing
/// the element's rich-text body, each edit a field edit of the deck at the
/// body's path. It is laid out while the box is only selected, so the press
/// that opens the box lands in its words; it draws and takes the keyboard
/// only while the person edits them.
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
        });
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

/// One finger on the canvas, driving it as the pointer.
#[derive(Clone, Copy)]
pub(super) struct Finger {
    pub(super) id: u64,
    /// Where it landed, in physical pixels.
    pub(super) start: winit::dpi::PhysicalPosition<f64>,
    /// It travelled past a tap's slop: its moves now drag.
    pub(super) moving: bool,
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
            || self.editing_title
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
        let pointer = match routed.event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
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
                ..
            } => self.text_drag = true,
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
        // A slide's type is sized in slide units, whatever the reader's
        // text scale, the same while its words are edited.
        let unit = render::unit_scale(theme);
        let theme = unit.as_ref().unwrap_or(theme);
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
    pub(super) fn draw_text_editor(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.editing_text {
            return;
        }
        let Some((_, element)) = self.selected_text_box() else {
            return;
        };
        let unit = render::unit_scale(theme);
        let theme = unit.as_ref().unwrap_or(theme);
        if let Some(editor) = self
            .text_editor
            .as_mut()
            .filter(|editor| editor.element_id == element.id && editor.words_h.is_some())
        {
            editor.host.draw(scene, t, tr, theme, &PresenceCtx::empty());
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

    /// Whether a finger landing at `p` grabs something on the canvas: an
    /// element, a handle of the selection, the words being edited. Nothing
    /// modal is open over it.
    pub(super) fn canvas_takes_finger(&self, p: Point) -> bool {
        if self.mode != Mode::Edit
            || self.help_overlay.is_visible()
            || self.ctx_menu.is_open()
            || self.layout_picker.is_open()
            || self.fill_picker_open
            || self.toolbar_menu_open
            || self.editing_title
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

    /// A finger that landed on the canvas ([`Self::canvas_takes_finger`])
    /// is the pointer, so touch does what the mouse does: a drag moves or
    /// resizes the box under it, a tap selects, a second tap opens a text
    /// box at the word tapped, and in the words being edited a tap places
    /// the caret and a drag selects. Until it travels past a tap's slop the
    /// finger only taps, so a tap never nudges a box. Other fingers wait
    /// while it is down. Returns whether the event was such a finger's.
    pub(super) fn touch_as_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::TouchPhase;
        let WindowEvent::Touch(touch) = event else {
            return false;
        };
        let at = ui::synthetic_input::cursor_moved(touch.location.x, touch.location.y);
        let events = match (self.finger, touch.phase) {
            (None, TouchPhase::Started) => {
                let p = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                if !self.canvas_takes_finger(p) {
                    return false;
                }
                self.finger = Some(Finger {
                    id: touch.id,
                    start: touch.location,
                    moving: false,
                });
                vec![at, ui::synthetic_input::mouse_press()]
            }
            (None, _) => return false,
            (Some(finger), TouchPhase::Moved) if finger.id == touch.id => {
                let travel = (touch.location.x - finger.start.x)
                    .hypot(touch.location.y - finger.start.y)
                    / ctx.scale_factor;
                if !finger.moving && travel <= ui::gesture::TouchGestureArena::TAP_SLOP {
                    *ctx.handled = true;
                    return true;
                }
                self.finger = Some(Finger {
                    moving: true,
                    ..finger
                });
                vec![at]
            }
            (Some(finger), TouchPhase::Ended | TouchPhase::Cancelled) if finger.id == touch.id => {
                let release = if finger.moving {
                    vec![at, ui::synthetic_input::mouse_release()]
                } else {
                    // A tap releases where it pressed.
                    vec![ui::synthetic_input::mouse_release()]
                };
                self.replay_pointer(&release, ctx);
                self.finger = None;
                *ctx.handled = true;
                return true;
            }
            // Another finger, while this one holds the canvas.
            (Some(_), _) => {
                *ctx.handled = true;
                return true;
            }
        };
        self.replay_pointer(&events, ctx);
        *ctx.handled = true;
        true
    }

    /// Run the pointer events a finger stands for, each as its own event:
    /// one consumed move must not swallow the press that follows it.
    pub(super) fn replay_pointer(
        &mut self,
        events: &[WindowEvent],
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        for event in events {
            *ctx.handled = false;
            Widget::handle_event(self, event, ctx);
        }
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
