//! The docked sidebar's resize edge: the separator assistive technology
//! and the keyboard step, the drag (pointer or finger) that sets the
//! sidebar's width or hides it past its narrowest, how a navigation
//! sidebar reads at that width (rail or list), and where the width is
//! remembered.

use vello::kurbo::Rect;

use super::{Inputs, Metrics, NavMode, NavShell, Resize};
use crate::num::f32_of_f64;
use crate::theme::Theme;
use crate::widget::flap::SplitLayout;
use crate::widget::{SplitHandle, SplitOrientation, SplitRatio};

/// The sidebar edge's own state beside the drag: the finger dragging it,
/// and where the width the person chose is remembered ([`super::memory`]),
/// taken from the window's host at the first layout that has one.
#[derive(Debug, Default)]
pub(super) struct Edge {
    pub(super) touch: Option<u64>,
    /// Which form the sidebar started in, settled the first time the
    /// window showed it: the rule never runs again in this window's life,
    /// whatever arrives in the list later. Only the person's drag moves it.
    started_as_rail: Option<bool>,
    memory: Option<crate::storage::Settings<super::memory::NavShellMemory>>,
}

impl NavShell {
    pub(super) fn sidebar_resize_target(&self) -> String {
        format!(
            "{}.sidebar-resize",
            self.id.as_deref().unwrap_or("nav-shell")
        )
    }

    /// The docked sidebar's resize edge as the shared split separator, its
    /// value the sidebar's share of the window, while the edge is draggable.
    pub(super) fn sidebar_resize_handle(&self) -> Option<SplitHandle> {
        let edge = self.geometry.resize_handle;
        let extent = self.rect.width();
        if self.resize.is_some() || self.animating || edge.width() <= 0.0 || extent <= 0.0 {
            return None;
        }
        let inputs = self.inputs.borrow();
        let overview = if inputs.has_overview {
            inputs.metrics.overview
        } else {
            0.0
        };
        let widest = inputs
            .metrics
            .sidebar_max
            .min((extent - overview - inputs.content_min()).max(0.0));
        let mut ratio = SplitRatio::default().with_bounds(0.0, f32_of_f64(widest / extent));
        ratio.set(crate::num::f32_of_f64(
            self.geometry.sidebar_column.width() / extent,
        ));
        let mut handle = SplitHandle::new(SplitOrientation::Horizontal, ratio);
        handle.set_bounds(Rect::new(self.rect.x0, edge.y0, self.rect.x1, edge.y1));
        handle.set_focused(self.resize_focused);
        Some(handle)
    }

    /// Resize the sidebar by a separator step or value, as a drag would.
    pub(super) fn step_sidebar_resize(&mut self, action: &crate::a11y::Action) -> bool {
        let Some(mut handle) = self.sidebar_resize_handle() else {
            return false;
        };
        if !handle.perform_action(action) {
            return false;
        }
        let width = handle.ratio().first_extent(self.rect.width());
        self.drag_sidebar_to(
            Resize {
                start_x: 0.0,
                start_width: 0.0,
            },
            width,
        );
        self.remember_sidebar_width();
        true
    }

    pub(super) fn drag_sidebar_to(&mut self, resize: Resize, x: f64) {
        // The edge follows the pointer one to one: the width is where the
        // pointer is, from the rail (or the minimum) to the maximum, and
        // how the destinations read follows that width at its threshold.
        let width = resize.start_width + (x - resize.start_x);
        let was_open = self.is_sidebar_open();
        let (min, scale) = {
            let inputs = self.inputs.borrow();
            (inputs.sidebar_narrowest(), inputs.metrics.scale)
        };
        if width < min * 0.75 {
            // Dragged past its narrowest: the sidebar hides.
            if was_open {
                let mut inputs = self.inputs.borrow_mut();
                inputs.sidebar.snap(false);
            }
        } else {
            let mut inputs = self.inputs.borrow_mut();
            if !inputs.sidebar.is_open(SplitLayout::Docked) {
                inputs.sidebar.snap(true);
            }
            inputs.user_sidebar = Some(width.max(min) / scale.max(f64::EPSILON));
        }
        self.update_toggle_names();
        self.publish_open_states();
        // A new width is a new layout of the same parts, as a window resize
        // is; only hiding or showing the sidebar changes which parts stand.
        if was_open != self.is_sidebar_open() {
            self.structure_dirty = true;
        }
        self.invalidate();
    }

    /// The drag of the edge ended (the button or finger let go, a keyboard
    /// step landed): remember the width it left.
    pub(super) fn end_sidebar_resize(&mut self) {
        self.resize = None;
        self.edge.touch = None;
        self.remember_sidebar_width();
        self.structure_dirty = true;
        self.invalidate();
    }

    /// The band a finger takes the edge in: the edge widened to a pointer
    /// target, half on each side of it.
    pub(super) fn sidebar_resize_touch_band(&self) -> Rect {
        let edge = self.geometry.resize_handle;
        if edge.width() <= 0.0 {
            return Rect::ZERO;
        }
        let target = f64::from(self.layout_theme.tokens.metrics.min_pointer_target);
        let half = (target / 2.0).max(edge.width() / 2.0);
        let x = edge.center().x;
        Rect::new(x - half, edge.y0, x + half, edge.y1)
    }

    /// A finger on the edge drags it as the pointer held down does: it
    /// takes the edge where it lands, the width follows it, and lifting
    /// it ends the resize. Returns whether the shell took `touch`.
    pub(super) fn handle_resize_touch(
        &mut self,
        touch: &winit::event::Touch,
        ctx: &crate::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::TouchPhase;
        let p = ctx.logical(touch.location);
        match touch.phase {
            TouchPhase::Started => {
                if self.resize.is_some() || !self.sidebar_resize_touch_band().contains(p) {
                    return false;
                }
                self.resize_focused = false;
                self.resize = Some(Resize {
                    start_x: p.x,
                    start_width: self.geometry.sidebar_column.width(),
                });
                self.edge.touch = Some(touch.id);
                self.last_cursor = p;
                true
            }
            TouchPhase::Moved => {
                if self.edge.touch != Some(touch.id) {
                    return false;
                }
                self.last_cursor = p;
                if let Some(resize) = self.resize {
                    self.drag_sidebar_to(resize, p.x);
                }
                true
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if self.edge.touch != Some(touch.id) {
                    return false;
                }
                self.end_sidebar_resize();
                true
            }
        }
    }

    /// The shell metrics under `theme` and how the sidebar adapts to its
    /// width, for a build of the tree: the remembered width taken up, a
    /// navigation-list sidebar measured as rail and list, and a rail grown
    /// with the text to fit its longest word, up to its cap (the overview's
    /// rail, and a sidebar that is always a rail).
    pub(super) fn sidebar_metrics(&mut self, theme: &Theme) -> (Metrics, Option<AdaptiveSidebar>) {
        self.recall_sidebar_width();
        let mut metrics = Metrics::resolve(theme);
        let (docked_rail, header_actions) = {
            let inputs = self.inputs.borrow();
            let actions = inputs.actions.get(super::Column::Sidebar.index());
            (
                inputs.mode == NavMode::Docked,
                actions.is_some_and(|n| *n > 0),
            )
        };
        let adaptive = if docked_rail {
            None
        } else {
            self.sidebar
                .as_deref()
                .and_then(crate::widget::nav_list::adaptive)
                .map(|nav| {
                    let mut adaptive = adaptive_sidebar(nav, theme, &metrics);
                    // The sidebar's own header actions ("Add subscription")
                    // need the list's width to stand in its header.
                    adaptive.rail_default &= !header_actions;
                    if let Some(settled) = self.edge.started_as_rail {
                        adaptive.rail_default = settled;
                    }
                    adaptive
                })
        };
        let fitted = [
            self.rail.as_deref(),
            self.sidebar.as_deref().filter(|_| docked_rail),
        ]
        .into_iter()
        .flatten()
        .filter_map(|part| crate::widget::nav_list::fitted_rail_width(part, theme))
        .reduce(f64::max);
        if let Some(rail) = fitted {
            metrics.rail = rail;
        }
        (metrics, adaptive)
    }

    /// How the sidebar's destinations read follows the width its body now
    /// has (`width`); a hidden sidebar keeps the form it had.
    pub(super) fn follow_sidebar_width(&mut self, width: f64) {
        if width > 0.0 {
            let (rail, adaptive) = {
                let inputs = self.inputs.borrow();
                (inputs.sidebar_reads_as_rail(width), inputs.adaptive)
            };
            if let Some(adaptive) = adaptive {
                self.edge
                    .started_as_rail
                    .get_or_insert(adaptive.rail_default);
            }
            self.set_sidebar_rail(rail);
        }
    }

    /// Paint the sidebar's destinations as a rail's tiles or as list rows,
    /// where the sidebar is a navigation list.
    pub(super) fn set_sidebar_rail(&mut self, rail: bool) {
        let Some(sidebar) = self.sidebar.as_deref_mut() else {
            return;
        };
        let Some(nav) = crate::widget::nav_list::adaptive_mut(sidebar) else {
            return;
        };
        if nav.set_rail(rail) {
            self.paint_dirty = true;
        }
    }

    /// Stage the sidebar at `width` (authored at 1×) as a person who
    /// dragged it there left it, for a capture of a widened or narrowed
    /// sidebar. Live windows get the width from the person's drag and from
    /// where it is remembered, never from the app.
    pub fn stage_sidebar_width(&mut self, width: f64) {
        if width.is_finite() && width > 0.0 {
            self.inputs.borrow_mut().user_sidebar = Some(width);
            self.structure_dirty = true;
            self.invalidate();
        }
    }

    /// What the person's sidebar width is remembered under: the shell's id.
    fn memory_key(&self) -> String {
        self.id.clone().unwrap_or_else(|| "nav-shell".to_owned())
    }

    /// At the first layout, take up the sidebar width the person left this
    /// app's window at, where the window's host remembers it.
    pub(super) fn recall_sidebar_width(&mut self) {
        if self.edge.memory.is_some() {
            return;
        }
        // Until the host says where, look again at each build: a window
        // may be built before its host knows which app it is.
        let Some(settings) = super::memory::current() else {
            return;
        };
        if let Some(width) = super::memory::recall(&settings, &self.memory_key()) {
            let mut inputs = self.inputs.borrow_mut();
            if inputs.user_sidebar.is_none() {
                inputs.user_sidebar = Some(width);
            }
        }
        self.edge.memory = Some(settings);
    }

    fn remember_sidebar_width(&self) {
        let width = self.inputs.borrow().user_sidebar;
        if let (Some(settings), Some(width)) = (&self.edge.memory, width) {
            super::memory::keep(settings, &self.memory_key(), width);
        }
    }
}

/// A navigation sidebar whose width decides how its destinations read
/// (`nav_list::AdaptiveNav`): narrower than [`Self::list_from`], a rail of
/// tiles, icon over caption; from there on, a list, icon beside label. The
/// width itself follows the person's drag continuously between the rail's
/// width and the sidebar's maximum, so the edge never jumps away from the
/// pointer; only how the rows read changes at the threshold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct AdaptiveSidebar {
    /// Its width as a rail: the narrowest it gets before it hides.
    pub(super) rail: f64,
    /// The width from which it is a list: where every label stands whole
    /// beside its icon, never past the sidebar's own minimum.
    pub(super) list_from: f64,
    /// It starts as a rail (a short, flat set of places); else as a list
    /// at the sidebar's width.
    pub(super) rail_default: bool,
}

impl Inputs {
    /// The sidebar's width before the person drags it: a rail-shaped
    /// navigation list starts as its rail, every other sidebar at its width.
    pub(super) fn sidebar_default(&self) -> f64 {
        match self.adaptive {
            Some(adaptive) if adaptive.rail_default => adaptive.rail,
            _ => self.metrics.sidebar,
        }
    }

    /// The narrowest a person can drag the docked sidebar before it hides:
    /// its rail where it has one, else its minimum.
    pub(super) fn sidebar_narrowest(&self) -> f64 {
        self.adaptive
            .map_or(self.metrics.sidebar_min, |adaptive| adaptive.rail)
    }

    /// Whether a sidebar `width` wide reads as a rail: always in
    /// [`NavMode::Docked`], below its list threshold for a navigation list,
    /// never for any other sidebar.
    pub(super) fn sidebar_reads_as_rail(&self, width: f64) -> bool {
        if self.mode == NavMode::Docked {
            return true;
        }
        self.adaptive
            .is_some_and(|adaptive| width < adaptive.list_from - 1e-6)
    }
}

/// How the navigation sidebar `nav` adapts to its width under `theme`.
pub(super) fn adaptive_sidebar(
    nav: &dyn crate::widget::nav_list::AdaptiveNav,
    theme: &Theme,
    m: &Metrics,
) -> AdaptiveSidebar {
    let rail = nav.rail_fit(theme);
    // A list from where every label stands whole beside its icon, and at
    // the sidebar's own minimum at the latest; always past the rail itself,
    // so the narrowest sidebar is a rail.
    let list_from = nav.list_fit(theme).min(m.sidebar_min).max(rail + 1.0);
    AdaptiveSidebar {
        rail,
        list_from,
        rail_default: nav.rail_by_default(theme),
    }
}
