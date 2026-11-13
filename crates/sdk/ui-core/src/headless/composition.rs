//! The frame's composition ([`crate::composition`]), read from the same
//! laid-out frame inspection reads: every widget by the path semantic
//! actions name it by, its layout node's style, what it is and what it
//! shows of its own.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::FrameHarness;
use crate::composition::{Composed, Composition, HtmlCx, css};
use crate::host_core::HostCore as _;
use crate::widget::Widget;

/// Every widget of the retained tree by its address: the path semantic
/// actions name it by, and the widget.
fn index_widgets<'a>(
    widget: &'a dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut HashMap<usize, (String, &'a dyn Widget)>,
) {
    let name = widget.name();
    let index = sibling_counts
        .entry(format!("{parent_path}::{name}"))
        .or_insert(0);
    let path = if parent_path.is_empty() {
        format!("{name}[{index}]")
    } else {
        format!("{parent_path}/{name}[{index}]")
    };
    *index = index.saturating_add(1);
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    let mut child_counts = HashMap::new();
    for child in widget.children() {
        index_widgets(child, &path, &mut child_counts, out);
    }
    // A wrapper that holds its child inline shares its address: the
    // outermost one names it, as it is the one laid out.
    out.entry(address(widget)).or_insert((path, widget));
}

fn address(widget: &dyn Widget) -> usize {
    std::ptr::from_ref(widget).cast::<()>() as usize
}

/// The layout tree, walked from its root, with the widget each node is.
struct Walk<'a> {
    widgets: HashMap<usize, (String, &'a dyn Widget)>,
    node_widgets: HashMap<NodeId, usize>,
    taffy: &'a TaffyTree<()>,
    theme: &'a crate::Theme,
    placed: std::cell::RefCell<std::collections::HashSet<usize>>,
}

impl Walk<'_> {
    /// Whether every child of `node` stands where the layout node put it,
    /// from `origin`.
    fn flows(&self, node: NodeId, origin: (f64, f64), children: &[Composed]) -> bool {
        let nodes = self.taffy.children(node).unwrap_or_default();
        !children.is_empty()
            && nodes.iter().zip(children).all(|(&child, composed)| {
                // A child its parent places by inset stands where it was put,
                // not in the run of its siblings.
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
        let widget = self
            .node_widgets
            .get(&node)
            .and_then(|address| self.widgets.get(address));
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
        // Children are laid out from where the widget stands, which a
        // scroller's offset already moved.
        let base = (rect.x0, rect.y0);
        let mut compose_children = || {
            let mut inner = 0;
            let children: Vec<Composed> = children_nodes
                .iter()
                .map(|&child| self.compose(child, base, &mut inner, path))
                .collect();
            let flows = self.flows(node, base, &children);
            (children, flows)
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
                own: None,
                focusable: widget.is_focusable(),
                flows: false,
                surfaces: Vec::new(),
                children: Vec::new(),
            },
            theme: self.theme,
            compose_children: &mut compose_children,
        };
        widget.html(&mut cx);
        cx.composed
    }
}

impl FrameHarness {
    /// The laid-out frame's composition: what a host that renders it with
    /// elements of its own shows ([`crate::composition`]).
    #[must_use]
    pub fn composition(&self) -> Composition {
        self.drain_component_wakes();
        let mut widgets = HashMap::new();
        let mut sibling_counts = HashMap::new();
        for root in self.presentation_roots() {
            index_widgets(root, "", &mut sibling_counts, &mut widgets);
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
                            && walk.widgets.get(address).is_some_and(|(_, widget)| {
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
        // The window's bar of slots, which the frame hosts over the app.
        layers.extend(self.slot_row_composition());
        let focus = self.focused.and_then(|focused| {
            walk.node_widgets
                .get(&focused)
                .and_then(|address| walk.widgets.get(address))
                .map(|(path, _)| path.clone())
        });
        Composition {
            viewport: [self.viewport.0, self.viewport.1],
            focus,
            root,
            layers,
        }
    }

    /// The paint of the widget at `path` alone, at `scale` device pixels per
    /// logical pixel, its box's top left at the origin: the picture a host
    /// shows for a widget that declares nothing of its own
    /// ([`crate::composition::Own::Painted`]). `None` when no laid-out widget has that path.
    pub fn paint_composed(&mut self, path: &str, scale: f64) -> Option<(Scene, [f64; 4])> {
        if path == super::slot_row::WINDOW_SLOTS {
            return self.paint_slot_row(scale);
        }
        // A widget's own part ([`HtmlCx::part`]): the widget's paint, within
        // the part's rect.
        let (widget_path, part) = match path.split_once('#') {
            Some((widget, _)) => (widget, Some(self.composed_rect(path)?)),
            None => (path, None),
        };
        let node = *self.named_target_nodes().get(widget_path)?;
        let text = &mut self.text;
        let theme = &self.theme;
        let presence = &self.presence;
        // SAFETY: the harness owns the laid-out tree and holds it exclusively
        // here; no reference into it outlives the call.
        unsafe {
            crate::bound_dispatch::mutate_bound_node(
                &self.bindings,
                &self.node_map,
                node,
                |widget| {
                    let rect = part.unwrap_or_else(|| widget.get_rect());
                    let mut scene = Scene::new();
                    let transform = Affine::scale(scale) * Affine::translate((-rect.x0, -rect.y0));
                    crate::widget::DrawChild::draw_child(
                        widget, &mut scene, transform, text, theme, presence,
                    );
                    (scene, [rect.x0, rect.y0, rect.width(), rect.height()])
                },
            )
        }
        .ok()
    }

    /// Where the composed node at `path` stands in the window, as the
    /// window's composition places it now.
    fn composed_rect(&self, path: &str) -> Option<Rect> {
        fn find(node: &Composed, path: &str) -> Option<[f64; 4]> {
            if node.path == path {
                return Some(node.rect);
            }
            node.children.iter().find_map(|child| find(child, path))
        }
        let composition = self.composition();
        let [x, y, width, height] = std::iter::once(&composition.root)
            .chain(&composition.layers)
            .find_map(|root| find(root, path))?;
        Some(Rect::new(x, y, x + width, y + height))
    }

    /// Scroll the widget at `path` to `offset` logical pixels, as a host
    /// whose own scroller the person moved asks; whether it took it.
    pub fn scroll_composed(&mut self, path: &str, offset: f64) -> bool {
        let Some(&node) = self.named_target_nodes().get(path) else {
            return false;
        };
        self.invalidate_local();
        // SAFETY: as in `paint_composed`.
        let scrolled = unsafe {
            crate::bound_dispatch::mutate_bound_node(
                &self.bindings,
                &self.node_map,
                node,
                |widget| {
                    widget.apply_primary_vertical_scroll(
                        crate::widget::scrollbar::PrimaryScrollAction::Absolute(offset),
                    )
                },
            )
        };
        matches!(
            scrolled,
            Ok(crate::widget::scrollbar::PrimaryScrollResult::Changed(_)
                | crate::widget::scrollbar::PrimaryScrollResult::Unchanged(_))
        )
    }
}
