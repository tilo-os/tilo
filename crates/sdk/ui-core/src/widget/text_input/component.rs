//! [`TextInput`] as a described component.

use crate::component::{Bound, Component, Described, Event, Person, Value};
use crate::tokens::TextRole;

use super::TextInput;

/// A field to type text into, bound to the text it edits. Each edit is spliced into that text,
/// never written over it, so people typing in the same text at once all keep what they typed.
#[derive(Component)]
#[component(name = "TextInput", role = "TextInput", label = "placeholder")]
pub struct TextInputProps {
    /// The text it edits.
    #[component(bind(merge = "splice"))]
    pub value: Bound<String>,
    /// Text it shows when it binds nothing: read and copied, never edited (a message body, a
    /// log). A field given one is read-only; what it reports of its selection is still
    /// [`Self::select`].
    pub text: Option<String>,
    /// Whether it is read and copied but never edited, even when it binds a field: a value shown
    /// for reading.
    pub read_only: bool,
    /// What to type, shown while it is empty.
    pub placeholder: Option<String>,
    /// Room for this many lines: the field wraps and grows with what is typed.
    pub lines: Option<i64>,
    /// The title of the document the window is for, edited in place as its large title.
    pub title: bool,
    /// Whether it can be typed into now.
    pub enabled: bool,
    /// It is where the person types next (an entry a command just opened): it takes the
    /// keyboard as soon as it is in the window, once.
    pub autofocus: bool,
    /// Someone else changed the text and the person has not seen it yet: the field takes their
    /// colour. The host sets it, from what the field is bound to (task-legibility §3).
    pub changed_by: Option<Person>,
    /// What the text was before that change, said when the field is hovered.
    pub previous: Option<String>,
    /// The entry at the end of a list: what is typed makes an item when Enter or leaving the
    /// field commits it, and the field clears; nothing typed leaves nothing (Escape, or leaving
    /// it empty). Its own draft, not a record: the item exists once it is committed.
    pub entry: bool,
    /// The plain key that focuses this control while its window has the keyboard, as the app
    /// declared it (`key: "Ctrl+N"`): a window's primary action brings the person to the list's
    /// entry rather than making an empty item. Never drawn here.
    #[component(data)]
    pub key: Option<String>,
    /// Enter was pressed: what was typed is done (a list's entry), carried with it. With it,
    /// Enter ends the text rather than breaking its line; Shift+Enter still breaks a wrapping
    /// field's line.
    #[component(event(value = String))]
    pub submit: Event,
    /// The selection settled (a drag let go, a double click, a select all, shift and an arrow
    /// key): carries the run selected as `<start>:<end>`, byte offsets into the text, or nothing
    /// when the selection collapsed. A read-only field reports it so what reads it — a message
    /// answered on just the words someone chose — can act on the run; a field being typed in
    /// keeps its selection private.
    #[component(event(value = String))]
    pub select: Event,
}

impl Default for TextInputProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            text: None,
            read_only: false,
            placeholder: None,
            lines: None,
            title: false,
            enabled: true,
            autofocus: false,
            changed_by: None,
            previous: None,
            entry: false,
            key: None,
            submit: Event::None,
            select: Event::None,
        }
    }
}

/// What a field changed by someone else says on hover: who changed it, and what it held.
fn change_note(by: &Person, previous: Option<&str>) -> String {
    match previous {
        Some(before) => crate::i18n::widget_format(
            "change-field-before",
            &[
                ("who", by.name.clone().into()),
                ("before", before.to_owned().into()),
            ],
        ),
        None => crate::i18n::widget_format("change-field", &[("who", by.name.clone().into())]),
    }
}

impl Described for TextInput {
    type Props = TextInputProps;

    fn from_props(key: &str, props: TextInputProps) -> Self {
        let TextInputProps {
            value,
            text,
            read_only,
            placeholder,
            lines,
            title,
            enabled,
            autofocus,
            changed_by,
            previous,
            entry,
            key: _,
            submit,
            select,
        } = props;
        let read_only = read_only || text.is_some();
        let change = changed_by.map(|by| {
            let note = change_note(&by, previous.as_deref());
            (by.identity, note)
        });
        let placeholder = placeholder.unwrap_or_default();
        let name = if value.name.is_empty() {
            placeholder.clone()
        } else {
            value.name.clone()
        };
        let edits = value.clone();
        let mut input = Self::from_state(&value.state)
            .with_id(key)
            .with_presence_path(value.path.clone())
            .on_change_view(move |text| edits.edited(Value::Text(text.to_owned())))
            .with_accessible_name(&name)
            .with_placeholder(&placeholder)
            .with_change(change)
            .disabled(!enabled)
            .with_read_only(read_only);
        input.change_before = previous;
        if let Some(text) = text {
            input = input
                .with_plain_chrome()
                .multiline(true)
                .with_text_role(TextRole::Body)
                .with_initial_value(&text);
        }
        if autofocus {
            input = input.wants_focus();
        }
        if let Some(lines) = lines {
            input = input
                .multiline(true)
                .with_min_lines(usize::try_from(lines.max(1)).unwrap_or(1));
        }
        if title {
            input = input
                .with_text_role(TextRole::LargeTitle)
                .with_plain_chrome()
                .multiline(true)
                .with_enter_submits(true);
        }
        if entry {
            input = input.with_entry(true);
        }
        if !submit.is_none() {
            input = input.on_submit(move |text| submit.fire(Some(Value::Text(text.to_owned()))));
        }
        input = input.with_selection_event(select);
        input
    }

    /// The text it edits, what it says and how it behaves change where the field stands: it
    /// keeps the keyboard, the caret and whatever is being composed into it.
    fn set_props(&mut self, props: TextInputProps) -> bool {
        let TextInputProps {
            value,
            text,
            read_only,
            placeholder,
            lines,
            title,
            enabled,
            autofocus,
            changed_by,
            previous,
            entry,
            key: _,
            submit,
            select,
        } = props;
        let read_only = read_only || text.is_some();
        if !self.follows(&value.state) {
            self.follow(&value.state);
        }
        self.path.clone_from(&value.path);
        self.change = changed_by.map(|by| {
            let note = change_note(&by, previous.as_deref());
            (by.identity, note)
        });
        self.change_before = previous;
        let edits = value.clone();
        self.set_on_change_view(move |text| edits.edited(Value::Text(text.to_owned())));
        let placeholder = placeholder.unwrap_or_default();
        let name = if value.name.is_empty() {
            placeholder.clone()
        } else {
            value.name.clone()
        };
        self.set_accessible_name(name);
        self.set_placeholder(&placeholder);
        self.set_disabled(!enabled);
        if autofocus {
            self.focus_wanted = true;
        }
        self.set_multiline(title || lines.is_some());
        self.min_lines = lines.map_or(0, |lines| usize::try_from(lines.max(1)).unwrap_or(1));
        self.set_text_role(if title {
            TextRole::LargeTitle
        } else {
            TextRole::Body
        });
        self.set_plain_chrome(title || text.is_some());
        self.set_read_only(read_only);
        if let Some(text) = text
            && self.value() != text
        {
            self.set_text(&text);
        }
        self.entry = entry;
        match submit {
            Event::None => self.set_on_submit(None),
            event => self.set_on_submit(Some(Box::new(move |text: &str| {
                event.fire(Some(Value::Text(text.to_owned())));
            }))),
        }
        self.enter_submits = self.enter_submits || title || entry;
        self.set_selection_event(select);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use crate::widget::Widget;
    use vello::kurbo::Rect;
    use winit::event::WindowEvent;

    /// A field keeps the keyboard, the caret and what is being composed into it when its props
    /// change: only what it says and the text it edits change.
    #[test]
    fn a_field_takes_new_props_keeping_the_keyboard_the_caret_and_the_composition() {
        let mut field = TextInput::from_props("field", TextInputProps::default());
        field.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        Widget::set_focus(&mut field, true);
        let composed = WindowEvent::Ime(winit::event::Ime::Preedit(
            "漢".into(),
            Some(("漢".len(), "漢".len())),
        ));
        assert!(
            Dispatch::new().send(&mut field, &composed).handled,
            "the field takes what is being composed"
        );
        assert!(field.is_composing(), "the field is composing");
        let caret = field.caret_position();

        let props = TextInputProps {
            placeholder: Some("Search".into()),
            ..TextInputProps::default()
        };
        assert!(
            field.set_props(props),
            "the field takes its props where it stands"
        );
        assert!(Widget::is_focused(&field), "the field kept the keyboard");
        assert_eq!(
            field.caret_position(),
            caret,
            "the caret stayed where the person put it"
        );
        assert!(
            field.is_composing(),
            "what was being composed is still in the field"
        );
    }
}
