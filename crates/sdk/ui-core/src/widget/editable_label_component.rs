//! [`EditableLabel`] as a described component.

use crate::component::{Bound, Component, Described};
use crate::tokens::TextRole;

use super::EditableLabel;

/// A name that is typed over where it stands: a document's title, a tab's name, a list item's.
/// Reading, it is text; pressed, it is a field whose Enter or click away commits the edit and
/// whose Escape keeps the name it had.
#[derive(Component)]
#[component(name = "EditableLabel", role = "TextInput", label = "placeholder")]
pub struct EditableLabelProps {
    /// The name it shows, and where an edit of it goes.
    #[component(bind)]
    pub value: Bound<String>,
    /// What it is set in: `body` unless said.
    pub role: TextRole,
    /// What it reads, dimmed, while the value is empty: "Untitled".
    pub placeholder: Option<String>,
    /// Begin an edit on a press, as a document's title renames in place; else on a double click.
    pub edit_on_click: bool,
    /// A blank entry empties the name rather than keeping the one before.
    pub clears_on_blank: bool,
    /// It takes the row's width, eliding a long name.
    pub fill: bool,
    /// Its spoken name, for a label no text beside it names.
    pub label: Option<String>,
    /// The title of the document the window is for, edited in place as its large title.
    pub title: bool,
}

impl Default for EditableLabelProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            role: TextRole::Body,
            placeholder: None,
            edit_on_click: false,
            clears_on_blank: false,
            fill: false,
            label: None,
            title: false,
        }
    }
}

impl Described for EditableLabel {
    type Props = EditableLabelProps;

    fn from_props(key: &str, props: EditableLabelProps) -> Self {
        let EditableLabelProps {
            value,
            role,
            placeholder,
            edit_on_click,
            clears_on_blank,
            fill,
            label,
            title,
        } = props;
        let mut editable = Self::new(&value.state.get())
            .on_edit(value)
            .with_id(key)
            .with_text_role(role);
        if let Some(placeholder) = placeholder.as_deref() {
            editable = editable.with_placeholder(placeholder);
        }
        if edit_on_click {
            editable = editable.edits_on_click();
        }
        if clears_on_blank {
            editable = editable.clears_on_blank();
        }
        if fill {
            editable = editable.filling();
        }
        if let Some(name) = label {
            editable = editable.with_accessible_name(&name);
        }
        if title {
            editable = editable.with_text_role(TextRole::LargeTitle);
        }
        editable
    }

    /// A name set elsewhere changes in place; what it is set in builds it again.
    fn set_props(&mut self, props: EditableLabelProps) -> bool {
        if props.role != self.text_role()
            || props.edit_on_click != self.edits_on_a_press()
            || props.fill != self.is_filling()
        {
            return false;
        }
        let shown = props.value.state.get();
        if self.value() != shown {
            self.set_value(&shown);
        }
        true
    }
}
