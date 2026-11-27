//! The setup pages' shared shapes: a stack of related lines, a page's title over its body, and
//! the focus a person left where they were.

use tilo_ui_base::tokens::Rhythm;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::Widget;
use ui::{widget::Column, widget::Form, widget::Text};

pub(crate) fn stack() -> Column {
    Column::new().with_no_padding().with_intrinsic_height()
}

pub(crate) fn title(text: &str) -> Text {
    Text::new(text)
        .with_role(TextRole::Title)
        .with_heading_level(1)
}

pub(crate) fn body(title_text: &str) -> Column {
    stack()
        .with_rhythm(Rhythm::Group)
        .push(Box::new(title(title_text)))
}

pub(crate) fn related() -> Column {
    stack().with_rhythm(Rhythm::Related)
}

pub(crate) fn form_block(form: Form) -> Box<dyn Widget> {
    Box::new(form)
}

pub(crate) fn focused_id(root: &dyn Widget) -> Option<String> {
    if root.is_focusable() && root.is_focused() {
        return root.test_id().map(str::to_string);
    }
    root.children().into_iter().find_map(focused_id)
}

pub(crate) fn refocus(root: &mut dyn Widget, id: &str) -> bool {
    if root.test_id() == Some(id) && root.is_focusable() {
        root.set_focus(true);
        return true;
    }
    root.children_mut()
        .into_iter()
        .any(|child| refocus(child, id))
}
