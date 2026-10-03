//! Shared prepaint solve for retained widgets with allocation-dependent styles,
//! and the small inset helpers every `Style` builder used to spell by hand.

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{AvailableSpace, LengthPercentage, NodeId, Size, TaffyTree};

use super::Widget;
use crate::Theme;
use crate::bound_dispatch::{BoundNodeMap, mutate_bound_node};
use crate::text_render::TextRenderer;
use crate::tokens::TextRole;

/// What sizes an extent handed to a responsive shell (HIG §9.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtentSizing {
    /// A destination list, an overview column, a breakpoint: sized by the
    /// text it holds, so it grows by [`Theme::responsive_scale`]
    /// (`ui_scale × text_scale.max(1.0)`).
    Text,
    /// An icon rail: glyphs, not text, so it follows [`Theme::ui_scale`]
    /// alone.
    Icon,
}

/// A breakpoint or pane width **authored at 1×** for a responsive shell
/// (`NavSplit`, `NavShell`, `AppShell`, `SidebarDrawer`), resolved against
/// the theme once when the tree is built. The one place the
/// authored-to-resolved rule lives, so the four shells cannot drift.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthoredExtent {
    px: f64,
    sizing: ExtentSizing,
}

impl AuthoredExtent {
    /// A text-sized extent authored at 1×.
    #[must_use]
    pub const fn text(px: f64) -> Self {
        Self {
            px,
            sizing: ExtentSizing::Text,
        }
    }

    /// An icon-sized extent (a rail) authored at 1×.
    #[must_use]
    pub const fn icon(px: f64) -> Self {
        Self {
            px,
            sizing: ExtentSizing::Icon,
        }
    }

    /// The authored 1× value.
    #[must_use]
    pub const fn authored(&self) -> f64 {
        self.px
    }

    #[must_use]
    pub const fn sizing(&self) -> ExtentSizing {
        self.sizing
    }

    /// The same sizing with a new authored value (a per-frame setter).
    #[must_use]
    pub const fn with_px(self, px: f64) -> Self {
        Self { px, ..self }
    }

    /// The extent as laid out under `theme`: identity on a stock theme.
    #[must_use]
    pub fn resolve(&self, theme: &Theme) -> f64 {
        self.resolve_with(theme.responsive_scale(), f64::from(theme.ui_scale))
    }

    /// [`Self::resolve`] from the two factors, for a helper that caches
    /// them instead of holding a theme (`SidebarDrawer`).
    #[must_use]
    pub fn resolve_with(&self, responsive_scale: f64, ui_scale: f64) -> f64 {
        self.px
            * match self.sizing {
                ExtentSizing::Text => responsive_scale,
                ExtentSizing::Icon => ui_scale,
            }
    }
}

/// Height of a shell control that carries one line of body text — a Back
/// chip, the slot a search field sits in — authored at 1× as `authored`
/// logical px.
///
/// It grows with `ui_scale` like every metric, and once the theme's body line
/// (`typography.size(TextRole::Body)` at the current text scale) outgrows the room the
/// authored height left for it, by exactly that difference: the authored
/// padding around the line is kept and the label never overflows the control.
/// Unchanged on a stock theme, so 1× geometry is exactly what was authored.
#[must_use]
pub fn text_control_height(authored: f64, theme: &Theme) -> f64 {
    let ui = f64::from(theme.ui_scale);
    let line_at_1x = f64::from(
        crate::tokens::typography::stock()
            .style(TextRole::Body)
            .line_height_px(),
    );
    let line_now = f64::from(theme.text(TextRole::Body).line_height_px());
    let padding = (authored - line_at_1x).max(0.0) * ui;
    (authored * ui).max(line_now + padding)
}

/// Uniform padding on all four sides, in logical pixels. Pass a token
/// (`theme.padding_standard`, `theme.tokens.spacing.s4`), not a number.
///
/// taffy's own `length(px)` also infers a uniform `Rect` when assigned to a
/// padding field; this named form states the intent at the call site and,
/// like the other two helpers, clamps a negative value to zero instead of
/// handing taffy an undefined inset.
#[must_use]
pub fn insets_all(px: f32) -> taffy::Rect<LengthPercentage> {
    insets(px, px, px, px)
}

/// Symmetric padding: `horizontal` on left and right, `vertical` on top and
/// bottom — the toolbar / action-row shape.
#[must_use]
pub fn insets_xy(horizontal: f32, vertical: f32) -> taffy::Rect<LengthPercentage> {
    insets(vertical, horizontal, vertical, horizontal)
}

/// Padding per side in CSS order (top, right, bottom, left). Negative values
/// are clamped to zero: taffy treats a negative inset as undefined and a
/// caller subtracting an overlap can otherwise drift below it.
#[must_use]
pub fn insets(top: f32, right: f32, bottom: f32, left: f32) -> taffy::Rect<LengthPercentage> {
    taffy::Rect {
        left: length(left.max(0.0)),
        right: length(right.max(0.0)),
        top: length(top.max(0.0)),
        bottom: length(bottom.max(0.0)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WidgetLayoutError {
    #[error(transparent)]
    Taffy(#[from] taffy::TaffyError),
    #[error("widget layout styles did not settle after eight adjustments")]
    DidNotSettle,
    #[error("widget composition did not settle after eight layout passes")]
    CompositionDidNotSettle,
    #[error("manual layout nesting exceeds 32 levels")]
    ManualNestingLimit,
    #[error("manual geometry must be finite with nonnegative dimensions")]
    InvalidManualGeometry,
    #[error("manual child index {0} is out of bounds")]
    ManualChildIndex(usize),
    #[error("manual policy must place every child")]
    UnplacedManualChild,
    #[error("manual child {0} was placed more than once")]
    DuplicateManualPlacement(usize),
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
    // SAFETY: forwards the caller's exact live-tree ownership contract.
    unsafe { compute_widget_layout_depth(taffy, root, available, nodes, bindings, text, 0) }
}

pub(super) unsafe fn compute_widget_layout_depth(
    taffy: &mut TaffyTree<()>,
    root: NodeId,
    available: Size<AvailableSpace>,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    bindings: &BoundNodeMap,
    text: &mut TextRenderer,
    depth: usize,
) -> Result<(), WidgetLayoutError> {
    if depth > 32 {
        return Err(WidgetLayoutError::ManualNestingLimit);
    }
    let mut policies = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let policy = unsafe {
            crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                widget.manual_layout_policy()
            })
        }
        .ok()
        .flatten();
        if let Some(policy) = policy {
            policies.push((node, policy));
        }
        pending.extend(taffy.children(node)?.into_iter().rev());
    }
    if !policies.is_empty() {
        taffy.disable_rounding();
    }
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
        for (node, policy) in &policies {
            // Descriptor extraction proved admission (including unbound).
            // Re-enter the same owner's existing lease for the WHOLE policy.
            let binding = bindings.binding_for(*node);
            let Ok(_owner) = binding
                .as_ref()
                .map(crate::bound_dispatch::CallbackBinding::enter)
                .transpose()
            else {
                continue;
            };
            changed |= unsafe {
                super::manual::run_policy(policy, taffy, *node, nodes, bindings, text, depth)?
            };
        }
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
pub(crate) struct CompositionFocus {
    ambiguous: bool,
    key: Option<String>,
    semantic_key: Option<String>,
    qualified_semantic: bool,
    binding: Option<crate::bound_dispatch::CallbackBinding>,
    legacy: Option<crate::history_mode::HistoryOwner>,
    state: Option<String>,
}
impl CompositionFocus {
    fn key(widget: &dyn Widget) -> Option<String> {
        widget
            .test_id()
            .map(|id| format!("named:{id}"))
            .or_else(|| {
                widget
                    .accessibility()
                    .and_then(|access| access.semantic_key)
                    .map(|key| format!("semantic:{key}"))
            })
    }
    /// Recover focus from the retained tree after a structural mutation. No
    /// pointer from the previous layout is read; owner custody follows the
    /// same component/legacy boundaries as the next admitted topology.
    pub(crate) fn capture_live(root: &dyn Widget) -> Option<Self> {
        fn walk(
            widget: &dyn Widget,
            binding: Option<crate::bound_dispatch::CallbackBinding>,
            legacy: Option<crate::history_mode::HistoryOwner>,
            focused_ancestor: bool,
            nodes: &mut Vec<(CompositionFocus, bool)>,
        ) {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
                return;
            };
            let reset = widget.resets_callback_binding();
            let own_binding = widget.callback_binding();
            let own_legacy = widget.history_owner();
            let legacy = if own_binding.is_some() || reset {
                None
            } else {
                own_legacy.or(legacy)
            };
            if legacy
                .as_ref()
                .is_some_and(|owner| owner.validate().is_err())
            {
                return;
            }
            let binding = if reset {
                own_binding
            } else {
                own_binding.or(binding)
            };
            let focused = widget.is_focusable() && widget.is_enabled() && widget.is_focused();
            nodes.push((
                CompositionFocus {
                    ambiguous: false,
                    key: CompositionFocus::key(widget),
                    semantic_key: widget
                        .accessibility()
                        .and_then(|access| access.semantic_key),
                    qualified_semantic: false,
                    binding: binding.clone(),
                    legacy: legacy.clone(),
                    state: crate::undo::get_state(widget),
                },
                focused && !focused_ancestor,
            ));
            widget.for_each_child(&mut |child| {
                walk(
                    child,
                    binding.clone(),
                    legacy.clone(),
                    focused_ancestor || focused,
                    nodes,
                );
            });
        }
        let mut nodes = Vec::new();
        walk(root, None, None, false, &mut nodes);
        let mut targets = nodes
            .iter()
            .enumerate()
            .filter(|(_, (_, focused))| *focused);
        let index = targets.next()?.0;
        if targets.next().is_some() {
            return None;
        }
        let (mut focus, _) = nodes.remove(index);
        let key = focus.key.as_ref()?;
        focus.ambiguous = nodes.iter().any(|(other, _)| {
            other.key.as_ref() == Some(key)
                && match (&focus.binding, &other.binding) {
                    (Some(a), Some(b)) => a.same_component(b),
                    (None, None) => true,
                    _ => false,
                }
                && match (&focus.legacy, &other.legacy) {
                    (Some(a), Some(b)) => a.same_owner(b),
                    (None, None) => true,
                    _ => false,
                }
        });
        Some(focus)
    }

    fn same_owner(&self, bindings: &BoundNodeMap, node: NodeId) -> bool {
        let component = match (&self.binding, bindings.binding_for(node)) {
            (Some(a), Some(b)) => a.same_component(&b),
            (None, None) => true,
            _ => false,
        };
        component
            && match (&self.legacy, bindings.legacy_for(node)) {
                (Some(a), Some(b)) => a.same_owner(&b),
                (None, None) => true,
                _ => false,
            }
    }
    /// # Safety
    /// The caller exclusively owns the live tree described by both maps.
    pub(crate) unsafe fn capture(
        node: Option<NodeId>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &BoundNodeMap,
    ) -> Option<Self> {
        let node = node?;
        let mut state_node = node;
        // SAFETY: the caller owns the exact admitted tree.
        let mut focus = unsafe {
            crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| Self {
                ambiguous: false,
                key: Self::key(widget),
                semantic_key: widget
                    .accessibility()
                    .and_then(|access| access.semantic_key),
                qualified_semantic: false,
                binding: bindings.binding_for(node),
                legacy: bindings.legacy_for(node),
                state: None,
            })
        }
        .ok()?;
        // Runners may observe a focused leaf after an editor focused both
        // itself and that leaf. Preserve the uniquely named aggregate path,
        // so replacing/splitting the leaf cannot retarget a generic text key.
        let aggregates: Vec<_> = nodes
            .keys()
            .copied()
            .filter(|&candidate| {
                candidate != node
                    && focus.same_owner(bindings, candidate)
                    && unsafe {
                        crate::bound_dispatch::inspect_bound_node(
                            bindings,
                            nodes,
                            candidate,
                            |widget| {
                                widget.test_id().is_some()
                                    && widget.is_focusable()
                                    && widget.is_focused()
                                    && bindings.is_descendant(node, candidate)
                            },
                        )
                        .unwrap_or(false)
                    }
            })
            .collect();
        if let [aggregate] = aggregates.as_slice() {
            let _ = unsafe {
                crate::bound_dispatch::inspect_bound_node(bindings, nodes, *aggregate, |widget| {
                    focus.key = Self::key(widget);
                    focus.semantic_key = widget
                        .accessibility()
                        .and_then(|access| access.semantic_key);
                    state_node = *aggregate;
                })
            };
        } else if aggregates.len() > 1 {
            focus.ambiguous = true;
        }
        // Anonymous controls retain only their own live state. Without a
        // stable identity, no snapshot may cross this rebuild.
        focus.key.as_ref()?;
        if let Some(key) = &focus.key {
            let matches = nodes
                .keys()
                .filter(|&&node| {
                    // SAFETY: the same exclusive admitted tree is still live.
                    unsafe {
                        crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                            focus.same_owner(bindings, node)
                                && Self::key(widget).as_ref() == Some(key)
                        })
                    }
                    .unwrap_or(false)
                })
                .take(2)
                .count();
            // A unique named control keeps its identity through delegate/role
            // changes. Repeated generic names need a unique canonical qualifier.
            let matches = if matches > 1 && focus.semantic_key.is_some() {
                focus.qualified_semantic = true;
                nodes
                    .keys()
                    .filter(|&&node| unsafe {
                        crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                            focus.same_owner(bindings, node)
                                && Self::key(widget).as_ref() == Some(key)
                                && widget
                                    .accessibility()
                                    .and_then(|access| access.semantic_key)
                                    == focus.semantic_key
                        })
                        .unwrap_or(false)
                    })
                    .take(2)
                    .count()
            } else {
                matches
            };
            focus.ambiguous |= matches != 1;
        }
        if !focus.ambiguous {
            focus.state = unsafe {
                crate::bound_dispatch::inspect_bound_node(bindings, nodes, state_node, |widget| {
                    crate::undo::get_state(widget)
                })
            }
            .ok()
            .flatten();
        }
        Some(focus)
    }

    /// Resolve current focus from current admitted flags, without saved state
    /// or stale node identity. One compound ancestor/descendant path is valid;
    /// unrelated focused siblings are ambiguous and yield no input target.
    /// # Safety
    /// The caller exclusively owns the live tree described by both maps.
    pub(crate) unsafe fn current(
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &BoundNodeMap,
        allowed: Option<&[NodeId]>,
    ) -> Option<NodeId> {
        let focused: Vec<_> = nodes
            .keys()
            .copied()
            .filter(|&node| {
                allowed.is_none_or(|order| order.contains(&node))
                    && unsafe {
                        crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                            widget.is_focusable() && widget.is_enabled() && widget.is_focused()
                        })
                        .unwrap_or(false)
                    }
            })
            .collect();
        let mut candidates = focused.iter().copied().filter(|&candidate| {
            let owner = Self {
                ambiguous: false,
                key: None,
                semantic_key: None,
                qualified_semantic: false,
                state: None,
                binding: bindings.binding_for(candidate),
                legacy: bindings.legacy_for(candidate),
            };
            focused.iter().all(|&node| {
                owner.same_owner(bindings, node)
                    && (node == candidate || bindings.is_descendant(node, candidate))
            })
        });
        let unique = candidates.next()?;
        candidates.next().is_none().then_some(unique)
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
        let mut candidates: Vec<_> = nodes
            .keys()
            .copied()
            .filter(|&node| {
                // SAFETY: the caller owns the exact admitted tree.
                unsafe {
                    crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                        !self.ambiguous
                            && self.same_owner(bindings, node)
                            && match &self.key {
                                Some(key) => Self::key(widget).as_ref() == Some(key),
                                None => false,
                            }
                    })
                }
                .unwrap_or(false)
            })
            .collect();
        if (self.qualified_semantic || candidates.len() > 1) && self.semantic_key.is_some() {
            candidates.retain(|&node| unsafe {
                crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                    widget
                        .accessibility()
                        .and_then(|access| access.semantic_key)
                        == self.semantic_key
                })
                .unwrap_or(false)
            });
        }
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
        // A compound editor and its focused line form one focus path. Blurring
        // either member can clear the other, so only blur unrelated controls.
        // Relationships come from the freshly admitted layout, including
        // child delegates omitted from retained-widget traversal.
        let related: std::collections::HashSet<_> = target
            .into_iter()
            .flat_map(|target| {
                nodes
                    .keys()
                    .copied()
                    .filter(|&node| {
                        self.same_owner(bindings, node)
                            && (node == target
                                || bindings.is_descendant(node, target)
                                || bindings.is_descendant(target, node))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        for &node in nodes.keys() {
            if related.contains(&node) {
                continue;
            }
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
                        crate::undo::set_state(widget, state);
                    }
                })
            };
        }
        if let Some(node) = target {
            // Restore the selected control last: its setter may focus a child.
            let _ = unsafe {
                mutate_bound_node(bindings, nodes, node, |widget| {
                    widget.set_focus(true);
                    if let Some(state) = &self.state {
                        crate::undo::set_state(widget, state);
                    }
                })
            };
        }
        target
    }
}

#[cfg(test)]
pub(crate) struct AnonymousFocusProbe {
    pub focused: bool,
    pub enabled: bool,
    pub state: String,
    builds: usize,
}
#[cfg(test)]
impl AnonymousFocusProbe {
    pub fn new(state: &str) -> Self {
        Self {
            focused: true,
            enabled: true,
            state: state.into(),
            builds: 0,
        }
    }
}
#[cfg(test)]
impl Widget for AnonymousFocusProbe {
    crate::provides!(dyn crate::undo::UndoPoints);

    fn name(&self) -> &str {
        "AnonymousFocusProbe"
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &crate::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Change allocation so stale NodeId reuse cannot accidentally pass.
        self.builds += 1;
        for _ in 0..self.builds {
            tree.new_leaf(taffy::Style::default()).unwrap();
        }
        let node = tree.new_leaf(taffy::Style::default()).unwrap();
        nodes.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: vello::kurbo::Point) {}
    fn draw(
        &mut self,
        _: &mut vello::Scene,
        _: vello::kurbo::Affine,
        _: &mut TextRenderer,
        _: &crate::Theme,
        _: &crate::PresenceCtx,
    ) {
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_enabled(&self) -> bool {
        self.enabled
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
}

#[cfg(test)]
impl crate::undo::UndoPoints for AnonymousFocusProbe {
    fn get_state(&self) -> Option<String> {
        Some(self.state.clone())
    }
    fn set_state(&mut self, state: &str) {
        self.state = state.into();
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
    }

    #[test]
    fn authored_extents_and_rounding_resolve_by_sizing() {
        /// HIG §9.6 in one place: text-sized extents grow by
        /// `ui_scale × text_scale.max(1.0)`, icon-sized ones by `ui_scale`.
        fn authored_extents_resolve_by_their_sizing() {
            let text = AuthoredExtent::text(760.0);
            let icon = AuthoredExtent::icon(86.0);
            assert_eq!(
                (text.resolve(&Theme::dark()), icon.resolve(&Theme::dark())),
                (760.0, 86.0)
            );
            let two = Theme::dark().with_text_scale(2.0);
            assert_eq!((text.resolve(&two), icon.resolve(&two)), (1520.0, 86.0));
            let small = Theme::dark().with_text_scale(0.75);
            assert_eq!(text.resolve(&small), 760.0, "text never shrinks an extent");
            let dense = Theme::dark().with_ui_scale(1.5);
            assert!((text.resolve(&dense) - 1140.0).abs() < 1e-9);
            assert!((icon.resolve(&dense) - 129.0).abs() < 1e-9);
            assert_eq!(text.with_px(300.0), AuthoredExtent::text(300.0));
            assert_eq!(icon.with_px(56.0).sizing(), ExtentSizing::Icon);
            assert_eq!(text.authored(), 760.0);
        }

        /// The authored height holds until the body line outgrows the room it
        /// left; then the line grows and the authored padding is kept.
        fn text_control_height_keeps_the_authored_height_until_the_line_outgrows_it() {
            let stock = Theme::dark();
            assert_eq!(text_control_height(34.0, &stock), 34.0);
            assert_eq!(text_control_height(36.0, &stock), 36.0);

            let line_1x = f64::from(stock.text(TextRole::Body).line_height_px());
            let two = Theme::dark().with_text_scale(2.0);
            let line_2x = f64::from(two.text(TextRole::Body).line_height_px());
            assert!(
                (line_2x - 2.0 * line_1x).abs() < 1e-3,
                "the theme's body line doubles"
            );
            let grown = text_control_height(34.0, &two);
            assert!(
                (grown - (line_2x + (34.0 - line_1x))).abs() < 1e-6,
                "34 px at 1× becomes the 2× line plus the same padding, got {grown}"
            );
            assert!(
                grown > 34.0 && grown < 68.0,
                "grows by the line, not by the whole scale"
            );

            let smaller = Theme::dark().with_text_scale(0.75);
            assert_eq!(
                text_control_height(34.0, &smaller),
                34.0,
                "smaller text never shrinks a control below its authored height"
            );

            let dense = Theme::dark().with_ui_scale(1.5);
            assert!(
                (text_control_height(34.0, &dense) - 51.0).abs() < 1e-6,
                "ui_scale scales the whole control"
            );
        }

        fn manual_fractional_allocation_contract() {
            use crate::widget::{Button, Manual};
            let mut widget = Manual::new(|ctx| {
                ctx.place(0, Rect::new(10.25, 20.75, 130.75, 61.0))?;
                Ok(Size {
                    width: ctx.available().width,
                    height: 100.0,
                })
            })
            .child(Box::new(Button::new("action", "Action", None)));
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = widget.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&widget, &tree, root, &nodes);
            }
            let mut text = crate::font_catalog::fixture_renderer();
            unsafe {
                compute_widget_layout(
                    &mut tree,
                    root,
                    Size {
                        width: AvailableSpace::Definite(400.0),
                        height: AvailableSpace::Definite(300.0),
                    },
                    &nodes,
                    &bindings,
                    &mut text,
                )
                .unwrap();
            }
            widget.apply_layout(&tree, root, Point::ZERO);
            assert_eq!(
                widget.children()[0].get_rect(),
                Rect::new(10.25, 20.75, 130.75, 61.0)
            );
        }

        fn ordinary_hosts_keep_default_pixel_rounding() {
            let mut widget = crate::widget::Container::new()
                .with_no_padding()
                .with_size(100.25, 40.25);
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = widget.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&widget, &tree, root, &nodes);
            }
            unsafe {
                compute_widget_layout(
                    &mut tree,
                    root,
                    Size {
                        width: AvailableSpace::Definite(300.0),
                        height: AvailableSpace::Definite(200.0),
                    },
                    &nodes,
                    &bindings,
                    &mut crate::font_catalog::fixture_renderer(),
                )
                .unwrap();
            }
            widget.apply_layout(&tree, root, Point::ZERO);
            assert_eq!(widget.get_rect(), Rect::new(0.0, 0.0, 100.0, 40.0));
        }

        authored_extents_resolve_by_their_sizing();
        text_control_height_keeps_the_authored_height_until_the_line_outgrows_it();
        manual_fractional_allocation_contract();
        ordinary_hosts_keep_default_pixel_rounding();
    }

    #[test]
    fn style_solver_and_structure_replacements_are_bounded() {
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

        style_solver_bounds_adjustment_cycles_and_rejects_unadmitted_callbacks();
        explicit_structure_replacements_are_bounded_before_measurement();
    }

    #[test]
    fn focus_transfer_refuses_unnamed_ambiguous_or_unadmitted_targets() {
        fn unnamed_focus_has_no_transferable_snapshot_and_keeps_its_own_state() {
            let mut old = AnonymousFocusProbe::new("old selection");
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = old.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&old, &tree, root, &nodes);
            }
            let focus = unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) };
            assert!(
                focus.is_none(),
                "anonymous state must never be captured for transfer"
            );
            nodes.clear();
            bindings.clear();
            drop(old);
            let mut replacement = AnonymousFocusProbe::new("replacement selection");
            let root = replacement.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&replacement, &tree, root, &nodes);
            }
            if let Some(focus) = focus {
                unsafe {
                    focus.restore(&nodes, &bindings, None);
                }
            }
            assert_eq!(replacement.state, "replacement selection");
            assert!(replacement.focused);
        }

        fn current_focus_refuses_unadmitted_disabled_excluded_and_unrelated_siblings() {
            use crate::widget::Column;
            let mut one = AnonymousFocusProbe::new("one");
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = one.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            assert!(unsafe { CompositionFocus::current(&nodes, &bindings, None) }.is_none());
            unsafe {
                bindings.rebuild(&one, &tree, root, &nodes);
            }
            assert_eq!(
                unsafe { CompositionFocus::current(&nodes, &bindings, None) },
                Some(root)
            );
            assert!(unsafe { CompositionFocus::current(&nodes, &bindings, Some(&[])) }.is_none());
            one.enabled = false;
            assert!(unsafe { CompositionFocus::current(&nodes, &bindings, None) }.is_none());
            nodes.clear();
            bindings.clear();
            let mut siblings = Column::new()
                .push(Box::new(AnonymousFocusProbe::new("a")))
                .push(Box::new(AnonymousFocusProbe::new("b")));
            let root = siblings.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&siblings, &tree, root, &nodes);
            }
            assert!(unsafe { CompositionFocus::current(&nodes, &bindings, None) }.is_none());
        }

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
            let focus =
                unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
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

        unnamed_focus_has_no_transferable_snapshot_and_keeps_its_own_state();
        current_focus_refuses_unadmitted_disabled_excluded_and_unrelated_siblings();
        focus_never_uses_an_ordinal_or_an_ambiguous_semantic_key();
        duplicate_source_key_cannot_transfer_focus_to_the_surviving_other_control();
        review_disabled_duplicate_target_does_not_transfer_focus_to_enabled_other();
    }

    #[test]
    fn compound_editor_focus_follows_named_identity_across_rebuilds() {
        fn compound_editor_focus_survives_rebuilt_or_split_focused_line() {
            use crate::widget::BlockEditor;
            for capture_leaf in [false, true] {
                let mut editor = BlockEditor::from_markdown("first");
                editor.set_focus(true);
                let mut tree = TaffyTree::new();
                let mut nodes = HashMap::new();
                let root = editor.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
                let selected = if capture_leaf {
                    tree.children(root).unwrap()[0]
                } else {
                    root
                };
                let mut bindings = BoundNodeMap::new();
                unsafe {
                    bindings.rebuild(&editor, &tree, root, &nodes);
                }
                let focus = unsafe { CompositionFocus::capture(Some(selected), &nodes, &bindings) }
                    .unwrap();
                nodes.clear();
                bindings.clear();
                editor = BlockEditor::from_markdown("first\n\nsecond");
                editor.set_focus(true);
                let root = editor.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
                unsafe {
                    bindings.rebuild(&editor, &tree, root, &nodes);
                }
                assert_eq!(
                    unsafe { focus.restore(&nodes, &bindings, None) },
                    Some(root)
                );
                assert!(editor.is_focused());
                assert!(editor.blocks()[0].line.is_focused());
                assert_eq!(
                    unsafe { CompositionFocus::current(&nodes, &bindings, None) },
                    Some(root)
                );
            }
        }

        fn title_focus_uses_named_identity_across_editing_delegate_replacement() {
            use crate::widget::EditableLabel;
            let mut old = EditableLabel::new("Title").with_id("note_title");
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = old.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&old, &tree, root, &nodes);
            }
            let focus =
                unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
            nodes.clear();
            bindings.clear();
            let mut rebuilt = EditableLabel::new("Title").with_id("note_title");
            rebuilt.begin_edit();
            let root = rebuilt.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&rebuilt, &tree, root, &nodes);
            }
            assert_eq!(
                unsafe { focus.restore(&nodes, &bindings, None) },
                Some(root)
            );
            assert!(rebuilt.is_editing());
            assert_eq!(rebuilt.take_outcome(), crate::widget::EditOutcome::None);
        }

        fn duplicate_named_editor_or_removed_editor_cannot_receive_focus() {
            use crate::widget::{BlockEditor, Column};
            for duplicate in [false, true] {
                let mut old = BlockEditor::from_markdown("old");
                old.set_focus(true);
                let mut tree = TaffyTree::new();
                let mut nodes = HashMap::new();
                let root = old.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
                let mut bindings = BoundNodeMap::new();
                unsafe {
                    bindings.rebuild(&old, &tree, root, &nodes);
                }
                let focus =
                    unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
                nodes.clear();
                bindings.clear();
                let mut rebuilt = Column::new();
                if duplicate {
                    rebuilt = rebuilt
                        .push(Box::new(BlockEditor::from_markdown("one")))
                        .push(Box::new(BlockEditor::from_markdown("two")));
                }
                let root = rebuilt.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
                unsafe {
                    bindings.rebuild(&rebuilt, &tree, root, &nodes);
                }
                assert!(unsafe { focus.restore(&nodes, &bindings, None) }.is_none());
                assert!(rebuilt.children().iter().all(|child| !child.is_focused()));
            }
        }

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
            // The editor owns its lines' layout, so its node is the one focused
            // node while the line it routes text to holds the caret.
            assert_eq!(
                nodes
                    .values()
                    .filter(|&&pointer| unsafe { (*pointer).is_focused() })
                    .count(),
                1
            );
            let focus =
                unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
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

        compound_editor_focus_survives_rebuilt_or_split_focused_line();
        title_focus_uses_named_identity_across_editing_delegate_replacement();
        duplicate_named_editor_or_removed_editor_cannot_receive_focus();
        compound_editor_restores_owner_and_inner_text_focus_after_rebuild();
    }

    #[test]
    fn canonical_tab_focus_survives_reorder_but_never_transfers_to_a_remaining_peer() {
        use crate::widget::{
            Column,
            tabs::{TabBar, TabDescriptor},
        };
        let bar = |id| {
            Box::new(TabBar::new(
                &format!("g{id}"),
                vec![TabDescriptor::new(id, "Tab")],
                id,
            )) as Box<dyn Widget>
        };
        let mut old = Column::new().push(bar(3)).push(bar(4));
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let mut bindings = BoundNodeMap::new();
        let root = old.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
        unsafe {
            bindings.rebuild(&old, &tree, root, &nodes);
        }
        let source = *nodes
            .keys()
            .find(|&&node| unsafe {
                crate::bound_dispatch::inspect_bound_node(&bindings, &nodes, node, |widget| {
                    widget
                        .accessibility()
                        .and_then(|a| a.semantic_key)
                        .as_deref()
                        == Some("tabbar:g3")
                })
                .unwrap_or(false)
            })
            .unwrap();
        let focus = unsafe { CompositionFocus::capture(Some(source), &nodes, &bindings) }.unwrap();
        for ids in [vec![4, 3], vec![4]] {
            nodes.clear();
            bindings.clear();
            let mut rebuilt = Column::new();
            for id in &ids {
                rebuilt = rebuilt.push(bar(*id));
            }
            let root = rebuilt.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&rebuilt, &tree, root, &nodes);
            }
            let restored = unsafe { focus.restore(&nodes, &bindings, None) };
            assert_eq!(restored.is_some(), ids.contains(&3));
            if let Some(node) = restored {
                assert!(unsafe {
                    crate::bound_dispatch::inspect_bound_node(&bindings, &nodes, node, |widget| {
                        widget
                            .accessibility()
                            .and_then(|a| a.semantic_key)
                            .as_deref()
                            == Some("tabbar:g3")
                    })
                    .unwrap()
                });
            }
        }
    }
}

/// Preserve legacy overflow extents except on axes with an exact absolute
/// allocation. Culling continues to use the separate content bounds.
pub(super) fn allocation_or_content_size(
    tree: &TaffyTree<()>,
    node: NodeId,
    layout: &taffy::Layout,
) -> Size<f32> {
    let exact = tree
        .style(node)
        .ok()
        .filter(|style| style.position == taffy::Position::Absolute);
    Size {
        width: if exact.is_some_and(|style| {
            style.min_size.width.into_option().is_some()
                && style.min_size.width == style.max_size.width
        }) {
            layout.size.width
        } else {
            layout.size.width.max(layout.content_size.width)
        },
        height: if exact.is_some_and(|style| {
            style.min_size.height.into_option().is_some()
                && style.min_size.height == style.max_size.height
        }) {
            layout.size.height
        } else {
            layout.size.height.max(layout.content_size.height)
        },
    }
}
