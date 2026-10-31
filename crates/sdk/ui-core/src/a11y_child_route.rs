//! Reaching a retained widget that has no layout node of the window's own.
//!
//! A widget that paints its own surface may lay out retained widgets itself
//! (an [`Embedded`](crate::widget::Embedded) row of buttons, a grid of
//! cards): they are its [`children`](crate::widget::Widget::children), and
//! it says so ([`lays_out_child`](crate::widget::Widget::lays_out_child)),
//! but the window's layout never gave them a node, so an assistive technology's
//! or an agent's action had nothing to be dispatched to. The accessibility
//! walk addresses such a widget through the nearest ancestor that has a
//! node: that ancestor's semantic target `tilo.child:<i>/<j>…` names the
//! path of child indices down to it, and what follows a `>` names a node of
//! the widget's own synthetic tree (its semantic target, or `#` and its key
//! path). The host dispatches the action to the ancestor's node and
//! [`perform`] carries it down, so the widget acts exactly as it does for a
//! node of its own.

use std::collections::HashMap;

use taffy::NodeId as TaffyNodeId;
use vello::kurbo::Rect;

use crate::widget::Widget;

const PREFIX: &str = "tilo.child:";

/// The target that reaches the widget at child `path` below a node, and
/// within it `inner` (a semantic target, or `#` and a key path).
pub(crate) fn encode(path: &[usize], inner: Option<&str>) -> String {
    let path = path
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join("/");
    match inner {
        Some(inner) => format!("{PREFIX}{path}>{inner}"),
        None => format!("{PREFIX}{path}"),
    }
}

/// The inner part naming the synthetic node at `keys` of the reached widget.
pub(crate) fn keys(keys: &[u64]) -> String {
    let keys = keys
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("/");
    format!("#{keys}")
}

/// What reaches a node of a reached widget's own.
enum Inner<'a> {
    /// The widget itself.
    Widget,
    /// A synthetic node it routes by its semantic target.
    Semantic(&'a str),
    /// A synthetic node it routes by key path.
    Keys(Vec<u64>),
}

fn decode(target: &str) -> Option<(Vec<usize>, Inner<'_>)> {
    let rest = target.strip_prefix(PREFIX)?;
    let (path, inner) = match rest.split_once('>') {
        Some((path, inner)) => (path, Some(inner)),
        None => (rest, None),
    };
    let path = path
        .split('/')
        .map(str::parse::<usize>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let inner = match inner {
        None => Inner::Widget,
        Some(inner) if let Some(keys) = inner.strip_prefix('#') => Inner::Keys(
            keys.split('/')
                .filter(|key| !key.is_empty())
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?,
        ),
        Some(target) => Inner::Semantic(target),
    };
    (!path.is_empty()).then_some((path, inner))
}

fn descend<'w>(widget: &'w mut dyn Widget, path: &[usize]) -> Option<&'w mut dyn Widget> {
    let Some((&index, rest)) = path.split_first() else {
        return Some(widget);
    };
    let child = widget.children_mut().into_iter().nth(index)?;
    descend(child, rest)
}

fn descend_ref<'w>(widget: &'w dyn Widget, path: &[usize]) -> Option<&'w dyn Widget> {
    let Some((&index, rest)) = path.split_first() else {
        return Some(widget);
    };
    let child = widget.children().into_iter().nth(index)?;
    descend_ref(child, rest)
}

/// Perform `action` addressed to `owner` by `target`: a child route acts on
/// the widget it reaches, any other target is the owner's own
/// ([`Widget::perform_semantic_action`]).
pub(crate) fn perform(
    owner: &mut dyn Widget,
    target: &str,
    action: &crate::a11y::Action,
    state: &mut crate::widget::Edits<'_>,
    path: &crate::widget::ModelPath,
) -> bool {
    let Some((route, inner)) = decode(target) else {
        return owner.perform_semantic_action(target, action, state, path);
    };
    let Some(widget) = descend(owner, &route) else {
        return false;
    };
    match inner {
        Inner::Widget => widget.perform_action(action, &[], state, path),
        Inner::Semantic(target) => widget.perform_semantic_action(target, action, state, path),
        Inner::Keys(keys) => widget.perform_action(action, &keys, state, path),
    }
}

/// Where the widget a child route reaches stands, for an activation it
/// declines to be pressed there. `None` for any other target.
pub(crate) fn rect(owner: &dyn Widget, target: &str) -> Option<Rect> {
    let (route, _) = decode(target)?;
    descend_ref(owner, &route).map(Widget::get_rect)
}

/// How a widget with no layout node of its own is reached: the nearest
/// laid-out ancestor and the child indices from it.
pub(crate) type ChildRoute = (TaffyNodeId, Vec<usize>);

/// The route of `w`'s child at `index`, from `w`'s own layout node `own` when
/// it lays that child out, else extending `w`'s own `route`.
pub(crate) fn child_route(
    w: &dyn Widget,
    own: Option<TaffyNodeId>,
    route: Option<&ChildRoute>,
    index: usize,
) -> Option<ChildRoute> {
    match (own, route) {
        (Some(owner), _) if w.lays_out_child(index) => Some((owner, vec![index])),
        (None, Some((owner, path))) => Some((*owner, [path.as_slice(), &[index]].concat())),
        (Some(_), _) | (None, None) => None,
    }
}

/// The routes an accessibility walk hands down: each widget's is set by its
/// parent before the widget is visited.
#[derive(Default)]
pub(crate) struct Routes(HashMap<*const (), ChildRoute>);

impl Routes {
    /// `w`'s own route when it has no layout node (`own`), after routing every
    /// child from it, hosted ones included, before any child is visited.
    pub(crate) fn enter(&mut self, w: &dyn Widget, own: Option<TaffyNodeId>) -> Option<ChildRoute> {
        let route = if own.is_some() {
            None
        } else {
            self.0.remove(&std::ptr::from_ref(w).cast::<()>())
        };
        for (index, child) in w.children().into_iter().enumerate() {
            if let Some(child_route) = child_route(w, own, route.as_ref(), index) {
                self.0
                    .insert(std::ptr::from_ref(child).cast::<()>(), child_route);
            }
        }
        route
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    use crate::widget::{Button, Embedded, TaffyTreeExt};

    /// A surface that paints a retained button where it chooses, as a
    /// painted page places its parts: the button is its child, but the
    /// window's layout gives the button no node.
    struct Painter {
        button: Embedded<Button>,
        rect: Rect,
    }

    impl Widget for Painter {
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            _theme: &crate::theme::Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            taffy.fill_leaf(node_map, self)
        }

        fn apply_layout(
            &mut self,
            taffy: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: vello::kurbo::Point,
        ) {
            self.rect = crate::widget::layout_rect(taffy, node, origin);
        }

        fn draw(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text: &mut crate::text_render::TextRenderer,
            theme: &crate::theme::Theme,
            presence: &crate::presence::PresenceCtx,
        ) {
            let at = Rect::new(40.0, 60.0, 200.0, 60.0);
            self.button.layout(at, theme, text);
            self.button.draw(scene, transform, text, theme, presence);
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }

        fn name(&self) -> &str {
            "Painter"
        }

        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            Some(crate::a11y::AccessNode::new(crate::a11y::Role::Group).with_name("Painted"))
        }

        fn children(&self) -> Vec<&dyn Widget> {
            vec![self.button.widget()]
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![self.button.widget_mut()]
        }

        fn lays_out_child(&self, _child_index: usize) -> bool {
            true
        }
    }

    /// An assistive technology reaches a button a painted surface lays out
    /// itself through the window's own walk: it is exported where it is
    /// drawn, and activating it presses it.
    #[test]
    fn a_painted_surfaces_retained_button_is_operated_through_the_walk() {
        let pressed = Rc::new(Cell::new(0));
        let button = Button::new("play", "Play").on_view({
            let pressed = Rc::clone(&pressed);
            move || pressed.set(pressed.get() + 1)
        });
        let painter = Painter {
            button: Embedded::new(button),
            rect: Rect::ZERO,
        };
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            painter,
            (400, 300),
            crate::theme::Theme::dark(),
        );
        harness.relayout();
        let update = harness.a11y_tree_update();
        let (id, node) = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Play"))
            .expect("the button is exported");
        let bounds = node.bounds().expect("where it is drawn");
        assert!(bounds.x0 >= 40.0 && bounds.y0 >= 60.0, "{bounds:?}");
        harness
            .perform_a11y_action(*id, &crate::a11y::Action::Activate)
            .expect("the walk routes to the button");
        assert_eq!(pressed.get(), 1);
    }

    #[test]
    fn a_route_reads_back_as_written() {
        let plain = encode(&[2, 0], None);
        assert_eq!(plain, "tilo.child:2/0");
        assert!(matches!(decode(&plain), Some((path, Inner::Widget)) if path == [2, 0]));
        let semantic = encode(&[1], Some("menu:item:3"));
        assert!(
            matches!(decode(&semantic), Some((path, Inner::Semantic("menu:item:3"))) if path == [1])
        );
        let keyed = encode(&[0, 4], Some(&keys(&[7, 9])));
        assert!(
            matches!(decode(&keyed), Some((path, Inner::Keys(keys))) if path == [0, 4] && keys == [7, 9])
        );
        assert!(decode("tilo.child:").is_none());
        assert!(decode("music.page-widget.play").is_none());
    }
}
