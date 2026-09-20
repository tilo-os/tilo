//! Shared prepaint solve for retained widgets with allocation-dependent styles.

use std::collections::HashMap;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};

use super::Widget;
use crate::bound_dispatch::{BoundNodeMap, mutate_bound_node};
use crate::text_render::TextRenderer;

#[derive(Debug, thiserror::Error)]
pub enum WidgetLayoutError {
    #[error(transparent)]
    Taffy(#[from] taffy::TaffyError),
    #[error("widget layout styles did not settle after eight adjustments")]
    DidNotSettle,
    #[error("widget composition did not settle after eight layout passes")]
    CompositionDidNotSettle,
}

/// Measure the host tree and settle allocation-dependent styles before calling
/// [`Widget::apply_layout`]. App and the headless runner use this same solver.
/// Manual hosts may use it with the maps from their current `build_layout`.
/// Raw Taffy layout still works for ordinary Grid tracks, but cannot resolve
/// `CardGrid`'s below-minimum single-column fallback without this adjustment.
///
/// This changes styles only: widget state and node identities are retained.
/// Paint-only and scroll-translation frames do not need to call it.
///
/// # Errors
/// Returns a Taffy error or `DidNotSettle`; callers must skip applying/painting
/// an unsettled layout.
///
/// # Safety
/// `nodes` and `bindings` must describe this exact live tree. The caller must
/// exclusively own every referenced widget throughout the call, without
/// moving, rebuilding or destroying widgets or allowing pointers to escape.
#[allow(
    clippy::implicit_hasher,
    reason = "bound dispatch uses the existing default-hasher node map"
)]
pub unsafe fn compute_widget_layout(
    taffy: &mut TaffyTree<()>,
    root: NodeId,
    available: Size<AvailableSpace>,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    bindings: &BoundNodeMap,
    text: &mut TextRenderer,
) -> Result<(), WidgetLayoutError> {
    // Intrinsic content can change without changing a Taffy style or topology.
    // Invalidate the measured node itself: marking only the root leaves cached
    // descendant heights intact and clips paragraphs as they wrap while typing.
    for &node in nodes.keys() {
        // SAFETY: use the same admission/lifetime checks as measurement below.
        let needs_measure = unsafe {
            mutate_bound_node(bindings, nodes, node, |widget| {
                widget.layout_needs_measure()
            })
        }
        .unwrap_or(false);
        if needs_measure {
            taffy.mark_dirty(node)?;
        }
    }
    for _ in 0..8 {
        taffy.compute_layout_with_measure(root, available, |known, available, node, _, _| {
            // SAFETY: caller owns the stable matching tree, each callback is
            // exclusive, and bound dispatch checks admission and lifetime.
            unsafe {
                mutate_bound_node(bindings, nodes, node, |widget| {
                    widget.measure(known, available, text)
                })
            }
            .unwrap_or(Size::ZERO)
        })?;
        let mut changed = false;
        for &node in nodes.keys() {
            // SAFETY: same contract as measurement; no topology mutation is
            // permitted by adjust_layout_with_measure.
            changed |= unsafe {
                mutate_bound_node(bindings, nodes, node, |widget| {
                    widget.adjust_layout_with_measure(taffy, node, text)
                })
            }
            .unwrap_or(false);
        }
        if !changed {
            return Ok(());
        }
    }
    Err(WidgetLayoutError::DidNotSettle)
}

/// A solved tree in traversal order, independent of allocated `NodeId`s. Include
/// topology as well as geometry: equal root rectangles alone do not prove that
/// a geometry-dependent child's composition has caught up with its allocation.
pub(crate) fn composition_geometry(
    taffy: &TaffyTree<()>,
    root: NodeId,
) -> Result<CompositionGeometry, WidgetLayoutError> {
    let mut geometry = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let children = taffy.children(node)?;
        let layout = taffy.layout(node)?;
        geometry.push((children.len(), layout.location, layout.size));
        pending.extend(children.into_iter().rev());
    }
    Ok(geometry)
}

type CompositionGeometry = Vec<(usize, taffy::Point<f32>, Size<f32>)>;

/// Shared prepaint lifecycle. First apply the new allocation to the retained
/// tree, then rebuild legacy geometry-dependent composition. Repeat only while
/// topology/geometry changes or a widget explicitly invalidates its structure.
/// `None` means sync replaced delegates: their stale maps must not be measured.
/// Eight passes bound both that case and oscillating composition. An error must
/// prevent paint; callers must not mark the new viewport successfully laid out.
pub(crate) fn settle_widget_composition(
    viewport_changed: bool,
    mut pass: impl FnMut(bool) -> Result<(Option<CompositionGeometry>, bool), WidgetLayoutError>,
) -> Result<(), WidgetLayoutError> {
    let mut previous = None;
    let mut rebuild = false;
    for _ in 0..8 {
        let (geometry, dirty) = pass(rebuild)?;
        let changed = viewport_changed && geometry != previous;
        if geometry.is_some() && !dirty && !changed {
            return Ok(());
        }
        previous = geometry;
        rebuild = true;
    }
    Err(WidgetLayoutError::CompositionDidNotSettle)
}

/// Focus state for a rebuild, matched by a unique semantic or named widget
/// identity rather than a traversal ordinal. Anonymous controls can retain focus on their existing
/// instance; ambiguity/removal deliberately leaves no restoration target.
#[derive(PartialEq, Eq)]
enum CompositionFocusKey {
    Semantic(String),
    Widget(String),
}

pub(crate) struct CompositionFocus {
    ambiguous: bool,
    key: Option<CompositionFocusKey>,
    state: Option<String>,
}
impl CompositionFocus {
    fn identity(widget: &dyn Widget) -> Option<CompositionFocusKey> {
        widget
            .accessibility()
            .and_then(|access| access.semantic_key)
            .map(CompositionFocusKey::Semantic)
            .or_else(|| {
                widget
                    .test_id()
                    .map(|id| CompositionFocusKey::Widget(id.to_owned()))
            })
    }

    /// Recover retained focus through the live widget tree after a mutation,
    /// without touching node pointers from the previous layout.
    pub(crate) fn capture_live(root: &dyn Widget) -> Option<Self> {
        fn focused(widget: &dyn Widget, result: &mut Vec<CompositionFocus>) {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
                return;
            };
            if widget.is_focusable() && widget.is_focused() {
                result.push(CompositionFocus {
                    ambiguous: false,
                    key: CompositionFocus::identity(widget),
                    state: widget.get_state(),
                });
                return; // A compound owner includes its focused descendants.
            }
            widget.for_each_child(&mut |child| focused(child, result));
        }
        fn matches(widget: &dyn Widget, key: &CompositionFocusKey) -> usize {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
                return 0;
            };
            let mut count = usize::from(CompositionFocus::identity(widget).as_ref() == Some(key));
            widget.for_each_child(&mut |child| count = count.saturating_add(matches(child, key)));
            count
        }
        let mut candidates = Vec::new();
        focused(root, &mut candidates);
        if candidates.len() != 1 {
            return None;
        }
        let mut focus = candidates.pop()?;
        if let Some(key) = &focus.key {
            focus.ambiguous = matches(root, key) != 1;
        }
        Some(focus)
    }

    /// # Safety
    /// The caller exclusively owns the live tree described by both maps.
    pub(crate) unsafe fn capture(
        node: Option<NodeId>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &BoundNodeMap,
    ) -> Option<Self> {
        // SAFETY: the caller owns the exact admitted tree.
        let mut focus = unsafe {
            crate::bound_dispatch::inspect_bound_node(bindings, nodes, node?, |widget| Self {
                ambiguous: false,
                key: Self::identity(widget),
                state: widget.get_state(),
            })
        }
        .ok()?;
        if let Some(key) = &focus.key {
            let matches = nodes
                .keys()
                .filter(|&&node| {
                    // SAFETY: the same exclusive admitted tree is still live.
                    unsafe {
                        crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                            Self::identity(widget).as_ref() == Some(key)
                        })
                    }
                    .unwrap_or(false)
                })
                .take(2)
                .count();
            focus.ambiguous = matches != 1;
        }
        Some(focus)
    }

    /// # Safety
    /// The caller exclusively owns the rebuilt tree described by both maps.
    /// `allowed` restricts the unique target to the current host focus order,
    /// including an active focus trap. It does not relax identity uniqueness.
    pub(crate) unsafe fn restore(
        &self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &BoundNodeMap,
        allowed: Option<&[NodeId]>,
    ) -> Option<NodeId> {
        let candidates: Vec<_> = nodes
            .keys()
            .copied()
            .filter(|&node| {
                // SAFETY: the caller owns the exact admitted tree.
                unsafe {
                    crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                        !self.ambiguous
                            && match &self.key {
                                Some(key) => Self::identity(widget).as_ref() == Some(key),
                                None => widget.is_focusable() && widget.is_focused(),
                            }
                    })
                }
                .unwrap_or(false)
            })
            .collect();
        let target = candidates
            .first()
            .copied()
            .filter(|_| candidates.len() == 1)
            .filter(|node| allowed.is_none_or(|order| order.contains(node)))
            .filter(|&node| {
                // Identity must be unique BEFORE focusability/enabled checks;
                // a disabled duplicate must never transfer state to its peer.
                unsafe {
                    crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                        widget.is_focusable() && widget.is_enabled()
                    })
                }
                .unwrap_or(false)
            });
        for &node in nodes.keys() {
            // SAFETY: same exclusive live-tree ownership, no topology changes.
            let _ = unsafe {
                mutate_bound_node(bindings, nodes, node, |widget| {
                    if Some(node) != target && widget.is_focused() {
                        widget.set_focus(false);
                    }
                })
            };
        }
        // Compound controls propagate focus into their internal editor. Restore
        // the owner after all other nodes have blurred; HashMap iteration order
        // must not clear a child after its owner has just focused it.
        if let Some(node) = target {
            let _ = unsafe {
                mutate_bound_node(bindings, nodes, node, |widget| {
                    widget.set_focus(true);
                    if let Some(state) = &self.state {
                        widget.set_state(state);
                    }
                })
            };
        }
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::TaffyTreeExt;
    use vello::kurbo::{Affine, Point, Rect};

    #[derive(Default)]
    struct UnsettledStyles {
        adjustments: usize,
        measures: usize,
    }
    impl Widget for UnsettledStyles {
        fn name(&self) -> &'static str {
            "UnsettledStyles"
        }
        fn sync(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &crate::Theme,
            nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = tree.leaf_node(taffy::Style::default());
            nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn measure(
            &mut self,
            _: Size<Option<f32>>,
            _: Size<AvailableSpace>,
            _: &mut TextRenderer,
        ) -> Size<f32> {
            self.measures = self.measures.checked_add(1).unwrap();
            Size {
                width: 40.0,
                height: 20.0,
            }
        }
        fn adjust_layout_style(&mut self, tree: &mut TaffyTree<()>, node: NodeId) -> bool {
            self.adjustments = self.adjustments.checked_add(1).unwrap();
            let mut style = tree.style(node).unwrap().clone();
            style.size.width = taffy::Dimension::length(if self.adjustments.is_multiple_of(2) {
                40.0
            } else {
                80.0
            });
            tree.set_style(node, style).unwrap();
            true
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {
            panic!("unsettled geometry must not be applied")
        }
        fn draw(
            &mut self,
            _: &mut vello::Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &crate::Theme,
            _: &crate::PresenceCtx,
        ) {
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        fn handle_event(
            &mut self,
            _: &winit::event::WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            _: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
            _: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
        }
    }

    #[test]
    fn style_solver_bounds_adjustment_cycles_and_rejects_unadmitted_callbacks() {
        let mut widget = UnsettledStyles::default();
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = widget.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
        let mut bindings = BoundNodeMap::new();
        let mut text = crate::font_catalog::fixture_renderer();
        let available = Size {
            width: AvailableSpace::Definite(300.0),
            height: AvailableSpace::Definite(200.0),
        };
        // SAFETY: the live tree is exclusively owned; absent admission must
        // reject callbacks even though the map contains a live pointer.
        unsafe {
            compute_widget_layout(&mut tree, root, available, &nodes, &bindings, &mut text)
                .unwrap();
        }
        assert_eq!((widget.measures, widget.adjustments), (0, 0));
        // SAFETY: all nodes and pointers came from this exact live build.
        unsafe {
            bindings.rebuild(&widget, &tree, root, &nodes);
        }
        tree.mark_dirty(root).unwrap();
        // SAFETY: same exclusively owned tree, now with matching admission.
        let result = unsafe {
            compute_widget_layout(&mut tree, root, available, &nodes, &bindings, &mut text)
        };
        assert!(matches!(result, Err(WidgetLayoutError::DidNotSettle)));
        assert_eq!(widget.adjustments, 8);
        assert!(widget.measures > 0);
    }

    #[test]
    fn explicit_structure_replacements_are_bounded_before_measurement() {
        let mut passes = 0;
        let result = settle_widget_composition(false, |rebuild| {
            assert_eq!(rebuild, passes != 0);
            passes += 1;
            Ok((None, true))
        });
        assert!(matches!(
            result,
            Err(WidgetLayoutError::CompositionDidNotSettle)
        ));
        assert_eq!(passes, 8);
    }
    #[test]
    fn focus_never_uses_an_ordinal_or_an_ambiguous_semantic_key() {
        use crate::widget::{Button, Column};
        for target_keys in [
            ["other", "target"],
            ["target", "target"],
            ["other", "removed"],
        ] {
            let mut old = Box::new(Button::new("target", "Target", None));
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = old.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            // SAFETY: exact exclusively owned tree and maps.
            unsafe {
                bindings.rebuild(old.as_ref(), &tree, root, &nodes);
            }
            let focus =
                unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
            nodes.clear();
            bindings.clear();
            let mut rebuilt = Column::new()
                .push(Box::new(Button::new(target_keys[0], "First", None)))
                .push(Box::new(Button::new(target_keys[1], "Second", None)));
            let root = rebuilt.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            // SAFETY: exact exclusively owned rebuilt tree and maps.
            unsafe {
                bindings.rebuild(&rebuilt, &tree, root, &nodes);
            }
            let restored = unsafe { focus.restore(&nodes, &bindings, None) };
            if target_keys == ["other", "target"] {
                assert!(restored.is_some());
                assert!(!rebuilt.children()[0].is_focused());
                assert!(rebuilt.children()[1].is_focused());
            } else {
                assert!(restored.is_none());
                assert!(rebuilt.children().iter().all(|child| !child.is_focused()));
            }
        }
    }
    #[test]
    fn duplicate_source_key_cannot_transfer_focus_to_the_surviving_other_control() {
        use crate::widget::{Button, Column};
        let mut old = Column::new()
            .push(Box::new(Button::new("duplicate", "Removed", None)))
            .push(Box::new(Button::new("duplicate", "Survivor", None)));
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = old.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
        let selected = tree.children(root).unwrap()[0];
        let mut bindings = BoundNodeMap::new();
        // SAFETY: exact exclusively owned tree and maps.
        unsafe {
            bindings.rebuild(&old, &tree, root, &nodes);
        }
        let focus =
            unsafe { CompositionFocus::capture(Some(selected), &nodes, &bindings) }.unwrap();
        nodes.clear();
        bindings.clear();
        let mut survivor = Button::new("duplicate", "Survivor", None);
        let root = survivor.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
        // SAFETY: exact exclusively owned replacement tree and maps.
        unsafe {
            bindings.rebuild(&survivor, &tree, root, &nodes);
        }
        assert!(unsafe { focus.restore(&nodes, &bindings, None) }.is_none());
        assert!(!survivor.is_focused());
    }
    #[test]
    fn review_disabled_duplicate_target_does_not_transfer_focus_to_enabled_other() {
        use crate::widget::{Button, Column};
        let mut old = Button::new("target", "Original", None);
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = old.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
        let mut bindings = BoundNodeMap::new();
        unsafe {
            bindings.rebuild(&old, &tree, root, &nodes);
        }
        let focus = unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
        nodes.clear();
        bindings.clear();
        let mut disabled = Button::new("target", "Original", None);
        disabled.set_enabled(false);
        let mut rebuilt = Column::new()
            .push(Box::new(disabled))
            .push(Box::new(Button::new("target", "Other", None)));
        let root = rebuilt.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
        unsafe {
            bindings.rebuild(&rebuilt, &tree, root, &nodes);
        }
        assert!(unsafe { focus.restore(&nodes, &bindings, None) }.is_none());
        assert!(rebuilt.children().iter().all(|child| !child.is_focused()));
    }
    #[test]
    fn compound_editor_restores_owner_and_inner_text_focus_after_rebuild() {
        use crate::widget::BlockEditor;
        let mut editor = Box::new(BlockEditor::new());
        editor.set_focus(true);
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = editor.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
        let mut bindings = BoundNodeMap::new();
        unsafe {
            bindings.rebuild(editor.as_ref(), &tree, root, &nodes);
        }
        assert!(editor.blocks()[0].line.is_focused());
        assert!(
            nodes
                .values()
                .filter(|&&pointer| unsafe { (*pointer).is_focused() })
                .count()
                > 1
        );
        let focus = unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
        for _ in 0..8 {
            nodes.clear();
            bindings.clear();
            tree = TaffyTree::new();
            let rebuilt = editor.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(editor.as_ref(), &tree, rebuilt, &nodes);
            }
            assert_eq!(
                unsafe { focus.restore(&nodes, &bindings, None) },
                Some(rebuilt)
            );
            assert!(editor.is_focused());
            assert!(editor.blocks()[0].line.is_focused());
        }
        // The stable named owner still cannot bypass a current focus trap.
        assert!(unsafe { focus.restore(&nodes, &bindings, Some(&[])) }.is_none());
        assert!(!editor.is_focused());
        assert!(!editor.blocks()[0].line.is_focused());
    }
}
