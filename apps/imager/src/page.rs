//! The setup pages' shared shapes: a stepper, a body that keeps its scroll, and a problem
//! above the actions.

use std::cell::Cell;
use std::rc::Rc;

use ui::tokens::{Rhythm, TextRole};
use ui::widget::{Column, Container, Form, Row, Scrollable, Stepper, Text, Widget};

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

#[cfg(feature = "installation")]
pub(crate) fn notice(status: &str, is_error: bool) -> Option<Box<dyn Widget>> {
    (is_error && !status.is_empty())
        .then(|| Box::new(ui::widget::Banner::error(status.to_string())) as Box<dyn Widget>)
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

pub(crate) struct Frame {
    stepper: Option<Stepper>,
    body: Column,
    offset: Option<Rc<Cell<(f32, f32)>>>,
    notice: Option<Box<dyn Widget>>,
    actions: Option<Box<dyn Widget>>,
    actions_in_body: bool,
    center_short_body: bool,
    cursor: Option<ui::kurbo::Point>,
}

impl Frame {
    pub(crate) fn new(body: Column) -> Self {
        Self {
            stepper: None,
            body,
            offset: None,
            notice: None,
            actions: None,
            actions_in_body: false,
            center_short_body: false,
            cursor: None,
        }
    }

    #[must_use]
    pub(crate) fn stepper_widget(mut self, stepper: Stepper) -> Self {
        self.stepper = Some(stepper);
        self
    }

    #[must_use]
    pub(crate) fn offset(mut self, offset: Rc<Cell<(f32, f32)>>) -> Self {
        self.offset = Some(offset);
        self
    }

    #[cfg(feature = "installation")]
    #[must_use]
    pub(crate) fn with_cursor(mut self, cursor: ui::kurbo::Point) -> Self {
        self.cursor = Some(cursor);
        self
    }

    #[must_use]
    pub(crate) fn notice(mut self, notice: Option<Box<dyn Widget>>) -> Self {
        self.notice = notice;
        self
    }

    #[must_use]
    pub(crate) fn actions(mut self, actions: impl Widget + 'static) -> Self {
        self.actions = Some(Box::new(actions));
        self
    }

    #[must_use]
    pub(crate) fn with_content_actions(mut self) -> Self {
        self.actions_in_body = true;
        self
    }

    #[cfg(feature = "installation")]
    #[must_use]
    pub(crate) fn center_short_body(mut self) -> Self {
        self.center_short_body = true;
        self
    }

    pub(crate) fn build(mut self) -> Box<dyn Widget> {
        if self.actions_in_body {
            if let Some(notice) = self.notice.take() {
                self.body = self.body.push(notice);
            }
            if let Some(actions) = self.actions.take() {
                self.body = self.body.push(actions);
            }
        }
        let mut scroll = Scrollable::new_vertical()
            .always_visible()
            .fill_parent_height()
            .with_focus_reveal();
        if let Some(cursor) = self.cursor {
            scroll = scroll.with_cursor(cursor);
        }
        let body: Box<dyn Widget> = if self.center_short_body {
            scroll = scroll.with_content_min_viewport_height();
            Box::new(
                Row::new()
                    .no_wrap()
                    .with_padding_rhythm(None, None)
                    .with_align_items(taffy::AlignItems::Center)
                    .push(Box::new(self.body)),
            )
        } else {
            Box::new(self.body)
        };
        scroll = scroll.with_child(body);
        if let Some(offset) = self.offset {
            scroll = scroll.with_persisted_offset(offset);
        }
        let mut column = Column::new().with_no_padding().with_rhythm(Rhythm::Group);
        if let Some(stepper) = self.stepper {
            column = column.push(Box::new(stepper));
        }
        column = column.push(Box::new(scroll));
        if let Some(notice) = self.notice {
            column = column.push(notice);
        }
        if let Some(actions) = self.actions {
            column = column.push(actions);
        }
        Box::new(
            Container::new()
                .filling()
                .with_inset(Rhythm::Group)
                .readable()
                .centered()
                .with_child(Box::new(column)),
        )
    }
}
