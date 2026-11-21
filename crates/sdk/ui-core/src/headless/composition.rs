//! The frame's composition ([`crate::composition`]), read from the same laid-out frame inspection
//! reads: every widget by the path semantic actions name it by, its layout node's style, what it is and
//! what it shows of its own.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};

use super::FrameHarness;
use crate::app_core::AppCore as _;
use crate::composition::{Composed, Composition, HtmlCx, css};
use crate::widget::Widget;

/// Every widget of the retained tree by its address: the path semantic actions name it by, and the
/// widget.
fn index_widgets<'a>(
    widget: &'a dyn Widget,
    path: String,
    out: &mut HashMap<usize, Vec<(String, &'a dyn Widget)>>,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    let mut child_counts = HashMap::new();
    for (index, child) in widget.children().into_iter().enumerate() {
        let (_, child_path, _) = crate::widget::widget_path(
            child,
            &path,
            widget.child_path_name(index),
            &mut child_counts,
        );
        index_widgets(child, child_path, out);
    }
    // A widget whose first field is a child widget shares its address: the one that holds the
    // other comes first.
    out.entry(address(widget))
        .or_default()
        .insert(0, (path, widget));
}

fn address(widget: &dyn Widget) -> usize {
    std::ptr::from_ref(widget).cast::<()>() as usize
}

/// The layout tree, walked from its root, with the widget each node is.
struct Walk<'a> {
    /// The widgets at each address, the one holding the others first.
    widgets: HashMap<usize, Vec<(String, &'a dyn Widget)>>,
    node_widgets: HashMap<NodeId, usize>,
    taffy: &'a TaffyTree<()>,
    theme: &'a crate::Theme,
    placed: std::cell::RefCell<std::collections::HashSet<usize>>,
    /// The addresses of the widgets being composed, outermost first.
    open: std::cell::RefCell<Vec<usize>>,
    /// The theme each grounded widget being composed hands what it holds, innermost last: a
    /// button on a card wears the card's on-ground colours, as its paint does.
    grounds: std::cell::RefCell<Vec<std::rc::Rc<crate::Theme>>>,
}

impl<'a> Walk<'a> {
    /// The widget a node at `address` is: the outermost of those there not already being
    /// composed around it, so a banner's node is the banner and its body's node is the body.
    fn widget_at(&self, address: usize) -> Option<&(String, &'a dyn Widget)> {
        let open = self
            .open
            .borrow()
            .iter()
            .filter(|&&at| at == address)
            .count();
        self.widgets.get(&address).and_then(|all| all.get(open))
    }

    /// The innermost widget at `address`.
    fn innermost(&self, address: usize) -> Option<&(String, &'a dyn Widget)> {
        self.widgets.get(&address).and_then(|all| all.last())
    }

    fn flows(&self, node: NodeId, origin: (f64, f64), children: &[Composed]) -> bool {
        let nodes = self.taffy.children(node).unwrap_or_default();
        // A page lays out a flex container's children the way the layout did; a grid's tracks
        // are the layout's own, so its children stand at their rects.
        let flex = self
            .taffy
            .style(node)
            .is_ok_and(|style| style.display == taffy::Display::Flex);
        flex && !children.is_empty()
            && nodes.iter().zip(children).all(|(&child, composed)| {
                self.taffy
                    .style(child)
                    .is_ok_and(|style| style.position != taffy::Position::Absolute)
                    && self.taffy.layout(child).is_ok_and(|layout| {
                        (origin.0 + f64::from(layout.location.x) - composed.rect[0]).abs() < 0.5
                            && (origin.1 + f64::from(layout.location.y) - composed.rect[1]).abs()
                                < 0.5
                    })
            })
    }

    fn compose(
        &self,
        node: NodeId,
        origin: (f64, f64),
        anonymous: &mut usize,
        parent: &str,
    ) -> Composed {
        let layout = self.taffy.layout(node).ok();
        let (x, y) = layout.map_or(origin, |layout| {
            (
                origin.0 + f64::from(layout.location.x),
                origin.1 + f64::from(layout.location.y),
            )
        });
        let size = layout.map_or((0.0, 0.0), |layout| {
            (f64::from(layout.size.width), f64::from(layout.size.height))
        });
        let at = self.node_widgets.get(&node).copied();
        let widget = at.and_then(|at| self.widget_at(at));
        let css = self.taffy.style(node).ok().map(css);
        let children_nodes = self.taffy.children(node).unwrap_or_default();
        let Some((path, widget)) = widget else {
            *anonymous = anonymous.saturating_add(1);
            let path = format!("{parent}#{anonymous}");
            let mut inner = 0;
            let children: Vec<Composed> = children_nodes
                .into_iter()
                .map(|child| self.compose(child, (x, y), &mut inner, &path))
                .collect();
            let flows = self.flows(node, (x, y), &children);
            return Composed {
                flows,
                surfaces: Vec::new(),
                children,
                tag: None,
                fallback: false,
                path,
                widget: String::new(),
                rect: [x, y, size.0, size.1],
                clip: None,
                layout: css,
                scroll: None,
                semantics: None,
                ground: None,
                floats: false,
                shadow_side: None,
                attrs: Vec::new(),
                own: None,
                focusable: false,
            };
        };
        let widget: &dyn Widget = *widget;
        self.placed.borrow_mut().insert(address(widget));
        let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
            return Composed {
                tag: None,
                fallback: false,
                path: path.clone(),
                widget: "ClosedComponent".into(),
                rect: [x, y, size.0, size.1],
                clip: None,
                layout: None,
                scroll: None,
                semantics: None,
                ground: None,
                floats: false,
                shadow_side: None,
                attrs: Vec::new(),
                own: None,
                focusable: false,
                flows: false,
                surfaces: Vec::new(),
                children: Vec::new(),
            };
        };
        let rect = widget.get_rect();
        let semantics = widget
            .accessibility()
            .map(|access| crate::a11y::with_synthetic_bounds(access, widget));
        let base = (rect.x0, rect.y0);
        let theme = self.grounds.borrow().last().cloned();
        let theme: &crate::Theme = theme.as_deref().unwrap_or(self.theme);
        let mut compose_children = |ground: Option<[u8; 4]>| {
            let on = ground.map(|[r, g, b, a]| {
                std::rc::Rc::new(theme.on_ground(vello::peniko::Color::from_rgba8(r, g, b, a)))
            });
            let grounded = on.is_some();
            self.grounds.borrow_mut().extend(on);
            let mut inner = 0;
            let children: Vec<Composed> = children_nodes
                .iter()
                .map(|&child| self.compose(child, base, &mut inner, path))
                .collect();
            let widgets = children_nodes
                .iter()
                .map(|child| self.node_widgets.get(child).copied().unwrap_or(0))
                .collect();
            if grounded {
                self.grounds.borrow_mut().pop();
            }
            let flows = self.flows(node, base, &children);
            (children, widgets, flows)
        };
        let mut cx = HtmlCx {
            composed: Composed {
                tag: None,
                fallback: false,
                path: path.clone(),
                widget: widget.name().to_owned(),
                rect: [rect.x0, rect.y0, rect.width(), rect.height()],
                clip: widget
                    .child_clip_rect()
                    .map(|r| [r.x0, r.y0, r.width(), r.height()]),
                layout: css,
                scroll: widget.primary_vertical_scroll().map(|region| {
                    [
                        region.metrics.offset,
                        region.metrics.content,
                        region.metrics.viewport,
                    ]
                }),
                semantics,
                ground: None,
                floats: false,
                shadow_side: None,
                attrs: Vec::new(),
                own: None,
                focusable: widget.is_focusable(),
                flows: false,
                surfaces: Vec::new(),
                children: Vec::new(),
            },
            theme,
            compose_children: &mut compose_children,
            child_widgets: Vec::new(),
        };
        self.open.borrow_mut().extend(at);
        widget.html(&mut cx);
        if at.is_some() {
            self.open.borrow_mut().pop();
        }
        cx.composed
    }
}

impl FrameHarness {
    /// The laid-out frame's composition: what a host that renders it with elements of its own shows
    /// ([`crate::composition`]).
    #[must_use]
    pub fn composition(&self) -> Composition {
        self.drain_component_wakes();
        let mut widgets = HashMap::new();
        let mut sibling_counts = HashMap::new();
        for root in self.presentation_roots() {
            let (_, path, _) = crate::widget::widget_path(root, "", None, &mut sibling_counts);
            index_widgets(root, path, &mut widgets);
        }
        let walk = Walk {
            widgets,
            node_widgets: self
                .node_map
                .iter()
                .map(|(&node, &widget)| (node, widget.cast::<()>() as usize))
                .collect(),
            taffy: &self.taffy,
            theme: &self.theme,
            placed: std::cell::RefCell::default(),
            open: std::cell::RefCell::default(),
            grounds: std::cell::RefCell::default(),
        };
        let mut anonymous = 0;
        let root = if self.layout_built {
            walk.compose(self.root_node, (0.0, 0.0), &mut anonymous, "")
        } else {
            Composed {
                tag: None,
                fallback: false,
                path: String::new(),
                widget: String::new(),
                rect: [
                    0.0,
                    0.0,
                    f64::from(self.viewport.0),
                    f64::from(self.viewport.1),
                ],
                clip: None,
                layout: None,
                scroll: None,
                semantics: None,
                ground: None,
                floats: false,
                shadow_side: None,
                attrs: Vec::new(),
                own: None,
                focusable: false,
                flows: false,
                surfaces: Vec::new(),
                children: Vec::new(),
            }
        };
        // What the layout tree does not reach stands over it: the palette,
        // an open menu, a dialog, each laid out as a tree of its own.
        let layers = self
            .node_map
            .keys()
            .copied()
            .filter(|&node| {
                node != self.root_node
                    && self.taffy.parent(node).is_none()
                    && walk.node_widgets.get(&node).is_some_and(|address| {
                        !walk.placed.borrow().contains(address)
                            && walk.innermost(*address).is_some_and(|(_, widget)| {
                                let rect = widget.get_rect();
                                rect.width() > 0.0 && rect.height() > 0.0
                            })
                    })
            })
            .collect::<Vec<_>>();
        let mut layers: Vec<Composed> = layers
            .into_iter()
            .map(|node| {
                let mut anonymous = 0;
                walk.compose(node, (0.0, 0.0), &mut anonymous, "")
            })
            .collect();
        layers.sort_by(|a, b| a.path.cmp(&b.path));
        layers.extend(self.part_row_composition());
        let focus = self.focused.and_then(|focused| {
            walk.node_widgets
                .get(&focused)
                .and_then(|address| walk.innermost(*address))
                .map(|(path, _)| path.clone())
        });
        let mut composition = Composition {
            viewport: [self.viewport.0, self.viewport.1],
            focus,
            root,
            layers,
        };
        composition.forget_semantics_children();
        composition
    }
}
