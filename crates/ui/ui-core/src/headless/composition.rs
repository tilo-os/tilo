//! The frame's composition ([`crate::composition`]), read from the same laid-out frame inspection
//! reads: every widget by the path semantic actions name it by, its layout node's style, what it is and
//! what it shows of its own.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::Affine;

use super::FrameHarness;
use crate::app_core::AppCore as _;
use crate::composition::{Composed, Composition, Picture, css};
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
    placed: std::cell::RefCell<std::collections::HashSet<usize>>,
    /// The addresses of the widgets being composed, outermost first.
    open: std::cell::RefCell<Vec<usize>>,
    /// What a widget's parent publishes for it in its own nodes, by the widget's address: a
    /// list's row with its place in the list, its level and its disclosure.
    represented: std::cell::RefCell<HashMap<usize, crate::a11y::AccessNode>>,
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

    /// What `widget` publishes for each child it represents in its own nodes (`access`), kept
    /// for when that child is composed: the node at the path it hangs the child at, or the
    /// node standing where the child stands.
    fn represent(&self, widget: &dyn Widget, access: &crate::a11y::AccessNode) {
        let mut represented = self.represented.borrow_mut();
        for (index, child) in widget.children().into_iter().enumerate() {
            if !widget.accessibility_represents_child(index) {
                continue;
            }
            let node = match widget.accessibility_child_path(index) {
                Some(path) if !path.is_empty() => path
                    .iter()
                    .try_fold(access, |node, &at| node.children.get(at)),
                _ => {
                    let rect = child.get_rect();
                    access.children.iter().find(|node| {
                        node.bounds.is_some_and(|bounds| {
                            (bounds.x0 - rect.x0).abs() < 0.5
                                && (bounds.y0 - rect.y0).abs() < 0.5
                                && (bounds.x1 - rect.x1).abs() < 0.5
                                && (bounds.y1 - rect.y1).abs() < 0.5
                        })
                    })
                }
            };
            if let Some(node) = node {
                represented.insert(address(child), node.clone());
            }
        }
    }

    fn flows(&self, node: NodeId, origin: (f64, f64), children: &[Composed]) -> bool {
        let nodes = self.taffy.children(node).unwrap_or_default();
        // A page lays out a flex or grid container's children from its style, with the grid's
        // tracks and each child's place in them, the way the layout did.
        let laid = self.taffy.style(node).is_ok_and(|style| {
            matches!(style.display, taffy::Display::Flex | taffy::Display::Grid)
        });
        laid && !children.is_empty()
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
            let mut children = children;
            let mut composed = Composed::new(path, "", [x, y, size.0, size.1]);
            composed.flows = self.flows(node, (x, y), &children);
            let tiled = if composed.flows {
                None
            } else {
                tiles(composed.rect, 0.0, &mut children)
            };
            composed.flows |= tiled.is_some();
            composed.layout = match tiled {
                Some(grid) => Some(format!("{}{grid}", css.unwrap_or_default())),
                None => css,
            };
            composed.children = children;
            return composed;
        };
        let widget: &dyn Widget = *widget;
        self.placed.borrow_mut().insert(address(widget));
        let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
            return Composed::new(path.clone(), "ClosedComponent", [x, y, size.0, size.1]);
        };
        let rect = widget.get_rect();
        let semantics = self
            .represented
            .borrow_mut()
            .remove(&address(widget))
            .or_else(|| {
                (!widget.accessibility_hidden())
                    .then(|| widget.accessibility())
                    .flatten()
                    .map(|access| crate::a11y::with_synthetic_bounds(access, widget))
            });
        if let Some(access) = semantics
            .as_ref()
            .filter(|access| !access.children.is_empty())
        {
            self.represent(widget, access);
        }
        let base = (rect.x0, rect.y0);
        // A widget hidden from assistive technology that still stands where it is drawn (a
        // control shown still in one state, as a style guide shows it) is a picture of its paint
        // on a page: what it holds is not there to use.
        let pictured = semantics.as_ref().is_some_and(|access| access.state.hidden)
            && rect.width() > 0.0
            && rect.height() > 0.0;
        self.open.borrow_mut().extend(at);
        let mut inner = 0;
        let mut children: Vec<Composed> = if pictured {
            Vec::new()
        } else {
            children_nodes
                .iter()
                .map(|&child| self.compose(child, base, &mut inner, path))
                .collect()
        };
        if at.is_some() {
            self.open.borrow_mut().pop();
        }
        let mut composed = Composed::new(
            path.clone(),
            widget.name(),
            [rect.x0, rect.y0, rect.width(), rect.height()],
        );
        composed.flows = self.flows(node, base, &children);
        // A widget that places its children by hand (a shell's columns, a list's rows, a field's
        // button beside its text, a banner's buttons over its body) lays out on a page as the
        // grid their edges make.
        let tiled = if composed.flows {
            None
        } else {
            let scrolled = widget
                .primary_vertical_scroll()
                .map_or(0.0, |region| region.metrics.offset);
            tiles(composed.rect, scrolled, &mut children)
        };
        composed.flows |= tiled.is_some();
        composed.clip = widget
            .child_clip_rect()
            .map(|r| [r.x0, r.y0, r.width(), r.height()]);
        composed.layout = match tiled {
            Some(grid) => Some(format!("{}{grid}", css.unwrap_or_default())),
            None => css,
        };
        composed.scroll = widget.primary_vertical_scroll().map(|region| {
            [
                region.metrics.offset,
                region.metrics.content,
                region.metrics.viewport,
            ]
        });
        composed.focusable = widget.is_focusable();
        composed.appears_from = widget.appears_from().map(|point| [point.x, point.y]);
        if pictured
            || children.is_empty()
                && crate::composition::page::shows_paint(semantics.as_ref(), composed.rect)
        {
            composed.picture = Some(Picture::default());
        }
        composed.semantics = semantics;
        composed.children = children;
        composed
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
            placed: std::cell::RefCell::default(),
            open: std::cell::RefCell::default(),
            represented: std::cell::RefCell::default(),
        };
        let mut anonymous = 0;
        let root = if self.layout_built {
            walk.compose(self.root_node, (0.0, 0.0), &mut anonymous, "")
        } else {
            Composed::new(
                String::new(),
                "",
                [
                    0.0,
                    0.0,
                    f64::from(self.viewport.0),
                    f64::from(self.viewport.1),
                ],
            )
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
        Composition {
            viewport: [self.viewport.0, self.viewport.1],
            focus,
            root,
            layers,
        }
    }

    /// `composition`, read from this frame ([`Self::composition`]), with the picture of each
    /// widget a page shows as its paint ([`crate::composition::page::shows_paint`]): its paint
    /// alone, drawn at twice its size for a sharp screen. A picture is drawn again only when
    /// what the widget paints changed.
    pub fn paint_pictures(&mut self, composition: &mut Composition) {
        let mut wanted = Vec::new();
        for node in std::iter::once(&composition.root).chain(&composition.layers) {
            painted_parts(node, &mut wanted);
        }
        let mut drawn = std::mem::take(&mut self.pictures);
        let mut targets = None;
        let mut shown = HashMap::with_capacity(wanted.len());
        for (path, rect) in wanted {
            let Some((scene, [_, _, width, height])) = self.scene_of(&path, rect, &mut targets)
            else {
                continue;
            };
            if scene.encoding().is_empty() {
                continue;
            }
            let version = scene_version(&scene, width, height);
            let kept = drawn.remove(&path).filter(|(was, _)| *was == version);
            let picture = match kept {
                Some((_, picture)) => Some(picture),
                None => picture_of(&path, &scene, width, height),
            };
            if let Some(picture) = picture {
                shown.insert(path, (version, picture));
            }
        }
        for node in std::iter::once(&mut composition.root).chain(&mut composition.layers) {
            hand_pictures(node, &shown);
        }
        self.pictures = shown;
    }

    /// The paint of the widget at `path` (or the window's part row), with its box's top left at
    /// the origin, at the picture's scale; none for a path no laid-out widget has.
    fn scene_of(
        &mut self,
        path: &str,
        rect: [f64; 4],
        targets: &mut Option<HashMap<String, NodeId>>,
    ) -> Option<(Scene, [f64; 4])> {
        if path == super::part_row::WINDOW_PARTS {
            return self.paint_part_row(PICTURE_SCALE);
        }
        let node = *targets
            .get_or_insert_with(|| self.named_target_nodes())
            .get(path)?;
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
                    let mut scene = Scene::new();
                    let transform =
                        Affine::scale(PICTURE_SCALE) * Affine::translate((-rect[0], -rect[1]));
                    crate::widget::DrawChild::draw_child(
                        widget,
                        &mut scene,
                        transform,
                        text,
                        theme,
                        &presence.draw_env(),
                    );
                    (scene, rect)
                },
            )
        }
        .ok()
    }
}

/// The picture of `scene`, `width` by `height` logical pixels, as a PNG's `data:` address.
fn picture_of(path: &str, scene: &Scene, width: f64, height: f64) -> Option<Arc<str>> {
    let pixels = |logical: f64| {
        let device = (logical * PICTURE_SCALE).ceil();
        (1.0..=f64::from(u16::MAX))
            .contains(&device)
            .then(|| crate::num::u32_of_f64(device))
    };
    let (width, height) = (pixels(width)?, pixels(height)?);
    let rgba =
        crate::offscreen::render_picture(scene, width, height, vello::peniko::Color::TRANSPARENT)
            .inspect_err(|why| tracing::warn!(path, %why, "a page's picture could not be drawn"))
            .ok()?;
    let png = crate::png::encode_rgba8(width, height, &rgba).ok()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    Some(format!("data:image/png;base64,{encoded}").into())
}

/// What `scene` draws, as a number that changes when it does: its paths, its draws and their
/// colours, its glyphs and where they stand, and its size.
fn scene_version(scene: &Scene, width: f64, height: f64) -> u64 {
    use std::hash::{Hash, Hasher};
    let encoding = scene.encoding();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    width.to_bits().hash(&mut hash);
    height.to_bits().hash(&mut hash);
    encoding.path_data.hash(&mut hash);
    encoding.draw_data.hash(&mut hash);
    encoding.path_tags.len().hash(&mut hash);
    encoding.draw_tags.len().hash(&mut hash);
    encoding.transforms.len().hash(&mut hash);
    encoding.resources.patches.len().hash(&mut hash);
    for glyph in &encoding.resources.glyphs {
        glyph.id.hash(&mut hash);
        glyph.x.to_bits().hash(&mut hash);
        glyph.y.to_bits().hash(&mut hash);
    }
    hash.finish()
}

/// Device pixels a page's picture has for each logical pixel of its widget: a sharp picture on a
/// screen of twice the density, scaled down on any other.
const PICTURE_SCALE: f64 = 2.0;

/// Every widget under `node` that shows as a picture, with where it stands.
fn painted_parts(node: &Composed, out: &mut Vec<(String, [f64; 4])>) {
    if node.picture.is_some() {
        out.push((node.path.clone(), node.rect));
    }
    for child in &node.children {
        painted_parts(child, out);
    }
}

/// `node` with each picture of `shown` handed to the widget it is of.
fn hand_pictures(node: &mut Composed, shown: &HashMap<String, (u64, Arc<str>)>) {
    if let Some(picture) = &mut node.picture {
        match shown.get(&node.path) {
            Some((version, drawn)) => {
                picture.version = *version;
                picture.data = Some(Arc::clone(drawn));
            }
            None => picture.data = None,
        }
    }
    for child in &mut node.children {
        hand_pictures(child, shown);
    }
}

/// The grid that lays out `children` where they stand in a box at `rect` whose content is
/// scrolled `scrolled` down: the tracks between every edge any of them has, and each child's
/// lines in them. Children that overlap share tracks and stand over one another in the order
/// they are drawn, as a banner's buttons over its body do. What lies past the box's far edges
/// extends the grid, as a list's rows past its end do. `None` when it shows no children.
fn tiles(rect: [f64; 4], scrolled: f64, children: &mut [Composed]) -> Option<String> {
    use std::fmt::Write as _;
    let [x, y, width, height] = rect;
    // The content's corner: a child's rect is where scrolling put it.
    let y = y - scrolled;
    let shown = |child: &&mut Composed| child.rect[2] > 0.0 && child.rect[3] > 0.0;
    let placed: Vec<&mut Composed> = children.iter_mut().filter(shown).collect();
    if placed.is_empty() {
        return None;
    }
    // The edges past the box's start are the first track's: a child there reaches back from it.
    let edges = |start: f64, length: f64, of: &dyn Fn(&Composed) -> [f64; 2]| {
        let mut edges = vec![0.0, length];
        for child in &placed {
            let [from, to] = of(child);
            edges.extend([(from - start).max(0.0), (to - start).max(0.0)]);
        }
        edges.sort_by(f64::total_cmp);
        edges.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        edges
    };
    let columns = edges(x, width, &|child| [child.rect[0], child.rect[0] + child.rect[2]]);
    let rows = edges(y, height, &|child| [child.rect[1], child.rect[1] + child.rect[3]]);
    let line = |edges: &[f64], at: f64| {
        edges
            .iter()
            .position(|edge| (edge - at).abs() < 0.5)
            .map_or(1, |index| index + 1)
    };
    if columns.len() < 2 || rows.len() < 2 {
        return None;
    }
    let tracks = |edges: &[f64]| {
        edges
            .windows(2)
            .map(|pair| format!("{}px", pair[1] - pair[0]))
            .collect::<Vec<_>>()
            .join(" ")
    };
    // A child's lines: from the edge it starts at to the one it ends at, the first track when
    // it stands wholly before the box's start.
    let lines = |edges: &[f64], from: f64, to: f64| {
        let start = line(edges, from.max(0.0));
        (start, line(edges, to.max(0.0)).max(start + 1))
    };
    for child in placed {
        let [cx, cy, cw, ch] = child.rect;
        let (left, top) = (cx - x, cy - y);
        let mut place = child.layout.take().unwrap_or_else(|| {
            if child.children.is_empty() {
                String::new()
            } else {
                "display:flex;flex-direction:column;".to_owned()
            }
        });
        let (column, row) = (lines(&columns, left, left + cw), lines(&rows, top, top + ch));
        let _ = write!(
            place,
            "grid-column:{}/{};grid-row:{}/{};margin:0;",
            column.0, column.1, row.0, row.1
        );
        // What stands before the box's start (rows a list scrolled up, a part reaching past its
        // wrapper) reaches back from the first track by as much, at the size it was laid out at.
        if left < -0.5 || top < -0.5 {
            let _ = write!(
                place,
                "margin:{}px 0 0 {}px;width:{cw}px;height:{ch}px;min-width:0;min-height:0;\
                 max-width:none;max-height:none;",
                top.min(0.0),
                left.min(0.0),
            );
        }
        child.layout = Some(place);
    }
    Some(format!(
        "display:grid;grid-template-columns:{};grid-template-rows:{};gap:0;padding:0;",
        tracks(&columns),
        tracks(&rows)
    ))
}
