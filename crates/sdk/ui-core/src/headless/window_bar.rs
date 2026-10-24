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

use taffy::NodeId;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::FrameHarness;
#[cfg(test)]
use crate::widget::tabs::TAB_BAR_HEIGHT;
use crate::widget::{TabBar, TabDescriptor, Widget, WindowSearch, WindowSearchRequest};

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
    /// capture (`super::write_capture`) names the whole window.
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

    /// The History control the window lists beside the app's slots, as the
    /// desktop's bar (or, outside a session, the runner's slot row) shows
    /// it: none while the app has no History to open.
    #[must_use]
    pub fn window_history_control(&self) -> Option<crate::widget::HistoryControl> {
        self.history
            .as_ref()
            .filter(|history| history.offers_control())
            .map(|history| crate::widget::HistoryControl::of(history.is_open()))
    }

    /// Press the window's History control as the bar hands the press over:
    /// it runs on the app's History door, as the History shortcut does, and
    /// never reaches the app.
    pub fn press_window_history(&mut self) -> Result<(), String> {
        self.invalidate_local();
        let control = self
            .window_history_control()
            .ok_or("History is unavailable")?;
        let history = self.history.as_mut().ok_or("History is unavailable")?;
        let intent = crate::standalone_history_host::history_control_binding(control)
            .invoke(())
            .map_err(|error| error.to_string())?;
        crate::history_mode::embedded::run_door_control(history, &intent)
            .map_err(|error| error.to_string())?;
        self.relayout();
        Ok(())
    }

    /// The control of `order` that takes the keyboard when its app does
    /// without one of its own choosing ([`crate::widget::keyboard_entry_rank`]).
    pub(super) fn keyboard_entry(&self, order: &[NodeId]) -> Option<NodeId> {
        order
            .iter()
            .copied()
            .filter_map(|node| {
                let rank = self
                    .inspect_node(node, crate::widget::keyboard_entry_rank)
                    .ok()
                    .flatten()?;
                Some((rank, node))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, node)| node)
    }

    /// Type `query` into the window bar's search, opened with Find or
    /// Replace as the desktop opens it, and carry it to the app's widgets,
    /// which find it in place. Returns the current match and the count, as
    /// the bar shows them.
    pub fn search_in_window(
        &mut self,
        opening: crate::window_toolbar::SearchOpening,
        query: &str,
    ) -> Option<(usize, usize)> {
        if !self.window_search.is_open() {
            let offer = crate::window_toolbar::SearchOffer::of(self.root.as_mut());
            self.window_search.open_as(
                offer.into(),
                crate::tokens::MotionSource::Keyboard,
                opening,
            );
        }
        self.window_search_action(
            "window-search-query",
            &crate::a11y::Action::SetValue(query.into()),
        );
        self.drive_window_search()
    }

    /// Step the window bar's search to the next (or previous) match.
    pub fn step_window_search(&mut self, forward: bool) -> Option<(usize, usize)> {
        self.window_search.step(forward);
        self.drive_window_search()
    }

    /// Type `with` into the open search's replacement and press Replace
    /// (or Replace all); the frame after writes what the app made of it.
    /// Returns whether the search offered Replace on what the window shows.
    pub fn replace_in_window(&mut self, with: &str, all: bool) -> bool {
        self.drive_window_search();
        if !self.window_search.abilities().replace {
            return false;
        }
        self.window_search_action(
            "window-search-replacement",
            &crate::a11y::Action::SetValue(with.into()),
        );
        self.window_search.request_replace(all);
        self.drive_window_search();
        true
    }

    /// The window bar's search's current match and count, as the bar
    /// shows them; `None` with nothing found.
    #[must_use]
    pub fn window_search_position(&self) -> Option<(usize, usize)> {
        self.window_search.position()
    }

    /// The query the window bar's search field shows.
    #[must_use]
    pub fn window_search_query(&self) -> &str {
        self.window_search.query()
    }

    /// Whether the window bar's search offers Replace on what the window
    /// shows.
    #[must_use]
    pub fn window_search_replaces(&self) -> bool {
        self.window_search.abilities().replace
    }

    /// Perform `action` on a part of the window bar's search (a
    /// `window-search-*` target: open it, type the query or the
    /// replacement, set an option, step, replace, close), as an agent
    /// performs it on the desktop's window, then carry the search to the
    /// app's widgets. The window carries out what the search asks of it:
    /// it opens in place where the app offers that.
    pub(super) fn perform_window_search_semantic(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        if !self.window_bar {
            return Err(format!(
                "{target} is part of the window bar's search: present the window bar"
            ));
        }
        // The search's parts stand where the bar lays them out; the bar
        // spans the window, as wide as the app under it.
        let width = f64::from(self.viewport.0);
        self.paint_window_bar(&mut Scene::new(), Affine::IDENTITY, width);
        let handled = self.window_search_action(target, action);
        for request in self.window_search.take_requests() {
            match request {
                WindowSearchRequest::Open => {
                    let offer = crate::window_toolbar::SearchOffer::of(self.root.as_mut());
                    self.window_search.open_as(
                        offer.into(),
                        crate::tokens::MotionSource::Action,
                        crate::window_toolbar::SearchOpening::Find,
                    );
                }
                WindowSearchRequest::Close => {
                    self.window_search
                        .close(crate::tokens::MotionSource::Action);
                }
                WindowSearchRequest::Widen(_) => {
                    return Err(format!(
                        "{target} widens the search to the desktop's, which a single app's window does not host"
                    ));
                }
            }
        }
        if !handled {
            return Err(format!(
                "the window bar's search did not take {action:?} on {target} now"
            ));
        }
        self.drive_window_search();
        Ok(())
    }

    fn window_search_action(&mut self, target: &str, action: &crate::a11y::Action) -> bool {
        let mut scratch = automerge::AutoCommit::new();
        self.window_search.perform_semantic_action(
            target,
            action,
            &mut crate::widget::Edits::lend(&mut scratch),
            &automerge::ROOT,
        )
    }

    /// Carry the search to the app's widgets, then run the frame that shows
    /// (and writes) what they made of it.
    fn drive_window_search(&mut self) -> Option<(usize, usize)> {
        self.invalidate_local();
        self.window_search.drive(self.root.as_mut());
        self.relayout();
        self.window_search.position()
    }

    /// One frame ([`FrameHarness::try_relayout`]), then the open search
    /// carried to the app's widgets, as the live window carries it once a
    /// frame, so what the app shows after the query was typed (a program's
    /// output, a page that loaded) is found too; the frame lays out again
    /// when that changed what it shows.
    pub(super) fn try_relayout_following_search(
        &mut self,
    ) -> Result<(), crate::widget::WidgetLayoutError> {
        self.try_relayout()?;
        if self.window_bar
            && self.window_search.wants_drive()
            && self.window_search.drive(self.root.as_mut())
        {
            self.invalidate_local();
            return self.try_relayout();
        }
        Ok(())
    }

    /// Press one of the app's own slots in the window's bar (a mode, an
    /// action, the sidebar toggle), as the desktop hands the press to the
    /// app. The press is a press on the app: it hands the app the keyboard
    /// (`Self::hand_keyboard_to_app`), as in the native window. What the
    /// app asked of the host when its sync took the press (the Undo beside a
    /// notice asks for an undo) is done and shown, as the live window's idle
    /// service does after that sync.
    pub fn press_window_slot(&mut self, event: crate::window_toolbar::ToolbarEvent) {
        self.invalidate_local();
        let hands_keyboard = crate::window_toolbar::hands_keyboard_to_app(&event);
        crate::window_toolbar::deliver(event);
        if hands_keyboard {
            self.hand_keyboard_to_app();
        }
        self.relayout();
        self.host_tick();
        self.invalidate_local();
        self.relayout();
    }

    /// The window took the keyboard (it opened, was brought forward —
    /// `WindowEvent::Focused(true)` — or the person pressed one of the app's
    /// slots in its bar): when nothing in it holds the keyboard, the app's
    /// keyboard entry ([`crate::widget::keyboard_entry_rank`]) takes it, as
    /// in the native window.
    pub(super) fn hand_keyboard_to_app(&mut self) {
        if self.focused.is_some() {
            return;
        }
        let instance = self.component_instance_id();
        let order: Vec<_> = self
            .focus_order()
            .into_iter()
            .filter(|&node| {
                self.app_focus_eligible(node)
                    && instance.is_none_or(|instance| {
                        self.bindings
                            .binding_for(node)
                            .is_some_and(|binding| binding.instance_id == instance)
                    })
            })
            .collect();
        if let Some(target) = self.keyboard_entry(&order) {
            self.set_focused(Some(target));
        }
    }

    /// The app's frame for a window of `viewport`: the part under the bar.
    pub(super) fn content_viewport(&self, viewport: (u32, u32)) -> (u32, u32) {
        if self.window_bar {
            (
                viewport.0,
                viewport
                    .1
                    .saturating_sub(self.window_bar_height_px(viewport.0)),
            )
        } else {
            viewport
        }
    }

    /// Where a point of the window (a capture's picture, as the person sees
    /// it) is in the app's frame under the bar. The bar is the desktop's:
    /// a point on it is refused, never handed to the app.
    pub(super) fn frame_point(&self, x: f64, y: f64) -> Result<(f64, f64), String> {
        let frame_y = self.frame_y(y);
        if frame_y < 0.0 {
            return Err(format!(
                "({x}, {y}) is on the window bar, which is the desktop's: invoke the \
                 action its slot runs instead"
            ));
        }
        Ok((x, frame_y))
    }

    /// [`Self::frame_point`]'s height alone, for the end of a gesture that
    /// may leave the frame.
    pub(super) fn frame_y(&self, y: f64) -> f64 {
        y - self.frame_top()
    }

    /// A rect of the app's frame where the window shows it.
    pub(super) fn window_rect(&self, rect: Rect) -> Rect {
        rect + vello::kurbo::Vec2::new(0.0, self.frame_top())
    }

    /// Where the app's frame starts in the window.
    fn frame_top(&self) -> f64 {
        if self.window_bar {
            f64::from(self.window_bar_height_px(self.viewport.0))
        } else {
            0.0
        }
    }

    /// The window's title and app, as its tab shows them.
    fn window_tab(&self) -> TabDescriptor {
        let bound = self
            .root
            .as_any()
            .and_then(|root| root.downcast_ref::<crate::widget::BoundAppWidget>());
        let (kind, title) = match bound {
            Some(bound) => (
                Some(crate::AppWidget::app_kind(bound)),
                crate::AppWidget::app_title(bound).to_owned(),
            ),
            None => self
                .window_app
                .clone()
                .map_or((None, String::new()), |(kind, title)| (Some(kind), title)),
        };
        let mut tab = TabDescriptor::new(TAB, title);
        tab.app_kind = kind;
        tab
    }

    /// The bar as the desktop would draw it for this window now, laid out
    /// across `width`.
    fn window_bar(&self, width: f64) -> TabBar {
        let tab = self.window_tab();
        let mut bar = TabBar::new("headless-window", vec![tab], TAB);
        bar.set_window_controls(true);
        bar.set_search_control(Some(SEARCH_CONTROL_ROOM));
        bar.set_toolbar(TAB, crate::window_toolbar::shown());
        bar.set_history_control(TAB, self.window_history_control());
        bar.set_group_focused(true);
        // The first layout gives the bar its width; the second measures a
        // compact window's notice strip for it.
        lay_out_bar(&mut bar, width, &self.theme);
        lay_out_bar(&mut bar, width, &self.theme);
        bar
    }

    /// The bar's height in whole logical pixels across a window `width`
    /// wide: its row, and a compact window's notice strip under it.
    pub(super) fn window_bar_height_px(&self, width: u32) -> u32 {
        let height = self.window_bar(f64::from(width)).bar_height().ceil();
        // A bar is a row or two and a strip tall, far inside `u32`.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the bar's height is a small positive layout length"
        )]
        let px = height.max(0.0) as u32;
        px
    }

    /// How the window's bar, as the desktop draws it across a window
    /// `width` wide, shows the notice up (none: no notice is up).
    #[must_use]
    pub fn window_bar_notice(&mut self, width: f64) -> Option<WindowBarNotice> {
        let _settled = crate::motion::testing::settled();
        let mut bar = self.window_bar(width);
        // Painting measures the slots' labels; the layout after it sizes
        // them.
        bar.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
        lay_out_bar(&mut bar, width, &self.theme);
        let bar_height = bar.bar_height();
        let row = bar.toolbar_row();
        let toolbar = row.toolbar()?;
        let slots = row.slots();
        let plain_leading = slots
            .leading
            .iter()
            .zip(&toolbar.leading)
            .filter(|(rect, action)| !action.navigates && rect.area() > 0.0)
            .count();
        let beside = slots.actions.len() + plain_leading;
        // A compact window's notice, in its strip under the row.
        if let Some((notice, text, whole, button)) = row.strip_notice() {
            return Some(WindowBarNotice {
                text: notice.text.clone(),
                text_bounds: text,
                whole,
                button: notice
                    .action
                    .as_ref()
                    .filter(|_| button.area() > 0.0)
                    .map(|action| WindowBarNoticeButton {
                        name: action.name.clone(),
                        bounds: button,
                        labelled: true,
                    }),
                beside,
                strip: Some(row.strip_rect()),
                bar_height,
            });
        }
        let notice = toolbar.notice.as_ref()?;
        Some(WindowBarNotice {
            text: notice.text.clone(),
            whole: row.notice_whole().unwrap_or(false),
            button: notice
                .action
                .as_ref()
                .filter(|_| slots.notice.area() > 0.0)
                .map(|action| WindowBarNoticeButton {
                    name: action.name.clone(),
                    bounds: slots.notice,
                    labelled: slots.notice_labelled,
                }),
            text_bounds: slots.location,
            beside,
            strip: None,
            bar_height,
        })
    }

    /// Paint the window's bar across the top of a `width`-wide window.
    pub(super) fn paint_window_bar(&mut self, scene: &mut Scene, transform: Affine, width: f64) {
        // A capture shows the bar at rest, as a person sees it after it
        // arrived: its slots' cross-fade has ended.
        let _settled = crate::motion::testing::settled();
        assert!(
            self.window_tab().app_kind.is_some(),
            "a hosted window's bar wears its app's icon, as every desktop window's does: \
             mount the app with FrameHarness::for_app"
        );
        let mut bar = self.window_bar(width);
        let title = self.window_tab().title;
        let search = &mut self.window_search;
        search.set_app_name(&title);
        place_search(&bar, search);
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
        let search = &mut self.window_search;
        search.set_ground(ground);
        search.draw(
            scene,
            transform,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
    }

    /// Paint the bar's top layer over the app's frame: the open search's
    /// options and replacement, under the field.
    pub(super) fn paint_window_bar_top_layer(&mut self, scene: &mut Scene, transform: Affine) {
        self.window_search.draw_overlay(
            scene,
            transform,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
    }
}

/// How the window's bar shows the notice up
/// ([`FrameHarness::window_bar_notice`]).
#[derive(Clone, Debug, PartialEq)]
pub struct WindowBarNotice {
    /// What it says.
    pub text: String,
    /// Where its text stands, its kind's icon first.
    pub text_bounds: Rect,
    /// Its text stands whole, not cut short.
    pub whole: bool,
    /// Its button, where it stands in the row (none: it has none, or the
    /// row has no room for it).
    pub button: Option<WindowBarNoticeButton>,
    /// How many of the app's actions and the leading actions that do not
    /// move through the content stand in the row beside it, not in the
    /// overflow menu: what yields to it.
    pub beside: usize,
    /// Its strip under the bar's row, where the window is compact (none:
    /// it stands inline, in the location's place).
    pub strip: Option<Rect>,
    /// The whole bar's height, the strip included: where the app's frame
    /// starts.
    pub bar_height: f64,
}

/// The notice's button as the window's bar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowBarNoticeButton {
    pub name: String,
    pub bounds: Rect,
    /// It names itself, not only its icon.
    pub labelled: bool,
}

/// The bar's height in whole logical pixels without a notice strip: one
/// row, the slots in it.
#[cfg(test)]
fn bar_height_px() -> u32 {
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
        let bar = harness.window_bar(800.0);
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

    fn harness_of(root: Box<dyn Widget>) -> FrameHarness {
        FrameHarness::new_with_controlled_inputs(
            crate::State::new(),
            root,
            (800, 600),
            crate::theme::Theme::dark().with_reduce_motion(true),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        )
    }

    fn focused_name(harness: &FrameHarness) -> Option<String> {
        ["first", "chosen"]
            .into_iter()
            .find(|name| {
                harness
                    .access_node_named(name, None)
                    .is_ok_and(|node| node.state.focused)
            })
            .map(str::to_owned)
    }

    /// A press on one of the app's own slots in the window's bar is a press
    /// on the app: with nothing in the window holding the keyboard, the
    /// control the app declares its default takes it, never the first
    /// button merely for being first, so what is typed next reaches the
    /// app. Typing in the bar's search keeps the keyboard where it is, and
    /// a control that holds it keeps it.
    #[test]
    fn a_slot_press_hands_the_keyboard_to_the_apps_default_control() {
        let root = crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("first", "First")))
            .push(Box::new(
                crate::widget::Button::new("chosen", "Chosen").with_default_focus(),
            ));
        let mut harness = harness_of(Box::new(root));
        harness.relayout();
        assert_eq!(harness.focused(), None, "nothing holds the keyboard");

        harness
            .press_window_slot(crate::window_toolbar::ToolbarEvent::Search { query: "a".into() });
        assert_eq!(harness.focused(), None, "the bar's search keeps it");

        harness.press_window_slot(crate::window_toolbar::ToolbarEvent::Action {
            id: "mode:1".into(),
        });
        assert_eq!(focused_name(&harness).as_deref(), Some("chosen"));

        harness.click_named("first").unwrap();
        assert_eq!(focused_name(&harness).as_deref(), Some("first"));
        harness.press_window_slot(crate::window_toolbar::ToolbarEvent::ToggleSidebar);
        assert_eq!(
            focused_name(&harness).as_deref(),
            Some("first"),
            "a control that holds the keyboard keeps it"
        );
        let _ = crate::window_toolbar::take_events();
    }

    /// A window that takes the keyboard (it opened, or was brought
    /// forward) hands it to the app's default control when nothing holds
    /// it; a window of buttons only keeps none focused, since Enter would
    /// press the first.
    #[test]
    fn a_window_taking_the_keyboard_hands_it_to_the_apps_default_control() {
        let root = crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("first", "First")))
            .push(Box::new(
                crate::widget::Button::new("chosen", "Chosen").with_default_focus(),
            ));
        let mut harness = harness_of(Box::new(root));
        harness.relayout();
        harness.send(&winit::event::WindowEvent::Focused(true));
        assert_eq!(focused_name(&harness).as_deref(), Some("chosen"));

        let buttons = crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("first", "First")));
        let mut harness = harness_of(Box::new(buttons));
        harness.relayout();
        harness.send(&winit::event::WindowEvent::Focused(true));
        assert_eq!(harness.focused(), None, "never a button for being first");
    }

    /// A spec aims at the picture it captures: with the bar over the app, a
    /// click on a control where the capture shows it presses that control.
    #[test]
    fn a_click_aimed_at_the_captured_window_lands_where_it_is_painted() {
        let _hosted = HostedReset;
        let root = crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("first", "First")))
            .push(Box::new(crate::widget::Button::new("chosen", "Chosen")));
        let mut harness = harness_of(Box::new(root));
        harness.present_in_window_bar();
        harness.relayout();
        let first = harness
            .targets()
            .into_iter()
            .find_map(|(name, rect)| (name == "first").then(|| harness.window_rect(rect)))
            .expect("the first button is a target");
        let bar = f64::from(bar_height_px());
        assert!(first.y0 >= bar, "the app's frame starts under the bar");
        let centre = first.center();
        harness
            .perform(&super::super::Action::ClickAt(centre.x, centre.y))
            .unwrap();
        assert_eq!(focused_name(&harness).as_deref(), Some("first"));
        assert!(
            harness
                .perform(&super::super::Action::ClickAt(centre.x, bar / 2.0))
                .is_err(),
            "the bar is the desktop's"
        );
    }

    /// An app that is only its name.
    struct NamedApp;

    impl Widget for NamedApp {
        fn name(&self) -> &str {
            "NamedApp"
        }
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &crate::theme::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            use crate::widget::TaffyTreeExt as _;
            tree.widget_leaf(taffy::Style::default(), nodes, self)
        }
    }

    impl crate::widget::AppWidget for NamedApp {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Files
        }
        fn app_title(&self) -> &str {
            "Files"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("F")
        }
    }

    // Every hosted bar wears its app's icon at its leading edge, whatever
    // the location says (a cloud folder, a page, nothing).
    #[test]
    fn every_hosted_bar_wears_its_apps_icon() {
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            NamedApp,
            (800, 600),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        crate::window_toolbar::declare(Some(crate::window_toolbar::WindowToolbar {
            location: Some(crate::window_toolbar::ToolbarLocation {
                label: "Trip to Lisbon".into(),
                ..crate::window_toolbar::ToolbarLocation::default()
            }),
            ..crate::window_toolbar::WindowToolbar::default()
        }));
        let tab = harness.window_tab();
        assert_eq!(tab.app_kind, Some(crate::widget::AppKind::Files));
        assert_eq!(tab.title, "Files");
        let mut scene = Scene::new();
        harness.paint_window_bar(&mut scene, Affine::IDENTITY, 800.0);
        crate::window_toolbar::declare(None);
    }

    /// A compact (420 px) window with a long title and no location: the
    /// title takes the room the row leaves it, with no slots and beside one
    /// trailing action alike, and is cut short only where none is left.
    #[test]
    fn a_compact_windows_long_title_takes_the_room_the_row_leaves_it() {
        use crate::window_toolbar::{ToolbarAction, WindowToolbar};
        const TITLE: &str = "Video Player Library";
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            NamedApp,
            (420, 800),
            crate::theme::Theme::dark(),
        );
        // The location names it in the body style.
        let style = harness.theme.text(crate::tokens::TextRole::Body);
        let title_w = f64::from(harness.text.measure_style(TITLE, None, &style).0);
        let one_action = WindowToolbar {
            actions: vec![ToolbarAction::new("open", "folder-open", "Open")],
            ..WindowToolbar::default()
        };
        for toolbar in [None, Some(one_action)] {
            let mut tab = TabDescriptor::new(TAB, TITLE);
            tab.app_kind = Some(crate::widget::AppKind::Files);
            let mut bar = TabBar::new("headless-window", vec![tab], TAB);
            bar.set_window_controls(true);
            bar.set_search_control(Some(SEARCH_CONTROL_ROOM));
            let slots = toolbar.is_some();
            bar.set_toolbar(TAB, toolbar);
            // Painting measures the title; the layout after it sizes it.
            for _ in 0..2 {
                lay_out_bar(&mut bar, 420.0, &harness.theme);
                bar.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut harness.text,
                    &harness.theme,
                    &harness.presence,
                );
            }
            if slots {
                let row = bar.toolbar_row();
                let location = row.slots().location;
                assert_eq!(
                    row.toolbar()
                        .and_then(|toolbar| toolbar.location.as_ref())
                        .map(|location| location.label.as_str()),
                    Some(TITLE),
                    "the title stands in the location's place"
                );
                assert!(
                    location.width() >= title_w,
                    "beside one action the title stands whole: {location:?}, {title_w} px"
                );
                assert_eq!(row.slots().actions.len(), 1, "and the action beside it");
            } else {
                assert_eq!(
                    bar.visible_title(0, &mut harness.text, &harness.theme)
                        .as_deref(),
                    Some(TITLE),
                    "with no slots the title stands whole"
                );
            }
        }
    }

    /// A compact window's notice has a strip of its own under the bar's
    /// row: its whole text and its action as a button that names itself,
    /// the row standing as it does without a notice, and the app's frame
    /// moved down by the strip until the notice goes, then back. A wide
    /// window's notice stands inline, with no strip.
    #[test]
    fn a_compact_windows_notice_has_its_own_strip_and_a_wide_ones_stands_inline() {
        use crate::widget::toast::ToastKind;
        use crate::window_toolbar::{ToolbarAction, ToolbarLocation, ToolbarNotice, WindowToolbar};
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            NamedApp,
            (420, 800),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        crate::window_toolbar::declare(Some(WindowToolbar {
            sidebar: Some(false),
            location: Some(ToolbarLocation {
                label: "Launch checklist".into(),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                ToolbarAction::new("share", "share", "Share"),
                ToolbarAction::new("pin", "pin", "Pin"),
            ],
            ..WindowToolbar::default()
        }));
        let quiet = harness.content_viewport((420, 800)).1;
        assert_eq!(quiet, 800 - bar_height_px(), "without a notice, one row");
        let text = "Save failed: your disk is full, so the note was not written";
        crate::window_toolbar::notify(Some(ToolbarNotice::new(text, ToastKind::Error).action(
            ToolbarAction::new("save-status-retry", "rotate-cw", "Try again"),
        )));

        let shown = harness.window_bar_notice(420.0).expect("the notice is up");
        let strip = shown
            .strip
            .expect("a compact window gives the notice a strip");
        assert_eq!(shown.text, text);
        assert!(shown.whole, "its whole text stands: {shown:?}");
        let button = shown
            .button
            .as_ref()
            .expect("Try again stands in the strip");
        assert!(
            button.labelled
                && button.bounds.width() > crate::widget::window_controls::CONTROL_PX
                && strip.contains(button.bounds.center()),
            "Try again names itself in the strip: {shown:?}"
        );
        assert!(
            (strip.y0 - TAB_BAR_HEIGHT).abs() < 0.5 && (shown.bar_height - strip.y1).abs() < 0.5,
            "the strip stands under the row, in the bar: {shown:?}"
        );
        let bar = harness.window_bar(420.0);
        let row = bar.toolbar_row();
        assert_eq!(
            row.toolbar()
                .and_then(|toolbar| toolbar.location.as_ref())
                .map(|location| location.label.as_str()),
            Some("Launch checklist"),
            "the row stands as it does without a notice"
        );
        assert!(row.slots().notice.area() <= 0.0);
        let pushed = harness.content_viewport((420, 800)).1;
        assert!(
            f64::from(quiet - pushed) >= strip.height() - 1.0,
            "the app's frame moves down by the strip: {quiet} to {pushed}"
        );

        let wide = harness.window_bar_notice(1280.0).expect("the notice is up");
        assert!(wide.strip.is_none(), "a wide window's notice stands inline");
        assert!(
            wide.whole && wide.button.as_ref().is_some_and(|button| button.labelled),
            "with Try again named after it: {wide:?}"
        );
        assert_eq!(
            harness.content_viewport((1280, 800)).1,
            800 - bar_height_px()
        );

        crate::window_toolbar::notify(None);
        assert_eq!(
            harness.content_viewport((420, 800)).1,
            quiet,
            "the app's frame moves back up once the notice goes"
        );
        crate::window_toolbar::declare(None);
    }

    /// The 1st of the calendar's weeks across a month: Mon, Jun 29 to
    /// Sun, Jul 5, 2026.
    fn week_across_a_month() -> crate::format::Period {
        let day = |m, d| chrono::NaiveDate::from_ymd_opt(2026, m, d).unwrap_or_default();
        crate::format::Period::Days(day(6, 29), day(7, 5))
    }

    /// The calendar's whole bar (the sidebar, previous, next and Today, a
    /// week as its period, five views, New event and a menu of more) in a
    /// compact (420 px) window that lists History, painted once so its
    /// views are measured.
    fn compact_calendar_bar() -> (HostedReset, FrameHarness, TabBar) {
        use crate::window_toolbar::{
            ToolbarAction, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
        };
        let hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            NamedApp,
            (420, 800),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        let view = |id: &str, label: &str| ToolbarView {
            id: id.into(),
            label: label.into(),
        };
        crate::window_toolbar::declare(Some(
            WindowToolbar {
                sidebar: Some(true),
                leading: vec![
                    ToolbarAction::new("previous", "chevron-left", "Previous week").navigating(),
                    ToolbarAction::new("next", "chevron-right", "Next week").navigating(),
                    ToolbarAction::new("today", "calendar", "Today").labelled(),
                ],
                location: Some(ToolbarLocation::period(week_across_a_month())),
                views: Some(ToolbarViews {
                    name: "View".into(),
                    choices: vec![
                        view("month", "Month"),
                        view("week", "Week"),
                        view("day", "Day"),
                        view("agenda", "Agenda"),
                        view("tasks", "Tasks"),
                    ],
                    chosen: "week".into(),
                }),
                actions: vec![
                    ToolbarAction::new("new-event", "plus", "New event"),
                    ToolbarAction::new("export", "upload", "Export calendar").in_menu(),
                    ToolbarAction::new("import", "download", "Import calendar").in_menu(),
                ],
                ..WindowToolbar::default()
            }
            .bounded(),
        ));
        let mut bar = harness.window_bar(420.0);
        // The window lists its History beside the slots, as the calendar's does.
        bar.set_history_control(TAB, Some(crate::widget::HistoryControl::of(false)));
        lay_out_bar(&mut bar, 420.0, &harness.theme);
        // The bar measures its views as it paints, as a capture paints it.
        bar.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut harness.text,
            &harness.theme,
            &harness.presence,
        );
        (hosted, harness, bar)
    }

    /// A compact (420 px) window's bar, beside its search and History, has
    /// no room for the calendar's views beside its navigation and period:
    /// the views and Today (which jumps rather than steps) go into the
    /// overflow menu, previous and next stay, and the period stays, elided.
    #[test]
    fn a_compact_window_keeps_its_navigation_and_location_and_folds_the_views() {
        use crate::a11y::{AccessNode, Role};
        fn find<'a>(node: &'a AccessNode, role: Role, name: &str) -> Option<&'a AccessNode> {
            if node.role == role && node.name.as_deref() == Some(name) {
                return Some(node);
            }
            node.children
                .iter()
                .find_map(|child| find(child, role, name))
        }
        let (_hosted, _harness, bar) = compact_calendar_bar();
        let rects = bar.test_rects();
        let rect = |name: &str| {
            rects
                .iter()
                .find(|(n, _)| n == name)
                .map_or(Rect::ZERO, |(_, r)| *r)
        };
        let sidebar = rect("window_sidebar");
        let previous = rect("window_leading:0");
        let next = rect("window_leading:1");
        let location = rect("window_location");
        let more = rect("window_more");
        let history = rect(crate::widget::tabs::WINDOW_HISTORY_TARGET);
        for (name, slot) in [
            ("the sidebar's toggle", sidebar),
            ("previous", previous),
            ("next", next),
            ("the period", location),
            ("the overflow button", more),
            ("History", history),
        ] {
            assert!(slot.area() > 0.0, "{name} stands in the bar: {rects:?}");
        }
        assert!(
            sidebar.x1 <= previous.x0 && previous.x1 <= next.x0 && next.x1 <= location.x0,
            "in their order: {rects:?}"
        );
        assert!(
            location.width() >= f64::from(crate::tokens::Metrics::BASE.location_min_width),
            "the period keeps at least its least width: {location:?}"
        );
        assert!(location.x1 <= more.x0, "{rects:?}");
        assert!(
            more.x1 + SEARCH_CONTROL_ROOM <= history.x0,
            "the window's search keeps its room before History: {rects:?}"
        );
        assert_eq!(rect("window_leading:2"), Rect::ZERO, "Today folds");
        assert_eq!(rect("window_views_menu"), Rect::ZERO, "so do the views");
        let tree = Widget::accessibility(&bar).expect("the bar is reachable");
        for label in ["Month", "Week", "Day", "Agenda", "Tasks"] {
            let item = find(&tree, Role::MenuItem, label)
                .unwrap_or_else(|| panic!("{label} is in the overflow menu"));
            assert_eq!(item.state.checked, Some(label == "Week"));
        }
        let today = find(&tree, Role::MenuItem, "Today").expect("Today is in the overflow menu");
        assert_eq!(
            today.action_target.as_deref(),
            Some("window-toolbar-leading:today"),
            "and runs as its button does"
        );
        crate::window_toolbar::declare(None);
    }

    /// A compact window's bar has no room for the week as `Mon, Jun 29 –
    /// Sun, Jul 5`: it names it `Jun 29 – Jul 5`, whole, instead of cutting
    /// the wider form short.
    #[test]
    fn a_compact_window_names_its_period_in_a_form_that_fits() {
        let (_hosted, mut harness, bar) = compact_calendar_bar();
        let location = bar
            .test_rects()
            .into_iter()
            .find(|(name, _)| name == "window_location")
            .map_or(Rect::ZERO, |(_, rect)| rect);
        assert!(location.area() > 0.0, "the period stands in the bar");
        let toolbar = crate::window_toolbar::declared().expect("the calendar declared its bar");
        let declared = toolbar.location.as_ref().expect("with its period");
        let forms = week_across_a_month().forms();
        assert_eq!(declared.label, forms[0]);
        let style = harness.theme.text(crate::tokens::TextRole::Body);
        let widest = f64::from(harness.text.measure_style(&forms[0], None, &style).0);
        assert!(
            widest > location.width(),
            "the widest form does not fit: {location:?}"
        );
        let (shown, _) = crate::widget::window_toolbar_row::location_shown(
            declared,
            bar.toolbar_row().slots().form,
            &harness.theme,
        );
        assert_eq!(
            shown, forms[1],
            "the bar names the week in its narrower form"
        );
        let shown_width = f64::from(harness.text.measure_style(shown, None, &style).0);
        assert!(shown_width <= location.width(), "whole");
        crate::window_toolbar::declare(None);
    }

    #[test]
    #[should_panic(expected = "wears its app's icon")]
    fn a_bar_with_no_app_to_wear_refuses_to_paint() {
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_root(
            crate::State::new(),
            crate::widget::Column::new(),
            (800, 600),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        harness.paint_window_bar(&mut Scene::new(), Affine::IDENTITY, 800.0);
    }

    /// A text the window's search finds in place and can replace in.
    struct Draft {
        text: String,
        found: Vec<crate::widget::find_bar::FindMatch>,
    }

    impl Widget for Draft {
        crate::provides!(dyn crate::widget::Findable);
        fn name(&self) -> &str {
            "Draft"
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &crate::theme::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            use crate::widget::TaffyTreeExt as _;
            tree.widget_leaf(taffy::Style::default(), nodes, self)
        }
    }

    impl crate::widget::AppWidget for Draft {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Files
        }
        fn app_title(&self) -> &str {
            "Writer"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("W")
        }
    }

    impl crate::widget::Findable for Draft {
        fn find(
            &mut self,
            query: &str,
            _options: crate::widget::find_bar::FindOptions,
        ) -> Vec<crate::widget::find_bar::FindMatch> {
            self.found = if query.is_empty() {
                Vec::new()
            } else {
                self.text
                    .match_indices(query)
                    .map(|(start, m)| {
                        crate::widget::find_bar::FindMatch::new(start, start + m.len())
                    })
                    .collect()
            };
            self.found.clone()
        }
        fn reveal(&mut self, _index: usize) {}
        fn supports_replace(&self) -> bool {
            true
        }
        fn content_revision(&self) -> u64 {
            u64::try_from(self.text.len()).unwrap_or(u64::MAX)
        }
    }

    // A scenario drives the window bar's search through its parts, as an
    // agent does on the desktop: open it, type, step, open the options and
    // type the replacement; what the window does not host is refused.
    #[test]
    fn a_scenario_drives_the_window_bars_search_through_its_parts() {
        use crate::a11y::Action;
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            Draft {
                text: "an apple and an end".into(),
                found: Vec::new(),
            },
            (800, 600),
            crate::theme::Theme::dark(),
        );
        assert!(
            harness
                .perform_semantic_named("window-search-open", None, &Action::Activate)
                .is_err(),
            "the search lives in the window bar"
        );
        harness.present_in_window_bar();
        let mut perform =
            |target: &str, action: Action| harness.perform_semantic_named(target, None, &action);
        perform("window-search-open", Action::Activate).unwrap();
        perform("window-search-query", Action::SetValue("an".into())).unwrap();
        perform("window-search-next", Action::Activate).unwrap();
        perform("window-search-options", Action::Expand).unwrap();
        perform("window-search-replacement", Action::SetValue("the".into())).unwrap();
        assert!(
            perform("window-search-hex", Action::Activate).is_err(),
            "a text offers no hex mode"
        );
        assert!(
            perform("window-search-everywhere", Action::Activate).is_err(),
            "the desktop's search is not the window's"
        );
        assert_eq!(harness.window_search_position(), Some((2, 3)));
        assert!(harness.window_search.options_open());
        assert_eq!(harness.window_search.replacement(), "the");
    }

    // What the app shows after the query was typed is found on the next
    // frame, as the live window drives its search once a frame.
    #[test]
    fn text_that_arrives_after_the_query_is_found_on_the_next_frame() {
        use crate::a11y::Action;
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            Draft {
                text: "an apple".into(),
                found: Vec::new(),
            },
            (800, 600),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        harness
            .perform_semantic_named("window-search-open", None, &Action::Activate)
            .unwrap();
        harness
            .perform_semantic_named("window-search-query", None, &Action::SetValue("an".into()))
            .unwrap();
        assert_eq!(harness.window_search_position(), Some((1, 1)));
        harness
            .widget_mut::<Draft>()
            .expect("the app is a draft")
            .text
            .push_str(" and an end");
        harness.relayout();
        assert_eq!(
            harness.window_search_position(),
            Some((1, 3)),
            "the count follows the new text"
        );
    }

    /// A list the window's search narrows.
    struct Rows {
        rows: Vec<&'static str>,
        filter: String,
    }

    impl Widget for Rows {
        crate::provides!(dyn crate::widget::Filterable);
        fn name(&self) -> &str {
            "Rows"
        }
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &crate::theme::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            use crate::widget::TaffyTreeExt as _;
            tree.widget_leaf(taffy::Style::default(), nodes, self)
        }
    }

    impl crate::widget::AppWidget for Rows {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Files
        }
        fn app_title(&self) -> &str {
            "Remote"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("R")
        }
    }

    impl crate::widget::Filterable for Rows {
        fn set_filter(&mut self, query: &str) {
            query.clone_into(&mut self.filter);
        }
        fn filter_count(&self) -> Option<crate::widget::FilterCount> {
            let shown = self
                .rows
                .iter()
                .filter(|row| row.contains(self.filter.as_str()))
                .count();
            Some(crate::widget::FilterCount::new(shown, self.rows.len()))
        }
    }

    // What a scenario types into the bar's search narrows the app's list
    // and stands in the bar's field, as on the desktop.
    #[test]
    fn a_scenario_narrows_a_list_through_the_window_bars_search() {
        use crate::a11y::Action;
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            Rows {
                rows: vec!["build-host", "build-arm", "laptop"],
                filter: String::new(),
            },
            (800, 600),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        harness
            .perform_semantic_named("window-search-open", None, &Action::Activate)
            .unwrap();
        harness
            .perform_semantic_named(
                "window-search-query",
                None,
                &Action::SetValue("build".into()),
            )
            .unwrap();
        assert_eq!(harness.window_search.query(), "build");
        assert_eq!(
            harness.window_search.count(),
            Some(crate::widget::FilterCount::new(2, 3))
        );
    }

    /// Hosting is per thread: a test leaves it as it found it.
    struct HostedReset;

    impl Drop for HostedReset {
        fn drop(&mut self) {
            crate::window_toolbar::set_hosted(false);
        }
    }
}
