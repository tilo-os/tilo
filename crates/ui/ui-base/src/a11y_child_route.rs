//! Reaching a retained widget that has no layout node of the window. A widget that paints its surface
//! may lay out retained widgets itself (an [`Embedded`](crate::widget::Embedded) row of buttons, a grid
//! of cards): they are its [`children`](crate::widget::Widget::children), and it says so
//! ([`lays_out_child`](crate::widget::Widget::lays_out_child)), but the window's layout never gave them
//! a node, so an assistive technology's or an agent's action had nothing to be dispatched to.

use std::collections::HashMap;

use taffy::NodeId as TaffyNodeId;
use vello::kurbo::Rect;

use crate::widget::Widget;

const PREFIX: &str = "tilo.child:";

/// The target that reaches the widget at child `path` below a node, and within it `inner` (a semantic
/// target, or `#` and a key path).
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

enum Inner<'a> {
    Widget,
    Semantic(&'a str),
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

/// Perform `action` addressed to `owner` by `target`: a child route acts on the widget it reaches, any
/// other target is the owner ([`Widget::perform_semantic_action`]).
///
/// A named element the reached child leaves to the widget that laid it out is offered there: a
/// context menu's row is its owner's command, which the owner keeps, so a tool activating the row
/// reaches the action behind it and never a press where the row is drawn.
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
    if let Some(widget) = descend(owner, &route) {
        match inner {
            Inner::Widget => return widget.perform_action(action, &[], state, path),
            Inner::Keys(keys) => return widget.perform_action(action, &keys, state, path),
            Inner::Semantic(target) => {
                if widget.perform_semantic_action(target, action, state, path) {
                    return true;
                }
            }
        }
    }
    match inner {
        Inner::Semantic(target) => owner.perform_semantic_action(target, action, state, path),
        Inner::Widget | Inner::Keys(_) => false,
    }
}

/// Where the widget a child route reaches is, for an activation it declines to be pressed there.
pub(crate) fn rect(owner: &dyn Widget, target: &str) -> Option<Rect> {
    let (route, _) = decode(target)?;
    descend_ref(owner, &route).map(Widget::get_rect)
}

/// How a widget with no layout node of it is reached: the nearest laid-out ancestor and the child
/// indices from it.
pub(crate) type ChildRoute = (TaffyNodeId, Vec<usize>);

/// The route of `w`'s child at `index`, from `w`'s own layout node `own` when it lays that child out,
/// else extending `w`'s own `route`.
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

/// The routes an accessibility walk hands down: each widget's is set by its parent before the widget is
/// visited.
#[derive(Default)]
pub(crate) struct Routes(HashMap<*const (), ChildRoute>);

impl Routes {
    /// `w`'s own route when it has no layout node (`own`), after routing every child from it, hosted
    /// ones included, before any child is visited.
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

