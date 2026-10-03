//! A slot in the widget tree whose content the app replaces in place.
//!
//! The request panel, the response panel and the dialog change with the
//! model, while the controls around them (the URL bar, the tabs, the split)
//! keep their focus and state. A [`Swap`] stands where the changing content
//! goes; the app keeps its [`SwapHandle`] and hands over a freshly built
//! element, which the slot adopts at the next synchronization pass, so a
//! replacement never happens while an event is still being routed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::widget::{AppModel, Element, ModelPath, ResizableSplit, Widget};

/// What the app holds to replace a slot's content.
#[derive(Clone)]
pub(crate) struct SwapHandle {
    pending: Rc<RefCell<Option<Element>>>,
}

impl SwapHandle {
    /// Replace the slot's content at its next synchronization.
    pub(crate) fn set(&self, content: Element) {
        *self.pending.borrow_mut() = Some(content);
    }
}

/// The slot itself: transparent to layout, input, paint and semantics.
pub(crate) struct Swap {
    content: Element,
    pending: Rc<RefCell<Option<Element>>>,
    structure_dirty: bool,
    repaint: Cell<bool>,
}

impl Swap {
    pub(crate) fn new(content: Element) -> (Self, SwapHandle) {
        let pending = Rc::new(RefCell::new(None));
        let handle = SwapHandle {
            pending: Rc::clone(&pending),
        };
        (
            Self {
                content,
                pending,
                structure_dirty: false,
                repaint: Cell::new(false),
            },
            handle,
        )
    }

    fn adopt(&mut self) {
        let next = self.pending.borrow_mut().take();
        if let Some(next) = next {
            self.content = next;
            self.structure_dirty = true;
            self.repaint.set(true);
        }
    }
}

impl ui::widget::Decorator for Swap {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.adopt();
        self.content.sync(state, path);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        self.content.take_structure_dirty() | own
    }

    fn needs_redraw(&self) -> bool {
        self.repaint.get() || self.content.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.repaint.set(false);
        self.content.mark_drawn();
    }

    fn name(&self) -> &'static str {
        "Swap"
    }
}

/// A [`ResizableSplit`] whose panes follow its divider.
///
/// The split moves its divider while it is dragged but leaves the layout it
/// built for the panes as it was. Asking for a new layout whenever the
/// divider has moved lets the panes follow it.
pub(crate) struct Following<L: Widget + 'static, R: Widget + 'static> {
    split: ResizableSplit<L, R>,
    laid_out_at: f32,
}

impl<L: Widget + 'static, R: Widget + 'static> Following<L, R> {
    pub(crate) fn new(split: ResizableSplit<L, R>) -> Self {
        let laid_out_at = split.split_position();
        Self { split, laid_out_at }
    }
}

impl<L: Widget + 'static, R: Widget + 'static> ui::widget::Decorator for Following<L, R> {
    type Inner = ResizableSplit<L, R>;

    fn inner(&self) -> &ResizableSplit<L, R> {
        &self.split
    }

    fn inner_mut(&mut self) -> &mut ResizableSplit<L, R> {
        &mut self.split
    }

    fn take_structure_dirty(&mut self) -> bool {
        let moved = (self.split.split_position() - self.laid_out_at).abs() > f32::EPSILON;
        self.laid_out_at = self.split.split_position();
        self.split.take_structure_dirty() | moved
    }

    fn name(&self) -> &'static str {
        "Following"
    }
}
