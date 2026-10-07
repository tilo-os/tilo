//! Concise heterogeneous layout composition for application code.
//!
//! These macros only expand to the ordinary retained [`Column`](super::Column),
//! [`Row`](super::Row), and [`Stack`](super::Stack) builders. They add no
//! alternate tree, lifecycle, or reconciliation behavior.

/// Build a retained [`Column`](crate::widget::Column) from concrete child
/// widgets without repeating `push(Box::new(...))`.
///
/// The result is still a `Column`, so its normal layout modifiers remain
/// chainable.
///
/// ```
/// use tilo_ui::tokens::TextRole;
/// use tilo_ui::widget::{Text, TextInput};
///
/// let form = tilo_ui::column![
///     Text::new("profile-title").with_role(TextRole::Title),
///     TextInput::local().with_accessible_name("profile-name-label"),
/// ]
/// .with_gap(12.0);
/// ```
#[macro_export]
macro_rules! column {
    ($($child:expr),* $(,)?) => {{
        let layout = $crate::widget::Column::new();
        $(let layout = layout.push(::std::boxed::Box::new($child));)*
        layout
    }};
}

/// Build a retained [`Row`](crate::widget::Row) from concrete child widgets.
///
/// ```
/// use tilo_ui::widget::{Button, Text};
///
/// let actions = tilo_ui::row![
///     Text::new("status-ready"),
///     Button::new("save", "save-label"),
/// ];
/// ```
#[macro_export]
macro_rules! row {
    ($($child:expr),* $(,)?) => {{
        let layout = $crate::widget::Row::new();
        $(let layout = layout.push(::std::boxed::Box::new($child));)*
        layout
    }};
}

/// Build a retained [`Stack`](crate::widget::Stack) from overlapping concrete
/// child widgets.
///
/// ```
/// use tilo_ui::widget::{Spinner, Text};
///
/// let loading = tilo_ui::stack![Text::new("content"), Spinner::new()];
/// ```
#[macro_export]
macro_rules! stack {
    ($($child:expr),* $(,)?) => {{
        let layout = $crate::widget::Stack::new();
        $(let layout = layout.push(::std::boxed::Box::new($child));)*
        layout
    }};
}

#[cfg(test)]
mod tests {
    use crate::widget::{Button, Text, Widget};

    #[test]
    fn layouts_accept_heterogeneous_or_no_children() {
        fn layouts_accept_heterogeneous_children_and_remain_concrete() {
            let column =
                crate::column![Text::new("Title"), Button::new("save", "Save")].with_gap(7.0);
            let row = crate::row![Text::new("A"), Text::new("B")].no_wrap();
            let stack = crate::stack![Text::new("Back"), Text::new("Front")];

            assert_eq!(Widget::children(&column).len(), 2);
            assert_eq!(Widget::children(&row).len(), 2);
            assert_eq!(Widget::children(&stack).len(), 2);
        }

        fn empty_layouts_are_valid() {
            let column = crate::column![];
            let row = crate::row![];
            let stack = crate::stack![];

            assert!(Widget::children(&column).is_empty());
            assert!(Widget::children(&row).is_empty());
            assert!(Widget::children(&stack).is_empty());
        }

        layouts_accept_heterogeneous_children_and_remain_concrete();
        empty_layouts_are_valid();
    }
}
