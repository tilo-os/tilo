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

use ui::tokens::TextRole;
use ui::widget::{Column, Container, Form, Row, Scrollable, Stepper, Text, Widget};

use crate::rhythm::px::{GROUP, PAGE_GUTTER, RELATED};

/// The widest a page's column grows: a form and its review read best at a
/// bounded measure (HIG §7a), the same one `FormPage` keeps.
pub(crate) const READABLE_WIDTH: f32 = 720.0;

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

/// A page body: its title, then each section a `GROUP` apart.
pub(crate) fn body(title_text: &str) -> Column {
    stack().with_gap(GROUP).push(Box::new(title(title_text)))
}

/// A page body whose first row is built by the caller (a title with a
/// trailing control).
pub(crate) fn body_with(header: Box<dyn Widget>) -> Column {
    stack().with_gap(GROUP).push(header)
}

/// A group of related lines inside a section: `RELATED` apart.
pub(crate) fn related() -> Column {
    stack().with_gap(RELATED)
}

/// A form as one block of a scrolling page. The form's own grid admits a
/// zero minimum height, so a page taller than its viewport would squeeze
/// it under its rows; inside a container it keeps its full height.
pub(crate) fn form_block(form: Form) -> Box<dyn Widget> {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_child(Box::new(form)),
    )
}

/// A page's width: the readable column inside the page gutter.
pub(crate) fn readable_page_width() -> f32 {
    READABLE_WIDTH + 2.0 * PAGE_GUTTER
}

/// `widget` in the column beside the page's scrolling body, ending where the
/// body's content ends: the scroll view keeps its scroll bar gutter on the
/// trailing edge, and the page's fixed rows keep the same edge.
fn beside_scroll(widget: Box<dyn Widget>) -> Box<dyn Widget> {
    Box::new(
        stack()
            .with_horizontal_padding(0.0, ui::tokens::Metrics::BASE.scrollbar_gutter)
            .push(widget),
    )
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
}

impl Frame {
    pub(crate) fn new(body: Column) -> Self {
        Self {
            stepper: None,
            body,
            offset: None,
            notice: None,
            actions: None,
        }
    }

    /// Where a wizard is.
    #[must_use]
    pub(crate) fn stepper(mut self, steps: &[&str], current: usize) -> Self {
        self.stepper = Some(
            Stepper::new(steps.iter().map(|step| (*step).to_string()).collect()).current(current),
        );
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

    pub(crate) fn build(self) -> Box<dyn Widget> {
        let mut scroll = Scrollable::new_vertical()
            .fill_parent_height()
            .always_visible()
            .with_focus_reveal()
            .with_child(Box::new(self.body));
        if let Some(offset) = self.offset {
            scroll = scroll.with_persisted_offset(offset);
        }
        let mut column = Column::new()
            .with_no_padding()
            .with_horizontal_padding(PAGE_GUTTER, PAGE_GUTTER)
            .with_vertical_padding(PAGE_GUTTER)
            .with_gap(GROUP);
        if let Some(stepper) = self.stepper {
            column = column.push(beside_scroll(Box::new(stepper)));
        }
        column = column.push(Box::new(scroll));
        if let Some(notice) = self.notice {
            column = column.push(beside_scroll(notice));
        }
        if let Some(actions) = self.actions {
            column = column.push(beside_scroll(Box::new(actions)));
        }
        Box::new(
            Container::new()
                .filling()
                .with_centered_max_width(readable_page_width())
                .with_child(Box::new(column)),
        )
    }
}
