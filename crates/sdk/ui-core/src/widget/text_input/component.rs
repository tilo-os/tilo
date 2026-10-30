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
    /// What to type, shown while it is empty.
    pub placeholder: Option<String>,
    /// Room for this many lines: the field wraps and grows with what is typed.
    pub lines: Option<i64>,
    /// The title of the document the window is for, edited in place as its large title.
    pub title: bool,
    /// Whether it can be typed into now.
    pub enabled: bool,
    /// Someone else changed the text and the person has not seen it yet: the field takes their
    /// colour. The host sets it, from what the field is bound to (task-legibility §3).
    pub changed_by: Option<Person>,
    /// What the text was before that change, said when the field is hovered.
    pub previous: Option<String>,
    /// Enter was pressed: what was typed is done (a quick add's entry). With it, Enter ends the
    /// text rather than breaking its line; Shift+Enter still breaks a wrapping field's line.
    #[component(event)]
    pub submit: Event,
}

impl Default for TextInputProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            placeholder: None,
            lines: None,
            title: false,
            enabled: true,
            changed_by: None,
            previous: None,
            submit: Event::None,
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
            placeholder,
            lines,
            title,
            enabled,
            changed_by,
            previous,
            submit,
        } = props;
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
            .with_presence_path(value.path.clone())
            .on_change(move |text| edits.edited(Value::Text(text.to_owned())))
            .with_id(key)
            .with_accessible_name(&name)
            .with_placeholder(&placeholder)
            .with_change(change)
            .disabled(!enabled);
        if let Some(lines) = lines {
            input = input
                .multiline(true)
                .with_min_lines(usize::try_from(lines.max(1)).unwrap_or(1));
        }
        // A long title wraps rather than running out of a narrow window; Enter still ends it.
        if title {
            input = input
                .with_text_role(TextRole::LargeTitle)
                .with_plain_chrome()
                .multiline(true)
                .with_enter_submits(true);
        }
        if !submit.is_none() {
            input = input.on_submit(move |_| submit.fire(None));
        }
        input
    }
}
