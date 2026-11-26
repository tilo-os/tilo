//! Shared prepaint solve for retained widgets with allocation-dependent styles, and the
//! small inset helpers every `Style` builder used to spell by hand.

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{AvailableSpace, Dimension, LengthPercentage, NodeId, Size, TaffyTree};

use super::Widget;
use crate::Theme;
use crate::text_render::TextRenderer;
use crate::tokens::TextRole;
use crate::widget::bound_nodes::{BoundNodeMap, layout_bound_node, mutate_bound_node};

pub mod counts;

/// What sizes an extent handed to a responsive shell (HIG §9.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtentSizing {
    Text,
    Icon,
}

/// A breakpoint or pane width **authored at 1×** for a painted app's
/// [`crate::widget::SidebarDrawer`], resolved against the theme once when the tree is
/// built.
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

    /// [`Self::resolve`] from the two factors, for a helper that caches them instead of
    /// holding a theme (`SidebarDrawer`).
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
/// (`typography.size(TextRole::Body)` at the current text scale) outgrows the height the
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

/// Symmetric padding: `horizontal` on left and right, `vertical` on top and bottom — the
/// toolbar / action-row shape.
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

/// The size of a view that takes the width it is given and a height from `aspect` (width over
/// height) where nothing else gives it one: a player or a page in a column that grows with what
/// it holds, which would otherwise stand zero tall there and paint over what follows.
#[must_use]
pub fn natural_shape(
    known: taffy::Size<Option<f32>>,
    available: taffy::Size<taffy::AvailableSpace>,
    aspect: f32,
) -> taffy::Size<f32> {
    let width = known.width.or(match available.width {
        taffy::AvailableSpace::Definite(width) => Some(width),
        taffy::AvailableSpace::MinContent | taffy::AvailableSpace::MaxContent => None,
    });
    // A pass that sizes the parent by its content asks before any width is known: the view asks
    // for no width of its own there, and the height of a typical column's width.
    // A reading column's width stands for the one not yet known.
    let across = width.unwrap_or(crate::tokens::Metrics::BASE.reading_measure);
    let height = known
        .height
        .unwrap_or(if aspect > 0.0 { across / aspect } else { 0.0 });
    taffy::Size {
        width: width.unwrap_or(0.0),
        height,
    }
}

/// The style of a view that fills what its parent gives it and, where its parent gives it no
/// height, stands at the size its `measure` names (a player's or a page's [`natural_shape`]).
///
/// It is as wide as its parent, and its height is its own until a parent stretches, grows or
/// shrinks it: a percentage height would resolve against a parent that grows with its content,
/// and stand taller than that parent grew for it.
#[must_use]
pub fn natural_filling() -> taffy::Style {
    taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::auto(),
        },
        min_size: taffy::Size {
            width: taffy::Dimension::length(0.0),
            height: taffy::Dimension::length(0.0),
        },
        flex_grow: 1.0,
        flex_shrink: 1.0,
        ..Default::default()
    }
}

/// `style` as a flex item that fills its share of what its parent's main axis leaves:
/// grown by `weight` from a zero basis, and measured at that share rather than at its content's
/// min-content size.
///
/// In a flex container an `auto` minimum is the content's min-content size, so an item that only
/// grows never shrinks below its content: a scrolling page under a fixed strip grows to its whole
/// content instead of taking what is left and scrolling inside it. A filling item takes exactly
/// its share; what it holds scrolls or clips inside it, and never pushes the parent past the size
/// the parent was given. An item that genuinely may not shrink below its content says so with an
/// explicit `min_size` on that axis, before or after this call.
///
/// The parent's main axis is not known where a widget builds its own style, so a filling item
/// keeps a zero minimum on both axes: growing is the main axis of whichever parent it fills,
/// and on the cross axis the rule is the same — it takes the size it is stretched to.
#[must_use]
pub fn filling(mut style: taffy::Style, weight: f32) -> taffy::Style {
    style.flex_grow = weight;
    // A weight of zero takes no share: the item is sized as it asks (its
    // `size`, its content), and its minimum stays its own.
    if weight > 0.0 {
        style.flex_basis = taffy::Dimension::length(0.0);
        if style.min_size.width == taffy::Dimension::auto() {
            style.min_size.width = taffy::Dimension::length(0.0);
        }
        if style.min_size.height == taffy::Dimension::auto() {
            style.min_size.height = taffy::Dimension::length(0.0);
        }
    }
    style
}

thread_local! {
    static LAYOUT_CLIP: std::cell::Cell<Option<vello::kurbo::Rect>> =
        const { std::cell::Cell::new(None) };
}

/// The window area content is seen through while it is being placed: the
/// viewport of the nearest enclosing scroll view (intersected with any
/// outer one), or `None` when nothing clips it. A collection reads it in
/// [`Widget::apply_layout`] to keep only the rows near what is shown laid
/// out; drawing culls against `PresenceCtx::should_cull` instead. It is in
/// effect too while a scroll view collects its content's damage and while
/// the host plans its copies ([`Widget::for_each_child_in_clip`]).
#[must_use]
pub fn layout_clip() -> Option<vello::kurbo::Rect> {
    LAYOUT_CLIP.with(std::cell::Cell::get)
}

/// Run `place` (a scroll view placing its content) with `clip` as the [`layout_clip`],
/// narrowed by any clip already in effect.
pub fn with_layout_clip<T>(clip: vello::kurbo::Rect, place: impl FnOnce() -> T) -> T {
    struct Restore(Option<vello::kurbo::Rect>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LAYOUT_CLIP.with(|cell| cell.set(self.0));
        }
    }
    let outer = layout_clip();
    let clip = outer.map_or(clip, |outer| outer.intersect(clip));
    let _restore = Restore(LAYOUT_CLIP.with(|cell| cell.replace(Some(clip))));
    place()
}

thread_local! {
    static DEVICE_SCALE: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

/// The device pixels per logical pixel of the display a layout is for: every tree the
/// toolkit solves rounds its edges onto that grid ([`solve_layout`]), so an edge lands on a
/// device pixel at 1.5 as at 2. 1 outside a host's layout pass.
#[must_use]
pub fn device_scale() -> f32 {
    DEVICE_SCALE.with(std::cell::Cell::get)
}

/// `value` on the device pixels of the display a layout is for
/// ([`device_scale`]): where a widget's own arithmetic — a scroll offset, a
/// camera, a measured gap — must land, since the solver rounds only the trees
/// it solves itself. A length that is no whole number of device pixels is the
/// nearest one, so what is drawn from it stands crisp rather than across two.
#[must_use]
pub fn snap_to_device_pixels(value: f64) -> f64 {
    let grid = f64::from(device_scale());
    (value * grid).round() / grid
}

/// Lay out at `scale` device pixels per logical pixel until the guard is
/// dropped: a host enters it around its whole layout pass (solving and
/// placing, with every tree a widget solves of its own), with its display's
/// scale. A scale that is not a finite positive number is 1.
#[must_use = "the scale holds while the guard lives"]
pub fn enter_device_scale(scale: f64) -> DeviceScaleGuard {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a display scale is a small positive number"
    )]
    let scale = if scale.is_finite() && scale > 0.0 {
        scale as f32
    } else {
        1.0
    };
    DeviceScaleGuard(DEVICE_SCALE.with(|cell| cell.replace(scale)))
}

/// Restores the device scale in effect before [`enter_device_scale`].
pub struct DeviceScaleGuard(f32);

impl Drop for DeviceScaleGuard {
    fn drop(&mut self) {
        DEVICE_SCALE.with(|cell| cell.set(self.0));
    }
}

thread_local! {
    static PLACING_AGAIN: std::cell::Cell<Option<*const ()>> = const { std::cell::Cell::new(None) };
}

/// The styles a widget reads and sets of its own node and its children's: in a tree before or after
/// a solve, or within one as the node is solved ([`taffy::Restyle`]).
pub trait Styles {
    fn style_of(&self, node: NodeId) -> Option<&taffy::Style>;
    /// Give `node` `style`; whether it took.
    fn set_style_of(&mut self, node: NodeId, style: taffy::Style) -> bool;
    fn first_child_of(&self, node: NodeId) -> Option<NodeId>;
}

impl Styles for TaffyTree<()> {
    fn style_of(&self, node: NodeId) -> Option<&taffy::Style> {
        self.style(node).ok()
    }

    fn set_style_of(&mut self, node: NodeId, style: taffy::Style) -> bool {
        self.set_style(node, style).is_ok()
    }

    fn first_child_of(&self, node: NodeId) -> Option<NodeId> {
        self.child_at_index(node, 0).ok()
    }
}

impl Styles for taffy::Restyle<'_> {
    fn style_of(&self, node: NodeId) -> Option<&taffy::Style> {
        Some(self.style(node))
    }

    fn set_style_of(&mut self, node: NodeId, style: taffy::Style) -> bool {
        self.set_style(node, style);
        true
    }

    fn first_child_of(&self, node: NodeId) -> Option<NodeId> {
        self.children(node).first().copied()
    }
}

/// A layout tree by address, to name it to [`placing_again_in`] while the tree's owner is
/// borrowed to place its widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeId(*const ());

impl TreeId {
    pub fn of(taffy: &TaffyTree<()>) -> Self {
        Self(std::ptr::from_ref(taffy).cast::<()>())
    }
}

/// Run `place` (the host placing its widgets) knowing that nothing of the
/// tree `tree` names was solved, built or rebuilt since its widgets were
/// last placed from it: only where things stand moved (a scroll, a glide),
/// so every box stands where it stood relative to its parent. A long
/// collection then places only what is near its [`layout_clip`]
/// ([`placing_again`]).
pub fn placing_again_in<T>(tree: TreeId, place: impl FnOnce() -> T) -> T {
    struct Restore(Option<*const ()>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PLACING_AGAIN.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(PLACING_AGAIN.with(|cell| cell.replace(Some(tree.0))));
    place()
}

/// Maximum `rect.y1` across a widget subtree, for finding the true bottom edge when taffy's
/// `content_size` under-reports.
pub fn max_bottom_edge(w: &dyn super::Widget) -> f64 {
    let mut deepest = w.get_rect().y1;
    if w.child_clip_rect().is_some() {
        return deepest;
    }
    for c in w.children() {
        deepest = deepest.max(max_bottom_edge(c));
    }
    deepest
}

/// Whether `event` happens at a place in the window (a pointer, a touch),
/// so only what stands there may take it.
pub fn at_a_place(event: &winit::event::WindowEvent) -> bool {
    use winit::event::WindowEvent;
    matches!(
        event,
        WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_)
    )
}

/// From [`Widget::apply_layout`]: whether `taffy` is a tree its widgets are being placed
/// again from unchanged (`placing_again_in`).
#[must_use]
pub fn placing_again(taffy: &TaffyTree<()>) -> bool {
    PLACING_AGAIN.with(std::cell::Cell::get) == Some(TreeId::of(taffy).0)
}

#[derive(Debug, thiserror::Error)]
pub enum WidgetLayoutError {
    #[error(transparent)]
    Taffy(#[from] taffy::TaffyError),
    #[error(
        "widget layout styles did not settle after eight adjustments in {available}; still \
         changing: {}; solved sizes by pass: {}",
        still_changing.join(", "),
        passes.join("; ")
    )]
    DidNotSettle {
        still_changing: Vec<&'static str>,
        passes: Vec<String>,
        available: String,
    },
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

/// The frames a host skipped because their layout failed, and when it last
/// said so. Every host (native, headless, the browser) skips such a frame
/// and keeps showing the last one, so input still lands and the next frame
/// lays out again. A layout that keeps failing would fail on every frame,
/// so a failure is reported once, in full; the same failure again says
/// nothing, and another one is reported with how many frames were skipped
/// since the last report.
#[derive(Debug, Default)]
pub struct SkippedFrames {
    reported: Option<String>,
    unreported: u64,
}

impl SkippedFrames {
    /// The failure last reported, as its error says it: what a host that keeps the report for a
    /// reader compares against to tell a new failure from one it already said.
    #[must_use]
    pub fn reported(&self) -> Option<&str> {
        self.reported.as_deref()
    }

    /// Note a frame skipped for `error`; the line to log, when this failure
    /// was not reported already.
    pub fn skip(&mut self, error: &WidgetLayoutError) -> Option<String> {
        let said = error.to_string();
        if self.reported.as_deref() == Some(said.as_str()) {
            self.unreported = self.unreported.saturating_add(1);
            return None;
        }
        let line = match std::mem::take(&mut self.unreported) {
            0 => format!("widget layout failed; skipping the frame: {said}"),
            skipped => format!(
                "widget layout failed; skipping the frame, after {skipped} frames skipped \
                 without a report: {said}"
            ),
        };
        self.reported = Some(said);
        Some(line)
    }
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
/// Returns a Taffy error (or a manual region's own error); callers must skip
/// applying/painting a failed layout. Styles that keep changing after eight
/// adjustments are no failure: the layout is solved once more as the last
/// pass left them, placed and drawn, and reported once
/// ([`take_unsettled_layout`]), so one part that does not settle never stops
/// the window it stands in.
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

/// Solve a layout [`Widget::build_layout`] just produced, admitting the tree's
/// nodes for the call.
///
/// A caller that lays the tree out once — a shot, a benchmark frame, a test
/// fixture — uses this instead of a bare Taffy solve, which answers no widget's
/// [`Widget::layout_measured`] and leaves every widget that asks layout-dirty.
///
/// # Errors
/// The same as [`compute_widget_layout`].
///
/// # Safety
/// `root`, `taffy`, `node` and `nodes` must be one live tree that the caller
/// exclusively owns throughout the call, as [`compute_widget_layout`] requires.
#[allow(
    clippy::implicit_hasher,
    reason = "bound dispatch uses the existing default-hasher node map"
)]
pub unsafe fn solve_built_layout(
    root: &dyn Widget,
    taffy: &mut TaffyTree<()>,
    node: NodeId,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    text: &mut TextRenderer,
    available: Size<AvailableSpace>,
) -> Result<(), WidgetLayoutError> {
    let mut bindings = BoundNodeMap::new();
    // SAFETY: forwards the caller's exact live-tree ownership contract.
    unsafe {
        bindings.rebuild(root, taffy, node, nodes);
        compute_widget_layout(taffy, node, available, nodes, &bindings, text)
    }
}

/// A node's measured size from its known dimensions and the space offered.
pub(crate) type MeasureNode<'a> =
    dyn FnMut(Size<Option<f32>>, Size<AvailableSpace>, NodeId) -> Size<f32> + 'a;

/// Solve `taffy` from `root`, measuring leaves with `measure`.
#[inline(never)]
pub fn solve_layout(
    taffy: &mut TaffyTree<()>,
    root: NodeId,
    available: Size<AvailableSpace>,
    measure: &mut MeasureNode<'_>,
) -> taffy::TaffyResult<()> {
    taffy.set_rounding_scale(device_scale());
    taffy.compute_layout_with_measure(root, available, |known, available, node, _, _| {
        measure(known, available, node)
    })
}

/// Solve `taffy` from `root` as the widgets it lays out ask: each leaf measured by its widget, and
/// each node whose widget styles it for its width ([`Widget::style_for_width`]) styled for every
/// width the solve gives it, before it is solved there.
///
/// # Safety
/// The same live-tree contract as [`compute_widget_layout`]: each callback is exclusive, and bound
/// dispatch checks admission and lifetime.
unsafe fn solve_widgets(
    taffy: &mut TaffyTree<()>,
    root: NodeId,
    available: Size<AvailableSpace>,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    bindings: &BoundNodeMap,
    text: &mut TextRenderer,
) -> taffy::TaffyResult<()> {
    taffy.set_rounding_scale(device_scale());
    // Taffy measures a leaf and styles a node one at a time, never inside each other.
    let text = std::cell::RefCell::new(text);
    // A node no widget is bound to (a container's wrapper around one of its parts) is styled by
    // the widget whose node holds it.
    let mut restyle =
        |node: NodeId, solving: taffy::Solving<'_>, styles: &mut taffy::Restyle<'_>| {
            let owner = if nodes.contains_key(&node) {
                node
            } else if let Some(parent) = styles.parent(node) {
                parent
            } else {
                return false;
            };
            // SAFETY: the caller's exact live-tree ownership contract.
            unsafe {
                layout_bound_node(bindings, nodes, owner, |widget| match solving {
                    taffy::Solving::Before(width) => {
                        widget.style_for_width(styles, width, &mut text.borrow_mut());
                        false
                    }
                    taffy::Solving::Laid(laid) => widget.style_laid_out(styles, laid),
                })
            }
            .unwrap_or(false)
        };
    taffy.compute_layout_with_measure_and_restyle(
        root,
        available,
        |known, available, node, _, _| {
            // SAFETY: the caller's exact live-tree ownership contract.
            unsafe {
                layout_bound_node(bindings, nodes, node, |widget| {
                    crate::widget::counts::measure();
                    widget.measure(known, available, &mut text.borrow_mut())
                })
            }
            .unwrap_or(Size::ZERO)
        },
        &mut restyle,
    )
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
    let started = (depth == 0).then(crate::time::now);
    // SAFETY: forwards the caller's exact live-tree ownership contract.
    let settled =
        unsafe { solve_widget_layout(taffy, root, available, nodes, bindings, text, depth) };
    crate::widget::counts::solved(started.map_or(0, |started| {
        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }));
    settled
}

/// One solve's settlement, counted by [`compute_widget_layout_depth`].
///
/// # Safety
/// The same live-tree contract as [`compute_widget_layout`].
unsafe fn solve_widget_layout(
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
    let mut sized = Vec::with_capacity(nodes.len());
    let mut sweep = crate::widget::bound_nodes::NodeSweep::new(bindings, nodes);
    while let Some(node) = pending.pop() {
        if nodes.contains_key(&node) {
            sized.push(node);
        }
        // SAFETY: the caller's exact live-tree ownership contract.
        let found = unsafe {
            sweep.inspect(node, |widget| {
                (widget.manual_layout_policy(), widget.layout_needs_measure())
            })
        }
        .ok()
        .and_then(|(policy, needs_measure)| policy.map(|policy| (policy, needs_measure)));
        if let Some((policy, needs_measure)) = found {
            policies.push((node, policy, needs_measure));
        }
        pending.extend(taffy.children(node)?.into_iter().rev());
    }
    // Intrinsic content can change without changing a Taffy style or topology.
    // Invalidate the measured node itself: marking only the root leaves cached
    // descendant heights intact and clips paragraphs as they wrap while typing.
    let mut asked: Vec<NodeId> = Vec::new();
    for &node in nodes.keys() {
        // SAFETY: use the same admission/lifetime checks as measurement below.
        let needs_measure =
            unsafe { sweep.layout(node, |widget| widget.layout_needs_measure()) }.unwrap_or(false);
        if needs_measure {
            asked.push(node);
            if depth == 0 {
                crate::widget::counts::marked(
                    taffy
                        .children(node)
                        .is_ok_and(|children| children.is_empty()),
                );
            }
            taffy.mark_dirty(node)?;
        }
    }
    // What each widget knows of its own size goes into the first solve, so a solve does not have
    // to be solved again for it: in the window's tree, and in a probe's, whose parts a region styles
    // as it built them.
    // Children are asked before the containers that hold them, so a container reads what its
    // children decided (whether one is in the flow) and styles its parts for it.
    let mut asked_sizes = std::collections::HashSet::with_capacity(nodes.len());
    for node in sized.into_iter().rev().chain(nodes.keys().copied()) {
        if !asked_sizes.insert(node) {
            continue;
        }
        // SAFETY: the same admission/lifetime checks as measurement below.
        let _ = unsafe { sweep.layout(node, |widget| widget.size_before_solve(taffy, node, text)) };
    }
    drop(sweep);
    // A region is solved again only when something it reads moved: the
    // allocation it is handed, a measure inside its own subtree, or a revision
    // its owner asked for. `reusable` is that question, one answer per region:
    // a still window keeps every solve it made.
    let mut probes = super::manual::Probes::default();
    let mut reusable = Vec::with_capacity(policies.len());
    let mut regions = HashMap::with_capacity(policies.len());
    for (index, (node, policy, needs_measure)) in policies.iter().enumerate() {
        let reset = super::manual::reset_region(policy, taffy, *node)?;
        reusable.push(depth == 0 && !reset && !needs_measure);
        regions.insert(*node, index);
    }
    if depth == 0 {
        for (index, (node, _, _)) in policies.iter().enumerate() {
            if !reusable[index] {
                mark_enclosing_regions(taffy, &regions, &mut reusable, *node);
            }
        }
    }
    let mut still_changing = Vec::new();
    let mut recent: std::collections::VecDeque<Vec<(NodeId, String)>> =
        std::collections::VecDeque::new();
    for _ in 0..8 {
        if !still_changing.is_empty() {
            if recent.len() == 3 {
                recent.pop_front();
            }
            recent.push_back(
                still_changing
                    .iter()
                    .map(|&node| (node, solved_node(taffy, node)))
                    .collect(),
            );
        }
        still_changing.clear();
        crate::widget::counts::pass();
        // SAFETY: the caller's exact live-tree ownership contract.
        unsafe { solve_widgets(taffy, root, available, nodes, bindings, text) }?;
        let mut changed = false;
        for (index, (node, policy, _)) in policies.iter().enumerate() {
            if reusable[index] && policy.settled_at(taffy.unrounded_layout(*node).size) {
                crate::widget::counts::kept();
                continue;
            }
            let binding = crate::widget::component_walk::node_binding(bindings, *node);
            let Some(_owner) = crate::widget::component_walk::enter(binding.as_ref()) else {
                continue;
            };
            crate::widget::counts::region();
            let placed = unsafe {
                super::manual::run_policy(
                    policy,
                    taffy,
                    *node,
                    nodes,
                    bindings,
                    text,
                    depth,
                    &mut probes,
                )?
            };
            if placed {
                still_changing.push(*node);
                mark_enclosing_regions(taffy, &regions, &mut reusable, *node);
            }
            changed |= placed;
        }
        let mut sweep = crate::widget::bound_nodes::NodeSweep::new(bindings, nodes);
        for &node in nodes.keys() {
            // SAFETY: same contract as measurement; no topology mutation is
            // permitted by adjust_layout_style.
            let adjusted = unsafe {
                sweep.layout(node, |widget| {
                    let adjusted = widget.adjust_layout_style(taffy, node, text);
                    if adjusted {
                        crate::widget::counts::restyled(widget.concrete_type_name());
                    }
                    adjusted
                })
            }
            .unwrap_or(false);
            if adjusted {
                still_changing.push(node);
                mark_enclosing_regions(taffy, &regions, &mut reusable, node);
            }
            changed |= adjusted;
        }
        drop(sweep);
        if !changed {
            // The solve has read every size that asked for one, so the
            // requests are met here rather than by a draw that may never
            // come: a widget placed and not drawn (a shut overflow API's
            // controls, a row a scroll hides) would otherwise ask again on
            // every frame after, and re-measure its path to the root.
            let mut sweep = crate::widget::bound_nodes::NodeSweep::new(bindings, nodes);
            for node in std::mem::take(&mut asked) {
                // SAFETY: the caller's exact live-tree ownership contract.
                let _ = unsafe { sweep.layout(node, |widget| widget.layout_measured()) };
            }
            return Ok(());
        }
    }
    if recent.len() == 3 {
        recent.pop_front();
    }
    recent.push_back(
        still_changing
            .iter()
            .map(|&node| (node, solved_node(taffy, node)))
            .collect(),
    );
    let passes = still_changing
        .iter()
        .map(|&node| {
            let sizes: Vec<String> = recent
                .iter()
                .filter_map(|pass| pass.iter().find(|(at, _)| *at == node))
                .map(|(_, solved)| solved.clone())
                .collect();
            // SAFETY: the caller's exact live-tree ownership contract.
            let name = unsafe { widget_name(bindings, nodes, node) };
            format!("{name} {}", sizes.join(" -> "))
        })
        .collect();
    let mut still_changing: Vec<&'static str> = still_changing
        .into_iter()
        .map(|node| unsafe { widget_name(bindings, nodes, node) })
        .collect();
    still_changing.sort_unstable();
    still_changing.dedup();
    let error = WidgetLayoutError::DidNotSettle {
        still_changing,
        passes,
        available: format!("{available:?}"),
    };
    // A probe (a region measuring what it holds) says so to the region, which
    // may measure another way; drawing it as it stood is the window's to do.
    // Every probe of every pass of the window's solve finishing its own eight
    // passes would multiply them, region inside region.
    if depth > 0 {
        return Err(error);
    }
    // The window is laid out as the last pass left its styles: one more solve
    // makes every box agree with them, and that layout is what is placed and
    // drawn. A part that kept changing shows as it last stood rather than
    // stopping every frame of the window it is in; it is reported once
    // ([`take_unsettled_layout`]).
    // SAFETY: as in the passes above.
    unsafe { solve_widgets(taffy, root, available, nodes, bindings, text) }?;
    let mut sweep = crate::widget::bound_nodes::NodeSweep::new(bindings, nodes);
    for node in asked {
        // SAFETY: the caller's exact live-tree ownership contract.
        let _ = unsafe { sweep.layout(node, |widget| widget.layout_measured()) };
    }
    drop(sweep);
    UNSETTLED.with(|held| {
        held.borrow_mut().get_or_insert(error);
    });
    Ok(())
}

thread_local! {
    /// The first solve since it was last taken whose styles did not settle.
    static UNSETTLED: std::cell::RefCell<Option<WidgetLayoutError>> =
        const { std::cell::RefCell::new(None) };
    /// The parts already reported as not settling, by what kept changing.
    static UNSETTLED_REPORTED: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// The first layout on this thread, since this was last asked, whose styles
/// kept changing after eight adjustments
/// ([`WidgetLayoutError::DidNotSettle`]). Such a layout is still placed and
/// drawn as its last pass left it, so the window around it keeps working.
#[must_use]
pub fn take_unsettled_layout() -> Option<WidgetLayoutError> {
    UNSETTLED.with(|held| held.borrow_mut().take())
}

/// [`take_unsettled_layout`] as the line a host logs: only the first time
/// what kept changing is seen on this thread, as a part that does not settle
/// would otherwise say so on every frame. A host asks once its frame is laid
/// out.
#[must_use]
pub fn take_unsettled_report() -> Option<String> {
    let error = take_unsettled_layout()?;
    let WidgetLayoutError::DidNotSettle { still_changing, .. } = &error else {
        return None;
    };
    let key = still_changing.join(", ");
    UNSETTLED_REPORTED
        .with(|reported| reported.borrow_mut().insert(key))
        .then(|| format!("widget layout drawn as its last pass left it: {error}"))
}

fn mark_enclosing_regions(
    taffy: &TaffyTree<()>,
    regions: &HashMap<NodeId, usize>,
    reusable: &mut [bool],
    node: NodeId,
) {
    let mut current = Some(node);
    while let Some(at) = current {
        if let Some(&index) = regions.get(&at) {
            reusable[index] = false;
        }
        current = taffy.parent(at);
    }
}

fn solved_node(taffy: &TaffyTree<()>, node: NodeId) -> String {
    let layout = taffy.unrounded_layout(node);
    let min_height = taffy
        .style(node)
        .map_or(Dimension::auto(), |style| style.min_size.height)
        .into_raw();
    let min_height = match min_height.tag() {
        taffy::CompactLength::LENGTH_TAG => format!("{}px", min_height.value()),
        taffy::CompactLength::PERCENT_TAG => format!("{}%", min_height.value() * 100.0),
        _ => "auto".to_owned(),
    };
    format!(
        "{}x{} (min height {min_height}, padding right {})",
        layout.size.width, layout.size.height, layout.padding.right
    )
}

/// The concrete type of the widget at `node`, for naming it in an error.
///
/// # Safety
/// The same live-tree contract as [`compute_widget_layout`].
unsafe fn widget_name(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
) -> &'static str {
    // SAFETY: forwards the caller's exact live-tree ownership contract.
    unsafe { layout_bound_node(bindings, nodes, node, |widget| widget.concrete_type_name()) }
        .unwrap_or("an unbound widget")
}

/// Whether any widget under `root` composes from its allocation (see
/// [`Widget::composition_follows_allocation`]): only then does a resize
/// recompose the tree rather than re-solve it. History is no such tree: its
/// chrome rebuilds its own bar in place when the window crosses the width
/// class it presents in (`set_width`), and the refresh that does it asks for
/// the tree rebuilt around the controls it replaced.
pub fn tree_composition_follows_allocation(root: &(impl Widget + ?Sized)) -> bool {
    root.composition_follows_allocation() || {
        let mut found = false;
        root.for_each_child(&mut |child| {
            found = found || tree_composition_follows_allocation(child);
        });
        found
    }
}

/// A solved tree in traversal order, independent of allocated `NodeId`s. Include
/// topology as well as geometry: equal root rectangles alone do not prove that
/// a geometry-dependent child's composition has caught up with its allocation.
pub fn composition_geometry(
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
pub fn settle_widget_composition(
    viewport_changed: bool,
    mut pass: impl FnMut(bool) -> Result<(Option<CompositionGeometry>, bool), WidgetLayoutError>,
) -> Result<(), WidgetLayoutError> {
    let _frame = crate::motion::pin_frame();
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
pub struct CompositionFocus {
    pub ambiguous: bool,
    pub key: Option<String>,
    semantic_key: Option<String>,
    qualified_semantic: bool,
    binding: Option<crate::widget::component_walk::Binding>,
    state: Option<String>,
}
impl CompositionFocus {
    fn key(widget: &dyn Widget) -> Option<String> {
        widget
            .test_id()
            .map(|id| format!("named:{id}"))
            .or_else(|| {
                widget
                    .accessibility_root()
                    .and_then(|access| access.semantic_key)
                    .map(|key| format!("semantic:{key}"))
            })
    }
    /// The semantic key a widget's [`Self::key`] names, without building its accessibility node:
    /// a named widget's is read only when its name alone is not unique.
    fn semantic_of(key: Option<&str>) -> Option<String> {
        key?.strip_prefix("semantic:").map(str::to_owned)
    }
    /// Whether `widget`'s [`Self::key`] is `key`. A name is compared without
    /// building the widget's accessibility node (and resolving its translated
    /// label), which a rebuild would otherwise do for every widget.
    fn has_key(widget: &dyn Widget, key: &str) -> bool {
        if let Some(name) = key.strip_prefix("named:") {
            return widget.test_id() == Some(name);
        }
        key.strip_prefix("semantic:").is_some_and(|semantic| {
            widget.test_id().is_none()
                && widget
                    .accessibility_root()
                    .and_then(|access| access.semantic_key)
                    .as_deref()
                    == Some(semantic)
        })
    }
    /// Recover focus from the retained tree after a structural mutation. No
    /// pointer from the previous layout is read; owner ownership follows the
    /// same component boundaries as the next admitted topology.
    pub fn capture_live(root: &dyn Widget) -> Option<Self> {
        type Visit<'v> =
            dyn FnMut(&dyn Widget, &Option<crate::widget::component_walk::Binding>, bool) + 'v;
        fn walk(
            widget: &dyn Widget,
            binding: Option<crate::widget::component_walk::Binding>,
            focused_ancestor: bool,
            visit: &mut Visit<'_>,
        ) {
            let Ok(_entry) = crate::widget::bound_nodes::enter_widget(widget) else {
                return;
            };
            let reset = widget.resets_binding_inheritance();
            let own_binding = crate::widget::component_walk::binding_of(widget);
            let binding = if reset {
                own_binding
            } else {
                own_binding.or(binding)
            };
            let focused = widget.is_focusable() && widget.is_enabled() && widget.is_focused();
            visit(widget, &binding, focused && !focused_ancestor);
            widget.for_each_child(&mut |child| {
                walk(child, binding.clone(), focused_ancestor || focused, visit);
            });
        }
        // The unique focus target first: only it needs an identity.
        let mut visited = 0_usize;
        let mut targets = 0_usize;
        let mut found = None;
        walk(root, None, false, &mut |widget, binding, target| {
            if target {
                targets = targets.saturating_add(1);
                if found.is_none() {
                    let key = CompositionFocus::key(widget);
                    found = Some((
                        visited,
                        CompositionFocus {
                            ambiguous: false,
                            semantic_key: CompositionFocus::semantic_of(key.as_deref()),
                            key,
                            qualified_semantic: false,
                            binding: binding.clone(),
                            state: crate::widget::undo_points::get_state(widget),
                        },
                    ));
                }
            }
            visited = visited.saturating_add(1);
        });
        if targets != 1 {
            return None;
        }
        let (target, mut focus) = found?;
        let key = focus.key.clone()?;
        let mut visited = 0_usize;
        let mut ambiguous = false;
        walk(root, None, false, &mut |widget, binding, _| {
            ambiguous = ambiguous
                || (visited != target
                    && match (&focus.binding, binding) {
                        (Some(a), Some(b)) => a.same_component(&**b),
                        (None, None) => true,
                        _ => false,
                    }
                    && Self::has_key(widget, &key));
            visited = visited.saturating_add(1);
        });
        focus.ambiguous = ambiguous;
        Some(focus)
    }

    fn same_owner(&self, bindings: &BoundNodeMap, node: NodeId) -> bool {
        match (
            &self.binding,
            crate::widget::component_walk::node_binding(bindings, node),
        ) {
            (Some(a), Some(b)) => a.same_component(&*b),
            (None, None) => true,
            _ => false,
        }
    }
    /// # Safety
    /// The caller exclusively owns the live tree described by both maps.
    pub unsafe fn capture(
        node: Option<NodeId>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &BoundNodeMap,
    ) -> Option<Self> {
        let node = node?;
        let mut state_node = node;
        // SAFETY: the caller owns the exact admitted tree.
        let mut focus = unsafe {
            crate::widget::bound_nodes::inspect_bound_node(bindings, nodes, node, |widget| Self {
                ambiguous: false,
                key: Self::key(widget),
                semantic_key: None,
                qualified_semantic: false,
                binding: crate::widget::component_walk::node_binding(bindings, node),
                state: None,
            })
        }
        .ok()?;
        // Runners may observe a focused leaf after an editor focused both
        // itself and that leaf. Preserve the uniquely named aggregate path,
        // so replacing/splitting the leaf cannot retarget a generic text key.
        // Every node of the focused widget's component is asked, so its scope is held across them.
        let mut held = crate::widget::component_walk::HeldScope::default();
        let _ = held.enter(focus.binding.as_ref());
        let aggregates: Vec<_> = nodes
            .keys()
            .copied()
            .filter(|&candidate| {
                candidate != node
                    && focus.same_owner(bindings, candidate)
                    && unsafe {
                        crate::widget::bound_nodes::inspect_bound_node(
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
                crate::widget::bound_nodes::inspect_bound_node(
                    bindings,
                    nodes,
                    *aggregate,
                    |widget| {
                        focus.key = Self::key(widget);
                        state_node = *aggregate;
                    },
                )
            };
        } else if aggregates.len() > 1 {
            focus.ambiguous = true;
        }
        // Anonymous controls retain only their own live state. Without a
        // stable identity, no snapshot may cross this rebuild.
        focus.key.as_ref()?;
        focus.semantic_key = Self::semantic_of(focus.key.as_deref());
        if let Some(key) = &focus.key {
            let matches = nodes
                .keys()
                .filter(|&&node| {
                    // SAFETY: the same exclusive admitted tree is still live.
                    unsafe {
                        crate::widget::bound_nodes::inspect_bound_node(
                            bindings,
                            nodes,
                            node,
                            |widget| focus.same_owner(bindings, node) && Self::has_key(widget, key),
                        )
                    }
                    .unwrap_or(false)
                })
                .take(2)
                .count();
            if matches > 1 && focus.semantic_key.is_none() {
                focus.semantic_key = unsafe {
                    crate::widget::bound_nodes::inspect_bound_node(
                        bindings,
                        nodes,
                        state_node,
                        |widget| {
                            widget
                                .accessibility_root()
                                .and_then(|access| access.semantic_key)
                        },
                    )
                }
                .ok()
                .flatten();
            }
            let matches = if matches > 1 && focus.semantic_key.is_some() {
                focus.qualified_semantic = true;
                nodes
                    .keys()
                    .filter(|&&node| unsafe {
                        crate::widget::bound_nodes::inspect_bound_node(
                            bindings,
                            nodes,
                            node,
                            |widget| {
                                focus.same_owner(bindings, node)
                                    && Self::has_key(widget, key)
                                    && widget
                                        .accessibility_root()
                                        .and_then(|access| access.semantic_key)
                                        == focus.semantic_key
                            },
                        )
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
                crate::widget::bound_nodes::inspect_bound_node(
                    bindings,
                    nodes,
                    state_node,
                    |widget| crate::widget::undo_points::get_state(widget),
                )
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
    pub unsafe fn current(
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &BoundNodeMap,
        allowed: Option<&[NodeId]>,
    ) -> Option<NodeId> {
        let allowed: Option<std::collections::HashSet<NodeId>> =
            allowed.map(|order| order.iter().copied().collect());
        let mut sweep = crate::widget::bound_nodes::NodeSweep::new(bindings, nodes);
        let focused: Vec<_> = nodes
            .keys()
            .copied()
            .filter(|&node| {
                allowed.as_ref().is_none_or(|order| order.contains(&node))
                    && unsafe {
                        sweep.inspect(node, |widget| {
                            widget.is_focusable() && widget.is_enabled() && widget.is_focused()
                        })
                    }
                    .unwrap_or(false)
            })
            .collect();
        drop(sweep);
        let mut candidates = focused.iter().copied().filter(|&candidate| {
            let owner = Self {
                ambiguous: false,
                key: None,
                semantic_key: None,
                qualified_semantic: false,
                state: None,
                binding: crate::widget::component_walk::node_binding(bindings, candidate),
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
    pub unsafe fn restore(
        &self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &BoundNodeMap,
        allowed: Option<&[NodeId]>,
    ) -> Option<NodeId> {
        let target = unsafe { self.resolve(nodes, bindings, allowed) };
        unsafe { self.hand_over(nodes, bindings, target) };
        target
    }

    /// Which control the saved focus names now, if exactly one does.
    /// # Safety
    /// The caller exclusively owns the rebuilt tree described by both maps.
    pub unsafe fn resolve(
        &self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &BoundNodeMap,
        allowed: Option<&[NodeId]>,
    ) -> Option<NodeId> {
        // Every node of the rebuilt tree is asked which control the saved focus names: the scope of
        // the one it belonged to is held across them.
        let mut held = crate::widget::component_walk::HeldScope::default();
        let _ = held.enter(self.binding.as_ref());
        let mut candidates: Vec<_> = nodes
            .keys()
            .copied()
            .filter(|&node| {
                // SAFETY: the caller owns the exact admitted tree.
                unsafe {
                    crate::widget::bound_nodes::inspect_bound_node(
                        bindings,
                        nodes,
                        node,
                        |widget| {
                            !self.ambiguous
                                && self.same_owner(bindings, node)
                                && match &self.key {
                                    Some(key) => Self::has_key(widget, key),
                                    None => false,
                                }
                        },
                    )
                }
                .unwrap_or(false)
            })
            .collect();
        if (self.qualified_semantic || candidates.len() > 1) && self.semantic_key.is_some() {
            candidates.retain(|&node| unsafe {
                crate::widget::bound_nodes::inspect_bound_node(bindings, nodes, node, |widget| {
                    widget
                        .accessibility_root()
                        .and_then(|access| access.semantic_key)
                        == self.semantic_key
                })
                .unwrap_or(false)
            });
        }
        candidates
            .first()
            .copied()
            .filter(|_| candidates.len() == 1)
            .filter(|node| allowed.is_none_or(|order| order.contains(node)))
            .filter(|&node| {
                // Identity must be unique BEFORE focusability/enabled checks;
                // a disabled duplicate must never transfer state to its peer.
                unsafe {
                    crate::widget::bound_nodes::inspect_bound_node(
                        bindings,
                        nodes,
                        node,
                        |widget| widget.is_focusable() && widget.is_enabled(),
                    )
                }
                .unwrap_or(false)
            })
    }

    /// Hand the keyboard to `target` and let every other focused control go.
    /// # Safety
    /// The caller exclusively owns the rebuilt tree described by both maps.
    unsafe fn hand_over(
        &self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &BoundNodeMap,
        target: Option<NodeId>,
    ) {
        // A compound editor and its focused line form one focus path. Blurring
        // either member can clear the other, so only blur unrelated controls.
        // Relationships come from the freshly admitted layout, including
        // child delegates omitted from retained-widget traversal.
        // Every unrelated control is blurred, so every node is asked: the scope is held across them.
        let mut held = crate::widget::component_walk::HeldScope::default();
        let _ = held.enter(self.binding.as_ref());
        let related: std::collections::HashSet<_> = target
            .into_iter()
            .flat_map(|anchor| {
                nodes
                    .keys()
                    .copied()
                    .filter(|&node| {
                        self.same_owner(bindings, node)
                            && (node == anchor
                                || bindings.is_descendant(node, anchor)
                                || bindings.is_descendant(anchor, node))
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
                        crate::widget::component_walk::note_input(bindings, node);
                    }
                })
            };
        }
        // Compound controls propagate focus into their internal editor. Restore
        // the owner after all other nodes have blurred; HashMap iteration order
        // must not clear a child after its owner has just focused it.
        if let Some(node) = target {
            let _ = unsafe {
                crate::widget::component_walk::note_input(bindings, node);
                mutate_bound_node(bindings, nodes, node, |widget| {
                    widget.set_focus(true);
                    if let Some(state) = &self.state {
                        crate::widget::undo_points::set_state(widget, state);
                    }
                })
            };
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub struct AnonymousFocusProbe {
    pub focused: bool,
    pub enabled: bool,
    pub state: String,
    builds: usize,
}
#[cfg(any(test, feature = "test-support"))]
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
#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::unwrap_used,
    reason = "test support: failing the calling test is the point"
)]
impl Widget for AnonymousFocusProbe {
    crate::provides!(dyn crate::widget::undo_points::UndoPoints);

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

#[cfg(any(test, feature = "test-support"))]
impl crate::widget::undo_points::UndoPoints for AnonymousFocusProbe {
    fn get_state(&self) -> Option<String> {
        Some(self.state.clone())
    }
    fn set_state(&mut self, state: &str) {
        self.state = state.into();
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

/// A linear container's `(rect, paint_rect)` at `origin`: the rect spans
/// [`allocation_or_content_size`], the paint rect the laid-out box grown to fit its
/// content.
pub fn flow_rects(
    tree: &TaffyTree<()>,
    node: NodeId,
    origin: vello::kurbo::Point,
) -> (vello::kurbo::Rect, vello::kurbo::Rect) {
    let layout = tree.layout(node).copied().unwrap_or_default();
    let allocated = allocation_or_content_size(tree, node, &layout);
    let location = vello::kurbo::Point::new(
        origin.x + f64::from(layout.location.x),
        origin.y + f64::from(layout.location.y),
    );
    let rect = vello::kurbo::Rect::new(
        location.x,
        location.y,
        location.x + f64::from(allocated.width),
        location.y + f64::from(allocated.height),
    );
    let paint_rect = vello::kurbo::Rect::from_origin_size(
        location,
        (
            f64::from(layout.size.width.max(layout.content_size.width)),
            f64::from(layout.size.height.max(layout.content_size.height)),
        ),
    );
    (rect, paint_rect)
}

/// Lay `widget` out on its own at `rect`, in a layout tree of its own, with
/// the passes the host runs (build, resolve, apply) but at a size the host
/// did not give it: a capture drawing a scrolling document at its full
/// height. The widget's retained layout nodes then name that tree, so the
/// host must rebuild its own before it lays the widget out again.
#[allow(
    clippy::cast_possible_truncation,
    reason = "window-space layout dimensions narrowed for taffy; never near f32::MAX"
)]
pub fn lay_out_detached(
    widget: &mut dyn Widget,
    rect: vello::kurbo::Rect,
    theme: &Theme,
    text: &mut TextRenderer,
) -> Result<(), WidgetLayoutError> {
    let (w, h) = (rect.width() as f32, rect.height() as f32);
    let mut taffy = TaffyTree::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = widget.build_layout(&mut taffy, theme, &mut nodes);
    let root = taffy.new_with_children(
        taffy::Style {
            size: Size::from_lengths(w, h),
            ..Default::default()
        },
        &[child],
    )?;
    let mut bindings = BoundNodeMap::new();
    // SAFETY: `widget`, `taffy`, `child` and `nodes` come from the build
    // above and stay exclusively borrowed while layout runs.
    unsafe {
        bindings.rebuild(widget, &taffy, child, &nodes);
        compute_widget_layout(
            &mut taffy,
            root,
            Size {
                width: AvailableSpace::Definite(w),
                height: AvailableSpace::Definite(h),
            },
            &nodes,
            &bindings,
            text,
        )?;
    }
    widget.apply_layout(&taffy, child, rect.origin());
    // A widget that came to rest only once it knew the room it stands in (a map centring its
    // camera on the viewport it just learned) asks for another pass: a window's frame loop runs
    // it, so a fixture that lays out once does too, and its places stand where the first frame
    // draws them.
    if widget.layout_needs_measure() {
        unsafe {
            bindings.rebuild(widget, &taffy, child, &nodes);
            compute_widget_layout(
                &mut taffy,
                root,
                Size {
                    width: AvailableSpace::Definite(w),
                    height: AvailableSpace::Definite(h),
                },
                &nodes,
                &bindings,
                text,
            )?;
        }
        widget.apply_layout(&taffy, child, rect.origin());
    }
    Ok(())
}

/// Solve `taffy` from `root` as a window does, for `widget` built into it at `node` with `nodes`:
/// what a test that builds its own tree lays out.
#[cfg(any(test, feature = "test-support"))]
#[expect(
    clippy::implicit_hasher,
    reason = "the layout pass keeps its node map with the default hasher"
)]
pub fn solve_built(
    widget: &mut dyn Widget,
    taffy: &mut TaffyTree<()>,
    (root, node): (NodeId, NodeId),
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    available: Size<AvailableSpace>,
    text: &mut TextRenderer,
) -> Result<(), WidgetLayoutError> {
    let mut bindings = BoundNodeMap::new();
    // SAFETY: the caller built `widget` into `taffy` with `nodes` and holds it exclusively while
    // the solve lasts.
    unsafe {
        bindings.rebuild(widget, taffy, node, nodes);
        compute_widget_layout(taffy, root, available, nodes, &bindings, text)
    }
}

/// The style of a widget that takes no place in its parent's flow.
pub fn removed_from_flow() -> taffy::Style {
    taffy::Style {
        display: taffy::Display::None,
        ..taffy::Style::default()
    }
}
