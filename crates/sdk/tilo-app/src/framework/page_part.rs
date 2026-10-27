//! A part of a painted page that is ordinary retained widgets (a row of
//! buttons, a grid of cards, a row of filter chips), embedded where the
//! page lays it out: how an app that paints its own surface shows the
//! framework's controls in it.
//!
//! The widgets own their hover, press, focus and accessibility; what they
//! ask for comes back as the app's own action. A part is rebuilt when its
//! key (everything it shows and does) changes.

use std::cell::RefCell;
use std::rc::Rc;

use ui_core::paint::PaintCtx;
use ui_core::presence::PresenceCtx;
use ui_core::vello::kurbo::Rect;
use ui_core::widget::{Column, Element, Embedded, Widget};
use ui_core::winit::event::WindowEvent;

/// Where a part's widgets leave the actions they ask for.
pub type Asked<A> = Rc<RefCell<Vec<A>>>;

/// A widget callback that asks for `action`.
pub fn asking<A: Clone + 'static>(asked: &Asked<A>, action: A) -> impl FnMut() + 'static {
    let asked = Rc::clone(asked);
    move || asked.borrow_mut().push(action.clone())
}

/// A retained widget laid out and painted where the page puts it. Only a
/// widget placed in view this frame takes input or answers for what is
/// under the pointer.
pub struct Placed<W: Widget> {
    host: Embedded<W>,
    placed: bool,
}

impl<W: Widget + 'static> Placed<W> {
    pub fn new(widget: W) -> Self {
        Self {
            host: Embedded::new(widget),
            placed: false,
        }
    }

    /// Swap in a rebuilt widget.
    pub fn replace(&mut self, widget: W) {
        self.host = Embedded::new(widget);
    }

    /// Forget this frame's placement; the next [`place`](Self::place)
    /// decides it again.
    pub fn unplace(&mut self) {
        self.placed = false;
    }

    pub fn widget(&self) -> &W {
        self.host.widget()
    }

    pub fn widget_mut(&mut self) -> &mut W {
        self.host.widget_mut()
    }

    /// The widget, while it is placed in view.
    pub fn visible(&self) -> Option<&W> {
        self.placed.then(|| self.host.widget())
    }

    /// The widget, while it is placed in view.
    pub fn visible_mut(&mut self) -> Option<&mut W> {
        self.placed.then(|| self.host.widget_mut())
    }

    pub fn layout(&mut self, rect: Rect, ctx: &mut PaintCtx<'_>) -> f64 {
        self.host.layout(rect, ctx.theme, ctx.text)
    }

    /// Paint the laid-out widget inside `clip` (the page's viewport) and
    /// count it as placed.
    pub fn draw(&mut self, ctx: &mut PaintCtx<'_>, clip: Rect) {
        let presence = PresenceCtx::default();
        let outer = presence.push_viewport(clip);
        self.host
            .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, &presence);
        presence.pop_viewport(outer);
        self.placed = true;
    }

    /// Lay the widget out from `top` across `[left, right]` and paint it
    /// where it meets `clip`. Returns the y below it.
    pub fn place(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        (left, right): (f64, f64),
        top: f64,
        clip: Rect,
    ) -> f64 {
        let bottom = top + self.layout(Rect::new(left, top, right, top), ctx);
        if bottom > clip.y0 && top < clip.y1 {
            self.draw(ctx, clip);
        }
        bottom
    }

    /// Route one window event to the widget; one out of view takes none.
    pub fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui_core::widget::EventCtx<'_>) {
        if !self.placed {
            return;
        }
        self.host.handle_event(
            event,
            ctx.scale_factor,
            ctx.modifiers,
            &mut ctx.state,
            ctx.path,
            ctx.handled,
        );
    }

    pub fn tooltip_at(&self, point: ui_core::vello::kurbo::Point) -> Option<String> {
        fn find(widget: &dyn Widget, point: ui_core::vello::kurbo::Point) -> Option<String> {
            for child in widget.children() {
                if let Some(found) = find(child, point) {
                    return Some(found);
                }
            }
            (widget.get_rect().contains(point))
                .then(|| widget.tooltip().map(str::to_owned))
                .flatten()
        }
        find(self.visible()?, point)
    }
}

pub struct PagePart<A> {
    pub host: Placed<Column>,
    key: String,
    asked: Asked<A>,
    /// What the part's controls are addressed by in the page's
    /// accessibility tree: this, then the control's test id.
    target: &'static str,
}

impl<A> PagePart<A> {
    /// Reuse the retained controls' semantics inside the painted page's
    /// synthetic accessibility subtree, retaining their actual action handlers.
    pub fn access_nodes(&self) -> Vec<ui_core::a11y::AccessNode> {
        fn collect(
            widget: &dyn Widget,
            path: &str,
            target: &str,
        ) -> Vec<ui_core::a11y::AccessNode> {
            let children = widget
                .children()
                .into_iter()
                .enumerate()
                .flat_map(|(index, child)| collect(child, &format!("{path}/{index}"), target))
                .collect::<Vec<_>>();
            if let Some(mut node) = widget.accessibility() {
                node = node.with_hashed_key(path).with_bounds(widget.get_rect());
                if let Some(id) = widget.test_id() {
                    node = node
                        .with_hashed_key(id)
                        .with_action_target(format!("{target}{id}"));
                }
                node.children.extend(children);
                vec![node]
            } else {
                children
            }
        }
        self.host
            .visible()
            .map_or_else(Vec::new, |column| collect(column, &self.key, self.target))
    }

    pub fn perform(
        &mut self,
        id: &str,
        action: &ui_core::a11y::Action,
        state: &mut ui_core::widget::Edits<'_>,
    ) -> bool {
        fn invoke(
            widget: &mut dyn ui_core::widget::Widget,
            id: &str,
            action: &ui_core::a11y::Action,
            state: &mut ui_core::widget::Edits<'_>,
        ) -> bool {
            if widget.test_id() == Some(id) {
                return widget.perform_action(action, &[], state, &ui_core::automerge::ROOT);
            }
            widget
                .children_mut()
                .into_iter()
                .any(|child| invoke(child, id, action, state))
        }
        self.host
            .visible_mut()
            .is_some_and(|column| invoke(column, id, action, state))
    }

    /// A part whose controls the page's accessibility tree addresses as
    /// `target` and their test id (`music.page-widget.`).
    #[must_use]
    pub fn new(target: &'static str) -> Self {
        Self {
            host: Placed::new(Column::new().with_no_padding()),
            key: String::new(),
            asked: Rc::default(),
            target,
        }
    }

    /// Bring the part up to date: rebuild it with `build` when `key` says
    /// what it shows changed.
    pub fn update(&mut self, key: String, build: impl FnOnce(&Asked<A>) -> Element) {
        self.host.unplace();
        if key == self.key {
            return;
        }
        self.key = key;
        let widget = build(&self.asked);
        self.host.replace(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(widget),
        );
    }

    /// What the part's widgets asked for since the last call.
    pub fn take_asked(&self) -> Vec<A> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    /// Where the widget whose test id is `id` stands.
    pub fn find(&self, id: &str) -> Option<Rect> {
        ui_core::widget::find_test_id(self.host.widget(), id).map(ui_core::widget::Widget::get_rect)
    }
}
