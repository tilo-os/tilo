//! The one page frame every imager surface stands in: the image wizard, the
//! guided setup and the folder import.
//!
//! A page is a readable column centred in the window: an optional stepper
//! on top, the scrolling body, an optional notice the person has to act on,
//! and the page's actions at the bottom. Every surface builds its page
//! here, so the stepper, the body's first line, the scroll bar and the
//! actions sit at the same place whichever surface is showing.

use std::cell::Cell;
use std::rc::Rc;

use ui::tokens::{Rhythm, TextRole};
use ui::widget::{Banner, Column, Container, Form, Row, Scrollable, Stepper, Text, Widget};

/// A padding-free, content-sized column: one section of a page.
pub(crate) fn stack() -> Column {
    Column::new().with_no_padding().with_intrinsic_height()
}

/// The page's title: the screen's one level-1 heading.
pub(crate) fn title(text: &str) -> Text {
    Text::new(text)
        .with_role(TextRole::Title)
        .with_heading_level(1)
}

/// A page body: its title, then each section a group apart.
pub(crate) fn body(title_text: &str) -> Column {
    stack()
        .with_rhythm(Rhythm::Group)
        .push(Box::new(title(title_text)))
}

/// A page body whose first row is built by the caller (a title with a
/// trailing control).
pub(crate) fn body_with(header: Box<dyn Widget>) -> Column {
    stack().with_rhythm(Rhythm::Group).push(header)
}

/// A group of related lines inside a section.
pub(crate) fn related() -> Column {
    stack().with_rhythm(Rhythm::Related)
}

/// A form as one block of a scrolling page.
pub(crate) fn form_block(form: Form) -> Box<dyn Widget> {
    Box::new(form)
}

/// The page's notice: a problem the person has to act on. What merely
/// happened is not repeated here; the page already shows it.
#[cfg(feature = "installation")]
pub(crate) fn notice(status: &str, is_error: bool) -> Option<Box<dyn Widget>> {
    (is_error && !status.is_empty())
        .then(|| Box::new(Banner::error(status.to_string())) as Box<dyn Widget>)
}

/// The id of the control holding keyboard focus in `root`, if it has one.
/// A surface that composes its page again reads it first and hands it to
/// [`refocus`], so the person keeps their place.
pub(crate) fn focused_id(root: &dyn Widget) -> Option<String> {
    if root.is_focusable() && root.is_focused() {
        return root.test_id().map(str::to_string);
    }
    root.children().into_iter().find_map(focused_id)
}

/// Give keyboard focus to the control `id` in a newly composed page.
pub(crate) fn refocus(root: &mut dyn Widget, id: &str) -> bool {
    if root.test_id() == Some(id) && root.is_focusable() {
        root.set_focus(true);
        return true;
    }
    root.children_mut()
        .into_iter()
        .any(|child| refocus(child, id))
}

/// One page, assembled by [`Frame::build`].
pub(crate) struct Frame {
    stepper: Option<Stepper>,
    body: Column,
    offset: Option<Rc<Cell<(f32, f32)>>>,
    notice: Option<Box<dyn Widget>>,
    actions: Option<Row>,
    actions_in_body: bool,
    center_short_body: bool,
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
        }
    }

    /// Where a wizard is.
    #[cfg(feature = "installation")]
    #[must_use]
    pub(crate) fn stepper(mut self, steps: &[&str], current: usize) -> Self {
        self.stepper = Some(
            Stepper::new(steps.iter().map(|step| (*step).to_string()).collect()).current(current),
        );
        self
    }

    /// Where a wizard is, with a stepper the caller configured (one that can
    /// go back to a done step).
    #[must_use]
    pub(crate) fn stepper_widget(mut self, stepper: Stepper) -> Self {
        self.stepper = Some(stepper);
        self
    }

    /// Keep the body's scroll position across rebuilds of the page.
    #[must_use]
    pub(crate) fn offset(mut self, offset: Rc<Cell<(f32, f32)>>) -> Self {
        self.offset = Some(offset);
        self
    }

    /// A problem the person has to act on, above the actions.
    #[must_use]
    pub(crate) fn notice(mut self, notice: Option<Box<dyn Widget>>) -> Self {
        self.notice = notice;
        self
    }

    /// The page's actions: Back at the leading edge, the main action at the
    /// trailing one.
    #[must_use]
    pub(crate) fn actions(mut self, actions: Row) -> Self {
        self.actions = Some(actions);
        self
    }

    /// Keep a short explanation, its problem and recovery controls together.
    /// They still scroll as one region when large text needs more room.
    #[must_use]
    pub(crate) fn with_content_actions(mut self) -> Self {
        self.actions_in_body = true;
        self
    }

    /// Centre a short body in its measured scrolling viewport. Long content
    /// keeps its intrinsic extent and starts at the scrolling edge; navigation
    /// stays outside that viewport in either case.
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
                self.body = self.body.push(Box::new(actions));
            }
        }
        let mut scroll = Scrollable::new_vertical()
            .always_visible()
            .fill_parent_height()
            .with_focus_reveal();
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
            column = column.push(Box::new(actions));
        }
        // A wizard stands alone in its window: its readable column is
        // centred, the page gutter around it.
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
