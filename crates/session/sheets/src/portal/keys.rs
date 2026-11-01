//! The sheet's keys. The sheet is modal and keeps the keyboard to itself:
//! no key reaches the app beneath, a proposed global shortcut included.

use ui::advanced::WindowEvent;
use ui::widget::{EventCtx, PromptOutcome, SecretPrompt, Widget};
use ui::winit::keyboard::{Key, NamedKey, PhysicalKey};

use super::view::{PROMPT_ID, Parts, field_id};
use super::{Body, Dialog, DraftValue, Field, Focus};

pub(super) struct Keys {
    pub(super) content: ui::widget::Element,
    pub(super) parts: Parts,
}

/// The stop after (or before) `at` among `count`, around the end.
fn step(at: usize, count: usize, reverse: bool) -> usize {
    let last = count.saturating_sub(1);
    let at = at.min(last);
    match (reverse, at) {
        (true, 0) => last,
        (true, at) => at.saturating_sub(1),
        (false, at) if at >= last => 0,
        (false, at) => at.saturating_add(1),
    }
}

/// The keys a shortcut row is bound to, as pressed: the held modifiers in
/// order, then the key by its place on the keyboard, so a change of layout
/// cannot change what was granted.
fn pressed_keys(physical: PhysicalKey, modifiers: ui::winit::keyboard::ModifiersState) -> String {
    let code = match physical {
        PhysicalKey::Code(code) => format!("{code:?}").to_ascii_uppercase(),
        PhysicalKey::Unidentified(_) => String::new(),
    };
    let mut parts = Vec::new();
    if modifiers.alt_key() {
        parts.push("ALT".to_owned());
    }
    if modifiers.control_key() {
        parts.push("CTRL".to_owned());
    }
    if modifiers.shift_key() {
        parts.push("SHIFT".to_owned());
    }
    if modifiers.super_key() {
        parts.push("SUPER".to_owned());
    }
    parts.push(code);
    parts.join("+")
}

impl Keys {
    fn dialog(&self) -> Option<Dialog> {
        self.parts.shown.get().dialog
    }

    fn focus(&self) -> Focus {
        self.parts.shown.get().focus
    }

    /// Up and Down: the next (or last) choice, around the end.
    fn select_next(&self, dialog: &Dialog, reverse: bool) {
        let count = dialog.choices();
        if count == 0 {
            return;
        }
        let next = step(dialog.selected, count, reverse);
        if matches!(dialog.body, Body::Launcher { .. }) {
            self.parts.focus(Focus::Icons);
            self.parts.icon(next);
        } else {
            if matches!(dialog.body, Body::Entry { .. } | Body::Draft { .. }) {
                self.parts.focus(Focus::Choices);
            }
            self.parts.select(next);
        }
    }

    /// Tab on a draft: this page's rows, then the answers.
    fn draft_tab(&self, dialog: &Dialog, reverse: bool) {
        let Body::Draft { rows, choices } = &dialog.body else {
            return;
        };
        let mut stops: Vec<Focus> = rows.iter().map(super::draft_focus).collect();
        let answers = stops.len();
        stops.extend((0..choices.len()).map(|_| Focus::Choices));
        if stops.is_empty() {
            return;
        }
        let at = match self.focus() {
            Focus::Choices => answers.saturating_add(dialog.selected),
            focus => stops.iter().position(|stop| *stop == focus).unwrap_or(0),
        };
        let next = step(at, stops.len(), reverse);
        let Some(focus) = stops.get(next).copied() else {
            return;
        };
        self.parts.focus(focus);
        if focus == Focus::Choices {
            self.parts.select(next.saturating_sub(answers));
        }
    }

    /// Hand the keyboard to the typed field in focus and give it `event`.
    fn type_into(&mut self, field: Field, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        fn find<'a>(widget: &'a mut dyn Widget, id: &str) -> Option<&'a mut dyn Widget> {
            if widget.test_id() == Some(id) {
                return Some(widget);
            }
            widget
                .children_mut()
                .into_iter()
                .find_map(|child| find(child, id))
        }
        let id = field_id(field);
        if let Some(input) = find(self.content.as_mut(), &id) {
            input.set_focus(true);
        }
        self.content.handle_event(event, ctx);
    }

    /// The secret prompt answered or backed out: its answer goes to the
    /// shell through the seat, never through an effect.
    fn settle_prompt(&mut self) {
        fn find(widget: &mut dyn Widget) -> Option<&mut SecretPrompt> {
            if widget.test_id() == Some(PROMPT_ID) {
                return widget
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<SecretPrompt>());
            }
            widget.children_mut().into_iter().find_map(find)
        }
        let Some(prompt) = find(self.content.as_mut()) else {
            return;
        };
        if let Some(checked) = prompt.option_checked() {
            self.parts.seat.option.set(Some(checked));
        }
        match prompt.take_outcome() {
            Some(PromptOutcome::Submit(answer)) => {
                *self.parts.seat.answer.borrow_mut() = Some(answer);
                self.parts.submit();
            }
            Some(PromptOutcome::Cancel) => self.parts.cancel(),
            None => {}
        }
    }

    /// A key on a sheet with a typed field or a draft; `true` when the
    /// sheet used it as a key rather than typing.
    fn field_key(&self, dialog: &Dialog, key: &Key, ctrl: bool, shift: bool) -> bool {
        let draft = matches!(dialog.body, Body::Draft { .. });
        let focus = self.focus();
        match key {
            Key::Named(NamedKey::Escape) => self.parts.cancel(),
            Key::Named(NamedKey::Tab) if draft => self.draft_tab(dialog, shift),
            Key::Named(NamedKey::Enter) => {
                // A prose field takes Enter as a new line; Ctrl+Enter answers.
                let new_line = matches!(focus, Focus::Field(Field::Draft(at))
                    if draft_multiline(dialog, at));
                if new_line && !ctrl {
                    return false;
                }
                self.parts.confirm();
            }
            Key::Named(NamedKey::PageDown) if draft && dialog.reviewing_more() => {
                self.parts.next_page();
            }
            Key::Named(NamedKey::PageUp) if draft => self.parts.back(),
            Key::Named(NamedKey::Space) if draft => match focus {
                Focus::Flag(at) => self.parts.toggle(at),
                Focus::Choices => self.parts.select(dialog.selected),
                _ => return false,
            },
            Key::Named(NamedKey::ArrowDown | NamedKey::Tab) if !draft => {
                self.select_next(dialog, false);
            }
            Key::Named(NamedKey::ArrowUp) if !draft => self.select_next(dialog, true),
            _ => return false,
        }
        true
    }

    /// A key on a sheet that takes no typed text.
    fn sheet_key(&self, dialog: &Dialog, key: &Key, physical: PhysicalKey, ctx: &EventCtx<'_>) {
        let modifiers = ctx.modifiers.state();
        let shortcuts = matches!(dialog.body, Body::Shortcuts(_));
        let chord = modifiers.control_key()
            || modifiers.alt_key()
            || modifiers.shift_key()
            || modifiers.super_key();
        if shortcuts && chord {
            self.parts.bind(pressed_keys(physical, modifiers));
            return;
        }
        let paged = dialog.page.is_some();
        match key {
            Key::Named(NamedKey::ArrowDown | NamedKey::Tab) => self.select_next(dialog, false),
            Key::Named(NamedKey::ArrowUp) => self.select_next(dialog, true),
            Key::Named(NamedKey::ArrowLeft | NamedKey::PageUp) if paged => {
                self.parts.back();
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::PageDown) if dialog.reviewing_more() => {
                self.parts.next_page();
            }
            Key::Named(NamedKey::Enter) => match dialog.body {
                Body::Answers { .. } => self.parts.answer(dialog.selected),
                _ => self.parts.confirm(),
            },
            Key::Named(NamedKey::Escape) => self.parts.cancel(),
            Key::Named(NamedKey::Backspace | NamedKey::Delete) if shortcuts => self.parts.unbind(),
            Key::Named(NamedKey::Space) => match &dialog.body {
                Body::Devices(_) => self.parts.toggle(dialog.selected),
                Body::Answers {
                    remember: Some(_), ..
                } => self.parts.remember(),
                _ => {}
            },
            // Any other key on a shortcut row is what it would be bound
            // to; the shell says why it is not taken without a modifier.
            _ if shortcuts => self.parts.bind(pressed_keys(physical, modifiers)),
            _ => {}
        }
    }
}

fn draft_multiline(dialog: &Dialog, at: usize) -> bool {
    match &dialog.body {
        Body::Draft { rows, .. } => rows.iter().any(|row| {
            row.index == at
                && row.editable
                && matches!(
                    row.value,
                    DraftValue::Text {
                        multiline: true,
                        ..
                    }
                )
        }),
        _ => false,
    }
}

impl ui::widget::Decorator for Keys {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    /// The sheet walks its own controls: Tab is its, not the host's.
    fn wants_tab_input(&self) -> bool {
        true
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let Some(dialog) = self.dialog() else {
            self.content.handle_event(event, ctx);
            return;
        };
        if matches!(dialog.body, Body::Secret(_)) {
            // The prompt routes its own keys, pointer and input method.
            self.content.handle_event(event, ctx);
            self.settle_prompt();
            *ctx.handled = true;
            return;
        }
        let (key, physical, repeat) = match event {
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ui::winit::event::ElementState::Pressed =>
            {
                (key.logical_key.clone(), key.physical_key, key.repeat)
            }
            WindowEvent::KeyboardInput { .. } => {
                *ctx.handled = true;
                return;
            }
            _ => {
                if let (WindowEvent::Ime(_), Focus::Field(field)) = (event, self.focus()) {
                    self.type_into(field, event, ctx);
                } else {
                    self.content.handle_event(event, ctx);
                }
                return;
            }
        };
        // Holding Enter must not page through a review and allow it with
        // the same press.
        let answers = matches!(key, Key::Named(NamedKey::Enter | NamedKey::PageDown));
        let guarded = dialog.page.is_some() || matches!(dialog.body, Body::Draft { .. });
        if repeat && answers && guarded {
            *ctx.handled = true;
            return;
        }
        let modifiers = ctx.modifiers.state();
        let typing = matches!(
            dialog.body,
            Body::Entry { .. } | Body::Draft { .. } | Body::Launcher { name: Some(_), .. }
        );
        if typing {
            let used = self.field_key(
                &dialog,
                &key,
                modifiers.control_key(),
                modifiers.shift_key(),
            );
            if !used {
                match self.focus() {
                    Focus::Field(field) => self.type_into(field, event, ctx),
                    // Off the name, a launcher's keys move between icons.
                    _ if matches!(dialog.body, Body::Launcher { .. }) => {
                        self.sheet_key(&dialog, &key, physical, ctx);
                    }
                    _ => {}
                }
            }
        } else {
            self.sheet_key(&dialog, &key, physical, ctx);
        }
        *ctx.handled = true;
    }
}
