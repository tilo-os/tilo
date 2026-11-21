//! The frame's composition ([`crate::composition`]), read from the same laid-out frame inspection
//! reads: every widget by the path semantic actions name it by, its layout node's style, what it is and
//! what it shows of its own.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::{FrameHarness, RelayoutStats};
use crate::app_core::AppCore as _;
use crate::composition::{Composed, Composition, HtmlCx, css};
use crate::widget::Widget;

/// Every widget of the retained tree by its address: the path semantic actions name it by, and the
/// widget.
fn index_widgets<'a>(
    widget: &'a dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut HashMap<usize, Vec<(String, &'a dyn Widget)>>,
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
    /// The colour of each of those grounds, innermost last.
    ground_colours: std::cell::RefCell<Vec<[u8; 4]>>,
    /// The composition a page was last handed, where it may be reused.
    kept: Option<KeptParts<'a>>,
    /// The ground each widget composed now stands on, by path, for the next frame's reuse.
    stood_on: std::cell::RefCell<HashMap<String, [u8; 4]>>,
}

/// What a page was last handed of the frame ([`FrameHarness::compose_frame`]).
#[derive(Default)]
pub(super) struct Presented {
    /// Bumped by every relayout that solved, built or remounted the layout, or met a new
    /// viewport: what a kept composition read of the layout.
    layout_revision: u64,
    /// The composition it was last handed.
    kept: Option<Kept>,
    /// The composition it was handed before that one, while they differ.
    before: Option<Composition>,
    /// A frame since it was handed one left the widgets' paint marks for the next to clear.
    drawn_owed: bool,
}

impl Presented {
    /// A relayout's `stats`, at a viewport that changed or not (see `layout_revision`).
    pub(super) const fn note_layout(&mut self, stats: &RelayoutStats, viewport_changed: bool) {
        if stats.rebuilt || stats.remounted || stats.measured || viewport_changed {
            self.layout_revision = self.layout_revision.wrapping_add(1);
        }
    }

    /// Whether a frame clears the widgets' paint marks now: one that painted, or one before the
    /// page was handed anything. A page's frame leaves them for the composition that shows them.
    pub(super) const fn clears_marks(&mut self, paint: bool) -> bool {
        if paint || self.kept.is_none() {
            return true;
        }
        self.drawn_owed = true;
        false
    }
}

/// The composition a page was last handed ([`FrameHarness::compose_frame`]): what it
/// read besides the widgets' own state, and the parts it composed.
pub(super) struct Kept {
    composition: Composition,
    read: Read,
    /// The ground each widget's part stood on, by path: a part whose ground changed under it
    /// wears other colours, so it is composed again.
    stood_on: HashMap<String, [u8; 4]>,
}

/// What a composition reads besides what each widget paints: the layout, the document and what
/// it lets the person do, the window's size and density, and whether the window shows its live
/// content. A frame that moved none of it, a hover, a press's look, a focus ring, a chart's
/// readout, changes only what its widgets paint.
#[derive(PartialEq)]
pub(super) struct Read {
    /// The window shows its live content: it has no History, or its History is closed with
    /// nothing in flight. An open History shows other times of the document than the one read
    /// here, so no part composed while it is open is kept.
    live: bool,
    layout: u64,
    document: (Vec<automerge::ChangeHash>, usize),
    presentation: u64,
    permissions: Result<crate::permissions::DirectoryPermissionObservation, String>,
    viewport: (u32, u32),
    scale: u64,
}

/// The parts of the kept composition by path, and the grounds they stood on.
struct KeptParts<'a> {
    parts: HashMap<&'a str, &'a Composed>,
    stood_on: &'a HashMap<String, [u8; 4]>,
    /// The widget each kept part is, with its address, where one of this frame's widgets is
    /// there.
    widgets: HashMap<&'a str, (usize, &'a dyn Widget)>,
}

impl<'a> KeptParts<'a> {
    fn of(kept: &'a Kept, widgets: &HashMap<usize, Vec<(String, &'a dyn Widget)>>) -> Self {
        fn index<'a>(node: &'a Composed, parts: &mut HashMap<&'a str, &'a Composed>) {
            parts.insert(node.path.as_str(), node);
            for child in &node.children {
                index(child, parts);
            }
        }
        let mut parts = HashMap::new();
        for root in std::iter::once(&kept.composition.root).chain(&kept.composition.layers) {
            index(root, &mut parts);
        }
        let widgets = widgets
            .iter()
            .flat_map(|(&address, all)| {
                all.iter()
                    .map(move |(path, widget)| (path, (address, *widget)))
            })
            .filter_map(|(path, widget)| {
                parts
                    .get_key_value(path.as_str())
                    .map(|(&path, _)| (path, widget))
            })
            .collect();
        Self {
            parts,
            stood_on: &kept.stood_on,
            widgets,
        }
    }
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

    /// The part the kept composition holds for `widget` at `path`, when composing it again would
    /// give the same: the widget, and every one under it, paints as it did ([`Widget::needs_redraw`]),
    /// stands where it stood, scrolled as it was, on the same ground.
    ///
    /// Each widget in the part is checked, not only the outermost: a scroll inside it moves what
    /// it scrolls and changes nothing of the widgets around. A part that moved is composed again:
    /// what a widget places of its own is placed in the window, and placed at another offset it
    /// rounds otherwise than the kept part moved as far (a page rail's thumbnails, a fraction of
    /// a pixel apart).
    fn reuse(&self, path: &str, widget: &dyn Widget) -> Option<Composed> {
        let kept = self.kept.as_ref()?;
        let before = *kept.parts.get(path)?;
        let ground = self.ground_colours.borrow().last().copied();
        let stands = |part: &Composed, widget: &dyn Widget| {
            let rect = widget.get_rect();
            part.widget == widget.name()
                && part.rect.map(f64::to_bits)
                    == [rect.x0, rect.y0, rect.width(), rect.height()].map(f64::to_bits)
                && part.scroll
                    == widget.primary_vertical_scroll().map(|region| {
                        [
                            region.metrics.offset,
                            region.metrics.content,
                            region.metrics.viewport,
                        ]
                    })
                && !widget.needs_redraw()
                && !widget.needs_animation_frame()
        };
        if kept.stood_on.get(path).copied() != ground || !stands(before, widget) {
            return None;
        }
        let mut parts = vec![before];
        while let Some(part) = parts.pop() {
            if let Some(&(_, widget)) = kept.widgets.get(part.path.as_str())
                && !stands(part, widget)
            {
                return None;
            }
            parts.extend(&part.children);
        }
        // Every widget the part holds counts as placed, as composing it would count it: one laid
        // out as a tree of its own (a tree's rows) is not also composed over the window.
        let mut placed = self.placed.borrow_mut();
        let mut stood_on = self.stood_on.borrow_mut();
        let mut carry = vec![before];
        while let Some(part) = carry.pop() {
            if let Some(&(address, _)) = kept.widgets.get(part.path.as_str()) {
                placed.insert(address);
            }
            if let Some(&ground) = kept.stood_on.get(&part.path) {
                stood_on.insert(part.path.clone(), ground);
            }
            carry.extend(&part.children);
        }
        Some(before.clone())
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
        if let Some(reused) = self.reuse(path, widget) {
            return reused;
        }
        if let Some(&ground) = self.ground_colours.borrow().last() {
            self.stood_on.borrow_mut().insert(path.clone(), ground);
        }
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
            self.ground_colours.borrow_mut().extend(ground);
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
                self.ground_colours.borrow_mut().pop();
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
        self.compose_with(None).0
    }

    /// The frame's composition for a page that presents it ([`Self::composition`]), or `None` when
    /// it is the one this last handed over: what the page shows already shows this frame.
    ///
    /// A frame that laid nothing out and changed nothing of the document, what it lets the person
    /// do or the window, only changed what some widgets paint (a hover, a press, a focus ring, a
    /// chart's readout): only those are composed again, and every other part is the one the last
    /// frame composed. A page's frames leave the widgets' paint marks
    /// ([`Widget::mark_drawn`]) for this to clear, as a drawn frame's paint clears them.
    pub fn compose_frame(&mut self) -> Option<&Composition> {
        let read = self.composition_read();
        let previous = self.presented.kept.take();
        let reusable = previous
            .as_ref()
            .filter(|kept| kept.read == read && read.live);
        let (composition, stood_on) = self.compose_with(reusable);
        if std::mem::take(&mut self.presented.drawn_owed) {
            self.root.mark_drawn();
        }
        let changed = previous
            .as_ref()
            .is_none_or(|kept| kept.composition != composition);
        self.presented.before = previous.filter(|_| changed).map(|kept| kept.composition);
        let kept = self.presented.kept.insert(Kept {
            composition,
            read,
            stood_on,
        });
        changed.then_some(&kept.composition)
    }

    /// The composition [`Self::compose_frame`] last handed over.
    #[must_use]
    pub fn presented_composition(&self) -> Option<&Composition> {
        self.presented.kept.as_ref().map(|kept| &kept.composition)
    }

    /// The composition [`Self::compose_frame`] handed over before the one it handed last, while
    /// they differ: what a page that showed it is handed the change from
    /// ([`Composition::since`]).
    #[must_use]
    pub fn composed_before(&self) -> Option<&Composition> {
        self.presented.before.as_ref()
    }

    fn composition_read(&self) -> Read {
        Read {
            live: self
                .history
                .as_ref()
                .is_none_or(|history| history.at_rest()),
            layout: self.presented.layout_revision,
            document: crate::doc::settle_mark(&self.state.doc),
            presentation: crate::widget::resource_presentation_epoch(),
            permissions: self.permissions.directory_observation(),
            viewport: self.viewport,
            scale: self.input_scale.to_bits(),
        }
    }

    fn compose_with(&self, kept: Option<&Kept>) -> (Composition, HashMap<String, [u8; 4]>) {
        self.drain_component_wakes();
        let mut widgets = HashMap::new();
        let mut sibling_counts = HashMap::new();
        for root in self.presentation_roots() {
            index_widgets(root, "", &mut sibling_counts, &mut widgets);
        }
        let kept = kept.map(|kept| KeptParts::of(kept, &widgets));
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
            ground_colours: std::cell::RefCell::default(),
            kept,
            stood_on: std::cell::RefCell::default(),
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
        (composition, walk.stood_on.into_inner())
    }

    /// The paint of each of `parts`, widgets or parts of the composition last read
    /// ([`Self::composition`]), alone, at `scale` device pixels per logical pixel, its box's top
    /// left at the origin: the pictures a page shows for widgets that declare nothing of their own
    /// ([`crate::composition::Own::Painted`]). `None` for a part no laid-out widget has the path of.
    ///
    /// The page's presenter hands the nodes from the composition it holds, all of a frame's at
    /// once: composing the window, and naming its widgets, again for each picture made a frame
    /// with many pictures (a resize, a page of charts) do that work once a picture.
    pub fn paint_composed<'c>(
        &mut self,
        parts: impl IntoIterator<Item = &'c Composed>,
        scale: f64,
    ) -> Vec<Option<(Scene, [f64; 4])>> {
        let mut targets = None;
        parts
            .into_iter()
            .map(|composed| {
                let path = composed.path.as_str();
                if path == super::part_row::WINDOW_PARTS {
                    return self.paint_part_row(scale);
                }
                let widget_path = path.split_once('#').map_or(path, |(widget, _)| widget);
                let node = *targets
                    .get_or_insert_with(|| self.named_target_nodes())
                    .get(widget_path)?;
                self.paint_node(node, composed, scale)
            })
            .collect()
    }

    /// The paint of `composed`, the widget laid out at `node` or a part of it.
    fn paint_node(
        &mut self,
        node: NodeId,
        composed: &Composed,
        scale: f64,
    ) -> Option<(Scene, [f64; 4])> {
        let [x, y, width, height] = composed.rect;
        let part = composed
            .path
            .contains('#')
            .then(|| Rect::new(x, y, x + width, y + height));
        // Words the page sets over the picture are left out of it.
        let words_on_top = composed.children.iter().any(shows_words);
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
                    let mut paint = || {
                        crate::widget::DrawChild::draw_child(
                            widget, &mut scene, transform, text, theme, presence,
                        );
                    };
                    // A chart's readout is the page's own elements over its picture.
                    crate::widget::chart_readout::without_readouts(|| {
                        if words_on_top {
                            crate::text_render::without_words(paint);
                        } else {
                            paint();
                        }
                    });
                    (scene, [rect.x0, rect.y0, rect.width(), rect.height()])
                },
            )
        }
        .ok()
    }

    /// Scroll the widget at `path` to `offset` logical pixels, as a host whose own scroller the person
    /// moved asks; whether it moved. A scroll changes only where the view stands, as a wheel's pan
    /// does: the frame after it places what came into view and syncs nothing.
    pub fn scroll_composed(&mut self, path: &str, offset: f64) -> bool {
        let Some(&node) = self.named_target_nodes().get(path) else {
            return false;
        };
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
            Ok(crate::widget::scrollbar::PrimaryScrollResult::Changed(_))
        )
    }
}

/// Whether `node` or a part of it shows words of its own on a page.
fn shows_words(node: &Composed) -> bool {
    use crate::composition::Own;
    matches!(
        node.own,
        Some(
            Own::Text(_)
                | Own::Field { .. }
                | Own::Blocks { .. }
                | Own::Face(_)
                | Own::Control { .. }
        )
    ) || node.children.iter().any(shows_words)
}
