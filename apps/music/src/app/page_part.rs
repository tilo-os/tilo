//! A part of the painted page that is ordinary retained widgets (a row of
//! buttons, a grid of cards, an empty state), embedded where the page lays
//! it out.
//!
//! The widgets own their hover, press, focus and accessibility; what they
//! ask for comes back as the app's own action. A part is rebuilt when its
//! key (everything it shows and does) changes.

use std::cell::RefCell;
use std::rc::Rc;

use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::widget::{Column, Element, Embedded};
use vello::kurbo::Rect;
use winit::event::WindowEvent;

use super::Action;

/// Where a part's widgets leave the actions they ask for.
pub(super) type Asked = Rc<RefCell<Vec<Action>>>;

/// A widget callback that asks for `action`.
pub(super) fn asking(asked: &Asked, action: Action) -> impl FnMut() + 'static {
    let asked = Rc::clone(asked);
    move || asked.borrow_mut().push(action.clone())
}

/// One embedded part of the page.
pub(super) struct PagePart {
    host: Embedded<Column>,
    key: String,
    asked: Asked,
    placed: bool,
}

impl PagePart {
    /// Reuse the retained controls' semantics inside the painted page's
    /// synthetic accessibility subtree, retaining their actual action handlers.
    pub(super) fn access_nodes(&self) -> Vec<ui::a11y::AccessNode> {
        fn collect(widget: &dyn ui::widget::Widget, path: &str) -> Vec<ui::a11y::AccessNode> {
            let children = widget
                .children()
                .into_iter()
                .enumerate()
                .flat_map(|(index, child)| collect(child, &format!("{path}/{index}")))
                .collect::<Vec<_>>();
            if let Some(mut node) = widget.accessibility() {
                node = node.with_hashed_key(path).with_bounds(widget.get_rect());
                if let Some(id) = widget.test_id() {
                    node = node
                        .with_hashed_key(id)
                        .with_action_target(format!("music.page-widget.{id}"));
                }
                node.children.extend(children);
                vec![node]
            } else {
                children
            }
        }
        if self.placed {
            collect(self.host.widget(), &self.key)
        } else {
            Vec::new()
        }
    }

    pub(super) fn perform(
        &mut self,
        id: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        fn invoke(
            widget: &mut dyn ui::widget::Widget,
            id: &str,
            action: &ui::a11y::Action,
            state: &mut ui::widget::Edits<'_>,
        ) -> bool {
            if widget.test_id() == Some(id) {
                return widget.perform_action(action, &[], state, &automerge::ROOT);
            }
            widget
                .children_mut()
                .into_iter()
                .any(|child| invoke(child, id, action, state))
        }
        self.placed && invoke(self.host.widget_mut(), id, action, state)
    }

    pub(super) fn new() -> Self {
        Self {
            host: Embedded::new(Column::new().with_no_padding()),
            key: String::new(),
            asked: Rc::default(),
            placed: false,
        }
    }

    /// Bring the part up to date: rebuild it with `build` when `key` says
    /// what it shows changed.
    pub(super) fn update(&mut self, key: String, build: impl FnOnce(&Asked) -> Element) {
        self.placed = false;
        if key == self.key {
            return;
        }
        self.key = key;
        let widget = build(&self.asked);
        self.host = Embedded::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(widget),
        );
    }

    /// Lay the part out from `top` across `[left, right]` and paint it where
    /// it meets `clip` (the page's viewport). Returns the y below it.
    pub(super) fn place(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        (left, right): (f64, f64),
        top: f64,
        clip: Rect,
    ) -> f64 {
        let height = self
            .host
            .layout(Rect::new(left, top, right, top), ctx.theme, ctx.text);
        let bottom = top + height;
        if bottom > clip.y0 && top < clip.y1 {
            let presence = PresenceCtx::default();
            let outer = presence.push_viewport(clip);
            self.host
                .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, &presence);
            presence.pop_viewport(outer);
            self.placed = true;
        }
        bottom
    }

    /// Route one window event to the part; a part out of view takes none.
    pub(super) fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
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

    /// What the part's widgets asked for since the last call.
    pub(super) fn take_asked(&self) -> Vec<Action> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    /// The tooltip of the control under `point`, when one is.
    pub(super) fn tooltip_at(&self, point: vello::kurbo::Point) -> Option<String> {
        fn find(widget: &dyn ui::widget::Widget, point: vello::kurbo::Point) -> Option<String> {
            for child in widget.children() {
                if let Some(found) = find(child, point) {
                    return Some(found);
                }
            }
            (widget.get_rect().contains(point))
                .then(|| widget.tooltip().map(str::to_owned))
                .flatten()
        }
        if !self.placed {
            return None;
        }
        find(self.host.widget(), point)
    }

    /// Where the widget whose test id is `id` stands.
    #[cfg(test)]
    pub(super) fn find(&self, id: &str) -> Option<Rect> {
        fn find(widget: &dyn ui::widget::Widget, id: &str) -> Option<Rect> {
            if widget.test_id() == Some(id) {
                return Some(widget.get_rect());
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| find(child, id))
        }
        find(self.host.widget(), id)
    }
}
