//! The desktop's window bar over a headless app's frame.
//!
//! In a Tilo session the desktop draws one bar per window: the app's icon,
//! the slots the app declares there ([`crate::window_toolbar`]), the
//! window's search and History controls and its own controls. Driven
//! headless, an app runs outside a session, so nothing draws that bar and
//! a picture of it would show the app without the chrome a person sees.
//! [`FrameHarness::present_in_window_bar`] makes the harness stand in for the
//! desktop: the app is told the bar hosts its slots, search and History
//! (as the native runner tells it over its shell connection), its frame is
//! laid out under the bar, and every capture paints the bar above it with
//! the shell's own [`TabBar`] and [`WindowSearch`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::FrameHarness;
use crate::widget::tabs::TAB_BAR_HEIGHT;
use crate::widget::{TabBar, TabDescriptor, Widget, WindowSearch};

/// The one tab the stand-in window shows.
const TAB: u64 = 1;

/// Room the bar keeps for the window's search control, as the window group
/// keeps it.
const SEARCH_CONTROL_ROOM: f64 =
    crate::widget::window_controls::CONTROL_PX + crate::widget::window_controls::CONTROL_GAP;

impl FrameHarness {
    /// Show the app as a desktop window shows it: its slots, search and
    /// History in the window's bar, its frame under the bar. The window
    /// keeps the size it had; the app gets the part under the bar, and a
    /// capture ([`super::write_capture`]) names the whole window.
    pub fn present_in_window_bar(&mut self) {
        self.invalidate_local();
        if self.window_bar {
            return;
        }
        self.window_bar = true;
        crate::window_toolbar::set_hosted(true);
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            history.set_open_button(false);
            history.set_window_presents(true);
        }
        self.document_history.remove_fallback_bar();
        crate::widget::window_header::offer_window_search(self.root.as_mut(), true);
        self.invalidate_layout_tree();
        let frame = self.content_viewport(self.viewport);
        if let Err(error) = self.resize(frame) {
            tracing::warn!(%error, "window bar: the frame under it did not lay out");
        }
    }

    /// Whether the harness draws the desktop's window bar over the app.
    #[must_use]
    pub fn presents_window_bar(&self) -> bool {
        self.window_bar
    }

    /// The app's frame for a window of `viewport`: the part under the bar.
    pub(super) fn content_viewport(&self, viewport: (u32, u32)) -> (u32, u32) {
        if self.window_bar {
            (viewport.0, viewport.1.saturating_sub(bar_height_px()))
        } else {
            viewport
        }
    }

    /// The window's title and app, as its tab shows them.
    fn window_tab(&self) -> TabDescriptor {
        let bound = self
            .root
            .as_any()
            .and_then(|root| root.downcast_ref::<crate::widget::BoundAppWidget>());
        let mut tab = TabDescriptor::new(
            TAB,
            bound.map_or_else(String::new, |bound| {
                crate::AppWidget::app_title(bound).to_owned()
            }),
        );
        tab.app_kind = bound.map(crate::AppWidget::app_kind);
        tab
    }

    /// The bar as the desktop would draw it for this window now, laid out
    /// across `width`, and the window's search control beside its slots.
    fn window_bar(&self, width: f64) -> (TabBar, WindowSearch) {
        let tab = self.window_tab();
        let title = tab.title.clone();
        let mut bar = TabBar::new("headless-window", vec![tab], TAB);
        bar.set_window_controls(true);
        bar.set_search_control(Some(SEARCH_CONTROL_ROOM));
        bar.set_toolbar(TAB, crate::window_toolbar::declared());
        let history = self
            .history
            .as_ref()
            .filter(|history| history.offers_control())
            .map(|history| crate::widget::HistoryControl::of(history.is_open()));
        bar.set_history_control(TAB, history);
        bar.set_group_focused(true);
        lay_out_bar(&mut bar, width, &self.theme);
        let mut search = WindowSearch::new();
        search.set_app_name(&title);
        place_search(&bar, &mut search);
        (bar, search)
    }

    /// Paint the window's bar across the top of a `width`-wide window.
    pub(super) fn paint_window_bar(&mut self, scene: &mut Scene, transform: Affine, width: f64) {
        let (mut bar, mut search) = self.window_bar(width);
        let ground = bar.ground_color(&self.theme);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            ground,
            None,
            &Rect::new(0.0, 0.0, width, bar.bar_height()),
        );
        bar.set_popup_area(Rect::new(0.0, 0.0, width, f64::from(self.viewport.1)));
        bar.draw(
            scene,
            transform,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
        search.set_ground(ground);
        search.draw(
            scene,
            transform,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
    }
}

/// The bar's height in whole logical pixels: one row, the slots in it.
pub(super) fn bar_height_px() -> u32 {
    // A fixed layout constant, far inside `u32`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "TAB_BAR_HEIGHT is a small positive layout constant"
    )]
    let px = TAB_BAR_HEIGHT.ceil() as u32;
    px
}

/// Lay `bar` out across `width` as the window group does: the bar is the
/// first child of a column as wide as the window.
fn lay_out_bar(bar: &mut TabBar, width: f64, theme: &crate::theme::Theme) {
    use crate::num::f32_of_f64;
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Dimension, NodeId, Style, TaffyTree};

    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, theme, &mut node_map);
    let height = bar.bar_height();
    let Ok(root) = taffy.new_with_children(
        Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: Dimension::length(f32_of_f64(width)),
                height: Dimension::length(f32_of_f64(height)),
            },
            ..Default::default()
        },
        &[node],
    ) else {
        return;
    };
    if taffy
        .compute_layout(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(f32_of_f64(width)),
                height: AvailableSpace::Definite(f32_of_f64(height)),
            },
        )
        .is_err()
    {
        return;
    }
    bar.apply_layout(&taffy, node, Point::ZERO);
}

/// Place the window's search control as the window group does: ending the
/// app's slots, else after the tabs.
fn place_search(bar: &TabBar, search: &mut WindowSearch) {
    let px = crate::widget::window_controls::CONTROL_PX;
    let gap = crate::widget::window_controls::CONTROL_GAP;
    if let Some((row, reach)) = bar.toolbar_search_slot() {
        let x0 = row.x1 + gap;
        let y0 = row.center().y - px / 2.0;
        search.place_trailing(Rect::new(x0, y0, x0 + px, y0 + px), reach.min(x0));
        return;
    }
    let row = bar.get_rect();
    let x0 = bar.tabs_end() + gap;
    let y0 = row.center().y - px / 2.0;
    let limit = bar.window_controls().content_limit(row);
    search.place(Rect::new(x0, y0, x0 + px, y0 + px), limit.max(x0 + px));
}

#[cfg(test)]
mod tests {
    use super::*;

    // USER REQUIREMENT (2026-10-21): "The screenshot harness runs apps
    // outside a session, so the window bar never shows. Make the harness
    // render the system window bar for app shots, so this is visible and
    // testable."
    #[test]
    fn a_window_bar_capture_hosts_the_apps_slots_above_its_frame() {
        let _hosted = HostedReset;
        let theme = crate::theme::Theme::dark().with_reduce_motion(true);
        let mut harness = FrameHarness::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Column::new()),
            (800, 600),
            theme,
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        );
        assert!(!crate::window_toolbar::is_hosted());
        harness.present_in_window_bar();
        assert!(
            crate::window_toolbar::is_hosted(),
            "the app hears the bar hosts it"
        );
        assert!(harness.presents_window_bar());
        assert_eq!(
            harness.viewport,
            (800, 600 - bar_height_px()),
            "the window keeps its size; the app gets the part under the bar"
        );
        assert_eq!(harness.content_viewport((800, 644)), (800, 600));
        crate::window_toolbar::declare(Some(crate::window_toolbar::WindowToolbar {
            sidebar: Some(true),
            actions: vec![crate::window_toolbar::ToolbarAction::new(
                "new",
                "plus",
                "New event",
            )],
            ..crate::window_toolbar::WindowToolbar::default()
        }));
        let (bar, _) = harness.window_bar(800.0);
        assert!((bar.bar_height() - TAB_BAR_HEIGHT).abs() < f64::EPSILON);
        let rects = bar.test_rects();
        let sidebar = rects
            .iter()
            .find(|(name, _)| name == "window_sidebar")
            .map_or(Rect::ZERO, |(_, rect)| *rect);
        assert!(sidebar.area() > 0.0, "the declared slots stand in the bar");
        let (_, _, _, actions) = bar.toolbar_rects();
        assert_eq!(actions.len(), 1);
        crate::window_toolbar::declare(None);
    }

    /// Hosting is per thread: a test leaves it as it found it.
    struct HostedReset;

    impl Drop for HostedReset {
        fn drop(&mut self) {
            crate::window_toolbar::set_hosted(false);
        }
    }
}
