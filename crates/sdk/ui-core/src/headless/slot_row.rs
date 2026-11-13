//! The app's window-bar slots in a row of the frame's own, as a window of its own shows them outside a
//! Tilo session. A browser page that mounts one app is such a window: no desktop draws a bar over it,
//! so nothing would show the slots the app declares ([`crate::window_toolbar`]), and an app whose
//! controls are its slots would have none.

use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use super::{FrameHarness, InspectionNode};
use crate::host_core::KeyHost as _;
use crate::search::SearchRequest;
#[cfg(test)]
use crate::slot_row::WindowAction;
use crate::slot_row::{RowInput, RowPlace, SlotRow, SlotRowHost};
use crate::widget::{HistoryControl, InPlace, Widget};
use crate::window_toolbar::{SearchOffer, SearchOpening, ToolbarEvent};

/// The name the row goes by in the frame's inspection and in its semantic actions: its slots are
/// performed on this owner with their targets.
pub const WINDOW_SLOTS: &str = "window-slots";

/// The row the frame hosts, and what it showed when last refreshed.
pub(super) struct HostedSlots {
    row: SlotRow,
    inset: f64,
    revision: u64,
}

/// The frame lent to its row: the app the row serves.
struct Lent<'a> {
    frame: &'a mut FrameHarness,
    search: Option<SearchRequest>,
}

impl SlotRowHost for Lent<'_> {
    fn search_root(&mut self) -> &mut dyn Widget {
        self.frame.root.as_mut()
    }

    fn search_offer(&mut self) -> InPlace {
        SearchOffer::of(self.frame.root.as_mut()).into()
    }

    fn open_app_search(&mut self, request: SearchRequest) {
        self.search = Some(request);
    }

    fn search_drove(&mut self) {
        self.frame.invalidate_local();
    }

    fn takes_search(&mut self, opening: SearchOpening) -> bool {
        let offer = SearchOffer::of(self.frame.root.as_mut());
        match opening {
            SearchOpening::Find => offer != SearchOffer::Nothing,
            SearchOpening::Replace => offer == SearchOffer::Document,
        }
    }

    fn history_control(&self) -> Option<HistoryControl> {
        self.frame.window_history_control()
    }

    fn press_history(&mut self, control: HistoryControl) {
        let Some(history) = self.frame.history.as_mut() else {
            return;
        };
        if let Err(error) =
            crate::history_mode::host::press_window_control(history.as_mut(), control)
        {
            tracing::warn!(%error, "window slots: History did not run");
        }
        self.frame.invalidate_local();
    }

    fn deliver(&mut self, event: ToolbarEvent) {
        if crate::window_toolbar::deliver_to_app(event) {
            self.frame.hand_keyboard_to_app();
        }
    }

    fn slots_acted(&mut self) {
        self.frame.invalidate_local();
    }
}

impl FrameHarness {
    /// Host the app's window-bar slots in a row across the top of the frame, as a window of its own
    /// outside a Tilo session does (a browser page that mounts one app): the app is told its slots,
    /// search and History are hosted, its frame is laid out under the row, and the row is painted,
    /// routed and inspected with it.
    pub fn host_window_slots(&mut self) {
        self.invalidate_local();
        if self.window_bar || self.slot_row.is_some() {
            return;
        }
        // The row's slots are this window's, as the desktop bar's are
        // ([`Self::present_in_window_bar`]): a process laying out several windows keeps each
        // row to its own window.
        self.host_bar_window();
        let _running = self.running();
        self.slot_row = Some(Box::new(HostedSlots {
            row: SlotRow::new(),
            inset: 0.0,
            revision: crate::window_toolbar::revision().wrapping_sub(1),
        }));
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            history.set_open_button(false);
            history.set_window_presents(true);
        }
        self.invalidate_layout_tree();
        self.relayout();
    }

    /// Whether the frame hosts the app's window-bar slots in a row of its own
    /// ([`Self::host_window_slots`]).
    #[must_use]
    pub fn hosts_window_slots(&self) -> bool {
        self.slot_row.is_some()
    }

    /// The frame is a window of its own on a desktop that leaves the frame to the app
    /// ([`Self::host_window_slots`]): its row carries the window's minimize, maximize and close in
    /// their pill at its trailing end, as the native runner's row does, and a press on the row's empty
    /// ground moves the window.
    pub fn host_window_controls(&mut self) {
        self.invalidate_local();
        let Some(hosted) = self.slot_row.as_mut() else {
            return;
        };
        hosted.row.set_window_controls(true);
        self.relayout();
    }

    /// Whether the frame's row carries the window's controls ([`Self::host_window_controls`]).
    #[must_use]
    pub fn hosts_window_controls(&self) -> bool {
        self.slot_row
            .as_ref()
            .is_some_and(|hosted| hosted.row.shows_window_controls())
    }

    /// How far below the window's top the app's frame starts: the row's height while the frame hosts
    /// the slots.
    pub(super) fn slot_row_inset(&self) -> f64 {
        self.slot_row.as_ref().map_or(0.0, |hosted| hosted.inset)
    }

    /// Run `serve` with the row and the frame lent to it, then open the search of the app's own it
    /// asked for.
    fn with_slot_row<R>(
        &mut self,
        serve: impl FnOnce(&mut SlotRow, &mut Lent<'_>) -> R,
    ) -> Option<R> {
        let mut hosted = self.slot_row.take()?;
        let mut lent = Lent {
            frame: self,
            search: None,
        };
        let result = serve(&mut hosted.row, &mut lent);
        let search = lent.search.take();
        self.slot_row = Some(hosted);
        if let Some(request) = search
            && let Err(error) = self.open_search(&request)
        {
            tracing::warn!(%error, "window slots: the app's search did not open");
        }
        Some(result)
    }

    /// Keep the row current before a frame lays out: what the app declares, the window's History and
    /// width. Returns whether the row changed, so a frame already drawn must be drawn again.
    pub(super) fn refresh_slot_row(&mut self) -> bool {
        if self.slot_row.is_none() {
            return false;
        }
        self.with_slot_row(|row, lent| row.serve_app_requests(lent));
        let title = self.window_tab().title;
        let history = self.window_history_control();
        let window = (f64::from(self.viewport.0), f64::from(self.viewport.1));
        let revision = crate::window_toolbar::revision();
        let Some(hosted) = self.slot_row.as_mut() else {
            return false;
        };
        hosted.revision = revision;
        let height = hosted.row.refresh(
            &self.theme,
            Some(RowPlace {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                window,
                app_name: &title,
            }),
            history,
        );
        let mut changed = hosted.row.take_repaint();
        if (hosted.inset - height).abs() > f64::EPSILON {
            hosted.inset = height;
            changed = true;
            self.invalidate_layout_tree();
        }
        self.with_slot_row(|row, lent| row.drive_search(lent));
        if changed {
            self.invalidate_local();
        }
        changed
    }

    /// Whether the row asks for a frame: the app declared other slots since it last showed them, or a
    /// menu or the search's caret moves.
    pub(super) fn slot_row_wants_frame(&self) -> (bool, bool) {
        self.slot_row.as_ref().map_or((false, false), |hosted| {
            (
                hosted.revision != crate::window_toolbar::revision(),
                hosted.row.needs_frame(),
            )
        })
    }

    /// Paint the row over the app's frame, its menus and the search's options over the app.
    pub(super) fn draw_slot_row(&mut self, transform: Affine) {
        let Some(hosted) = self.slot_row.as_mut() else {
            return;
        };
        if !hosted.row.is_shown() {
            return;
        }
        let window = Rect::new(
            0.0,
            0.0,
            f64::from(self.viewport.0),
            f64::from(self.viewport.1),
        );
        let bounds = hosted.row.paint_bounds(0.0, &self.theme, window);
        let (scene, text, theme, presence) =
            (&mut self.scene, &mut self.text, &self.theme, &self.presence);
        crate::scene_parts::part(
            scene,
            crate::scene_parts::PartKey::new("window-slots", 0),
            vec![bounds],
            |scene| {
                hosted
                    .row
                    .draw(scene, transform, text, theme, presence, 0.0)
            },
        );
    }

    /// Hand `event` to the row when it is the row's, before the app sees it. Returns whether the row
    /// took it.
    pub(super) fn slot_row_event(&mut self, event: &WindowEvent) -> bool {
        if self.slot_row.is_none() {
            return false;
        }
        let modifiers = self.modifiers;
        let input = RowInput {
            scale: self.input_scale,
            modifiers: &modifiers,
            platform: self.platform,
        };
        let Some((took, redraw)) = self.with_slot_row(|row, lent| {
            let routed = row.route(lent, event, &input);
            let _ = row.take_window_actions();
            routed
        }) else {
            return false;
        };
        if took || redraw {
            self.invalidate_local();
            if self.coalesce_input_frames {
                self.input_frame_pending = true;
            } else {
                self.relayout();
            }
        }
        took
    }

    /// The row as the frame's inspection shows it: one toolbar of the app's slots and the window's
    /// search.
    pub(super) fn slot_row_inspection(&self) -> Option<InspectionNode> {
        let hosted = self.slot_row.as_ref()?;
        if !hosted.row.is_shown() {
            return None;
        }
        let window = Rect::new(0.0, 0.0, f64::from(self.viewport.0), hosted.inset);
        Some(InspectionNode {
            path: WINDOW_SLOTS.into(),
            widget: "WindowSlots".into(),
            test_id: None,
            focusable: false,
            rect: [window.x0, window.y0, window.width(), window.height()],
            child_clip: None,
            page_title: None,
            layout: None,
            paint_key: None,
            semantics: Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Toolbar)
                    .with_name(self.window_tab().title)
                    .with_bounds(window)
                    .with_children(hosted.row.accessibility()),
            ),
            children: Vec::new(),
        })
    }

    /// The row as HTML: a toolbar across the top of the window, each slot a real button showing its
    /// face, named as a reader hears it, and the location as its words.
    pub(super) fn slot_row_composition(&self) -> Option<crate::composition::Composed> {
        use crate::composition::{Composed, Own};
        use std::hash::{Hash, Hasher};
        let hosted = self.slot_row.as_ref()?;
        if !hosted.row.is_shown() {
            return None;
        }
        let window = Rect::new(0.0, 0.0, f64::from(self.viewport.0), hosted.inset);
        let nodes = hosted.row.accessibility();
        let named = |key: &str| {
            nodes
                .iter()
                .find(|node| node.action_target.as_deref() == Some(key))
                .cloned()
        };
        let mut row = Composed::new(
            WINDOW_SLOTS.into(),
            "WindowSlots",
            [window.x0, window.y0, window.width(), window.height()],
        );
        row.tag = Some("header");
        row.semantics = Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Toolbar)
                .with_name(self.window_tab().title),
        );
        if !hosted.row.composes() {
            // Its paint, with each slot a real button over it.
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_string(&nodes)
                .unwrap_or_default()
                .hash(&mut hash);
            window.width().to_bits().hash(&mut hash);
            format!("{:?}", self.theme.tokens.colors.text_primary).hash(&mut hash);
            row.own = Some(Own::Painted {
                version: hash.finish(),
            });
            row.children = nodes
                .iter()
                .filter_map(|slot| {
                    let rect = slot.bounds?.to_rect();
                    (rect.width() > 0.0 && rect.height() > 0.0).then(|| {
                        let mut button = Composed::new(
                            format!(
                                "{WINDOW_SLOTS}#{}",
                                slot.action_target.as_deref().unwrap_or_default()
                            ),
                            "WindowSlot",
                            [rect.x0, rect.y0, rect.width(), rect.height()],
                        );
                        button.tag = Some("button");
                        button.semantics = Some(slot.clone());
                        button.focusable = true;
                        button
                    })
                })
                .collect();
            return Some(row);
        }
        row.surfaces.push((
            [window.x0, window.y0, window.width(), window.height()],
            "ground",
        ));
        let strip = hosted.row.strip_card(&self.theme);
        for (key, rect, own) in hosted.row.composed_parts(&self.theme) {
            let notice = key == crate::widget::window_toolbar_row::STRIP_KEY;
            let location = key == crate::widget::window_toolbar_row::LOCATION_KEY;
            let mut part = Composed::new(
                format!("{WINDOW_SLOTS}#{key}"),
                "WindowSlot",
                [rect.x0, rect.y0, rect.width(), rect.height()],
            );
            part.own = Some(own);
            if notice {
                part.widget = "WindowNotice".into();
                part.semantics = nodes
                    .iter()
                    .find(|node| node.live.is_some() && node.role == crate::a11y::Role::StaticText)
                    .cloned();
            } else if location {
                part.widget = "WindowLocation".into();
                part.semantics =
                    named(crate::widget::window_toolbar_row::LOCATION_TARGET).or_else(|| {
                        nodes
                            .iter()
                            .find(|node| node.bounds.is_some_and(|b| b.to_rect() == rect))
                            .cloned()
                    });
                if part
                    .semantics
                    .as_ref()
                    .is_some_and(|node| node.action_target.is_some())
                {
                    part.tag = Some("button");
                    part.focusable = true;
                }
            } else {
                part.tag = Some("button");
                part.focusable = true;
                part.semantics = named(&key);
            }
            row.children.push(part);
        }
        if let Some(band) = strip {
            let mut card = Composed::new(
                format!("{WINDOW_SLOTS}#strip"),
                "WindowNoticeStrip",
                [band.x0, band.y0, band.width(), band.height()],
            );
            card.surfaces
                .push(([band.x0, band.y0, band.width(), band.height()], "raised"));
            if let Some(index) = row
                .children
                .iter()
                .position(|child| child.widget == "WindowNotice")
            {
                card.children.push(row.children.remove(index));
            }
            row.children.push(card);
        }
        Some(row)
    }

    /// The row's band alone, at `scale`, for its picture.
    pub(super) fn paint_slot_row(&mut self, scale: f64) -> Option<(vello::Scene, [f64; 4])> {
        let hosted = self.slot_row.as_mut()?;
        let width = f64::from(self.viewport.0);
        let mut scene = vello::Scene::new();
        hosted.row.draw(
            &mut scene,
            Affine::scale(scale),
            &mut self.text,
            &self.theme,
            &self.presence,
            0.0,
        );
        Some((scene, [0.0, 0.0, width, hosted.inset]))
    }

    /// The window's own actions pressed through the row's semantics since the last call.
    #[cfg(test)]
    pub(super) fn take_slot_row_window_actions(&mut self) -> Vec<WindowAction> {
        self.with_slot_row(|row, _| row.take_window_actions())
            .unwrap_or_default()
    }

    /// Perform `action` on the row's part `target` (a `window-toolbar-*` slot or a `window-search-*`
    /// part), as assistive technology and automation do, and show what the app made of it, as a press
    /// on the slot does.
    pub(super) fn perform_slot_row_semantic(
        &mut self,
        target: Option<&str>,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        let Some(target) = target else {
            return Ok(());
        };
        let ran = self
            .with_slot_row(|row, lent| row.perform(lent, target, action))
            .ok_or("this frame hosts no window slots")?;
        if !ran {
            return Err(format!("the window slot {target} took no {action:?} now"));
        }
        self.relayout();
        self.host_tick();
        self.invalidate_local();
        self.relayout();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
    use crate::window_toolbar::{ToolbarAction, WindowToolbar};
    use std::cell::RefCell;
    use std::rc::Rc;

    const MUTE: &str = "window-toolbar-action:call-mute";

    /// An app whose one control is a slot in the window's bar: a call's microphone, named for what a
    /// press does, as Chat declares it.
    struct CallApp {
        content: crate::widget::Column,
        muted: bool,
        heard: Rc<RefCell<Vec<ToolbarEvent>>>,
    }

    impl crate::widget::Decorator for CallApp {
        type Inner = crate::widget::Column;

        fn inner(&self) -> &Self::Inner {
            &self.content
        }

        fn inner_mut(&mut self) -> &mut Self::Inner {
            &mut self.content
        }

        fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            for event in crate::window_toolbar::take_events() {
                if event
                    == (ToolbarEvent::Action {
                        id: "call-mute".into(),
                    })
                {
                    self.muted = !self.muted;
                }
                self.heard.borrow_mut().push(event);
            }
            let name = if self.muted { "Unmute" } else { "Mute" };
            crate::window_toolbar::declare(Some(WindowToolbar {
                actions: vec![ToolbarAction::new("call-mute", "mic", name)],
                location: Some(crate::window_toolbar::ToolbarLocation {
                    label: "Call".into(),
                    ..crate::window_toolbar::ToolbarLocation::default()
                }),
                ..WindowToolbar::default()
            }));
            self.content.sync(state, path);
        }
    }

    fn call_app() -> (FrameHarness, Rc<RefCell<Vec<ToolbarEvent>>>) {
        let heard = Rc::new(RefCell::new(Vec::new()));
        let app = CallApp {
            content: crate::widget::Column::new()
                .push(Box::new(crate::widget::Button::new("send", "Send"))),
            muted: true,
            heard: Rc::clone(&heard),
        };
        let mut harness = FrameHarness::for_root(
            crate::State::new(),
            app,
            (900, 600),
            crate::theme::Theme::dark().with_reduce_motion(true),
        );
        harness.host_window_slots();
        harness.relayout();
        (harness, heard)
    }

    fn slot(harness: &FrameHarness, target: &str) -> Option<crate::a11y::AccessNode> {
        fn find(node: &crate::a11y::AccessNode, target: &str) -> Option<crate::a11y::AccessNode> {
            if node.action_target.as_deref() == Some(target) {
                return Some(node.clone());
            }
            node.children.iter().find_map(|child| find(child, target))
        }
        let inspection = harness.inspection();
        let row = inspection
            .root
            .children
            .iter()
            .find(|node| node.path == WINDOW_SLOTS)?;
        find(row.semantics.as_ref()?, target)
    }

    #[test]
    fn the_frame_hosts_the_apps_slots_in_a_row_above_its_frame() {
        let (harness, _) = call_app();
        assert!(harness.hosts_window_slots());
        assert!(
            crate::window_toolbar::is_hosted(),
            "the app hears its slots are hosted"
        );
        let frame = Widget::get_rect(harness.root::<CallApp>());
        assert!(
            (frame.y0 - TOOLBAR_ROW_HEIGHT).abs() < f64::EPSILON,
            "the app's frame starts under the row: {frame:?}"
        );
        assert!((frame.height() - (600.0 - TOOLBAR_ROW_HEIGHT)).abs() < f64::EPSILON);
        let mute = slot(&harness, MUTE).expect("the microphone stands in the row");
        assert_eq!(mute.name.as_deref(), Some("Unmute"));
        let bounds = mute.bounds.expect("the slot says where it stands");
        assert!(bounds.y1 <= TOOLBAR_ROW_HEIGHT && bounds.x1 > bounds.x0);
        assert!(
            slot(&harness, "window-search-open").is_some(),
            "the row ends with the window's search"
        );
    }

    #[test]
    fn a_window_of_its_own_has_one_bar_with_the_windows_controls() {
        let (mut harness, _) = call_app();
        assert!(!harness.hosts_window_controls(), "a page hosts no controls");
        harness.host_window_controls();
        assert!(harness.hosts_window_controls());
        let frame = Widget::get_rect(harness.root::<CallApp>());
        assert!(
            (frame.y0 - TOOLBAR_ROW_HEIGHT).abs() < f64::EPSILON,
            "the window has one bar: the app starts under it, not under two: {frame:?}"
        );
        let row = harness
            .inspection()
            .root
            .children
            .into_iter()
            .find(|node| node.path == WINDOW_SLOTS)
            .expect("the row stands in the frame");
        assert!(
            (row.rect[3] - TOOLBAR_ROW_HEIGHT).abs() < 0.5,
            "the row is one bar tall: {:?}",
            row.rect
        );
    }

    #[test]
    fn activating_a_slot_reaches_the_app_and_the_row_shows_its_outcome() {
        let (mut harness, heard) = call_app();
        harness
            .perform_semantic_named(WINDOW_SLOTS, Some(MUTE), &crate::a11y::Action::Activate)
            .expect("the slot runs");
        assert_eq!(
            heard.borrow().as_slice(),
            [ToolbarEvent::Action {
                id: "call-mute".into()
            }]
        );
        let mute = slot(&harness, MUTE).expect("the microphone stays in the row");
        assert_eq!(
            mute.name.as_deref(),
            Some("Mute"),
            "the row shows the app's new state"
        );
        assert!(
            harness
                .perform_semantic_named(
                    WINDOW_SLOTS,
                    Some("window-toolbar-action:missing"),
                    &crate::a11y::Action::Activate
                )
                .is_err(),
            "a slot the app does not declare runs nothing"
        );
    }

    // A person's press on the slot, where the row paints it, is the same
    // press, and never reaches the app's own controls under the row.
    #[test]
    fn a_pointer_press_on_a_slot_reaches_the_app() {
        let (mut harness, heard) = call_app();
        let bounds = slot(&harness, MUTE)
            .and_then(|mute| mute.bounds)
            .expect("the slot stands in the row");
        let (x, y) = ((bounds.x0 + bounds.x1) / 2.0, (bounds.y0 + bounds.y1) / 2.0);
        harness.send_host_event(&crate::synthetic_input::cursor_moved(x, y));
        harness.send_host_event(&crate::synthetic_input::mouse_press());
        harness.send_host_event(&crate::synthetic_input::mouse_release());
        harness.relayout();
        assert_eq!(
            heard.borrow().as_slice(),
            [ToolbarEvent::Action {
                id: "call-mute".into()
            }]
        );
        assert_eq!(
            slot(&harness, MUTE).and_then(|mute| mute.name).as_deref(),
            Some("Mute")
        );
    }

    #[test]
    fn the_windows_controls_are_named_and_run_for_a_reader() {
        let (mut harness, _) = call_app();
        harness.host_window_controls();
        for (target, name) in [
            ("window-control-minimize", "Minimize"),
            ("window-control-maximize", "Maximize"),
            ("window-control-close", "Close"),
        ] {
            let control = slot(&harness, target).expect("the control is reachable");
            assert_eq!(control.name.as_deref(), Some(name), "{target}");
            assert!(control.state.focusable, "{target} takes focus");
            let bounds = control.bounds.expect("the control says where it stands");
            assert!(
                bounds.y1 <= TOOLBAR_ROW_HEIGHT && bounds.x1 > bounds.x0,
                "{target} stands in the bar: {bounds:?}"
            );
        }
        harness
            .perform_semantic_named(
                WINDOW_SLOTS,
                Some("window-control-close"),
                &crate::a11y::Action::Activate,
            )
            .expect("the control runs");
        assert_eq!(
            harness.take_slot_row_window_actions(),
            [WindowAction::Close],
            "the press reaches the window"
        );
    }

    // The bar names what the window is on: a label at the row's location
    // that names no target, because a press on it runs nothing that a press
    // on the row's own background does not. A reader that presses it is
    // told nothing was refused, and no slot runs.
    #[test]
    fn a_press_on_the_rows_own_label_runs_nothing_and_is_not_refused() {
        let (mut harness, heard) = call_app();
        let named = |harness: &FrameHarness, name: &str| {
            fn find(node: &crate::a11y::AccessNode, name: &str) -> bool {
                node.name.as_deref() == Some(name)
                    || node.children.iter().any(|child| find(child, name))
            }
            let inspection = harness.inspection();
            inspection
                .root
                .children
                .iter()
                .find(|node| node.path == WINDOW_SLOTS)
                .and_then(|row| row.semantics.as_ref())
                .is_some_and(|row| find(row, name))
        };
        assert!(
            named(&harness, "Call"),
            "the call is named in the row: {:?}",
            slot(&harness, MUTE).and_then(|mute| mute.name)
        );
        harness
            .perform_semantic_named(WINDOW_SLOTS, None, &crate::a11y::Action::Activate)
            .expect("a press on the row's own label is not refused");
        assert!(
            heard.borrow().is_empty(),
            "no slot ran: {:?}",
            heard.borrow()
        );
        assert!(harness.take_slot_row_window_actions().is_empty());
    }

    /// An app whose bar holds a menu, as a list's sort menu does: the row's button says what the menu
    /// is, and the rows it lists run by their id.
    struct SortApp {
        content: crate::widget::Column,
        heard: Rc<RefCell<Vec<ToolbarEvent>>>,
    }

    impl crate::widget::Decorator for SortApp {
        type Inner = crate::widget::Column;

        fn inner(&self) -> &Self::Inner {
            &self.content
        }

        fn inner_mut(&mut self) -> &mut Self::Inner {
            &mut self.content
        }

        fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            for event in crate::window_toolbar::take_events() {
                self.heard.borrow_mut().push(event);
            }
            crate::window_toolbar::declare(Some(WindowToolbar {
                actions: vec![
                    ToolbarAction::new("sort", "arrow-up-down", "Sort")
                        .labelled()
                        .with_items(vec![
                            ToolbarAction::new("sort~row-0", "", "Everything"),
                            ToolbarAction::new("sort~row-1", "", "Title"),
                        ]),
                ],
                ..WindowToolbar::default()
            }));
            self.content.sync(state, path);
        }
    }

    fn sort_app() -> (FrameHarness, Rc<RefCell<Vec<ToolbarEvent>>>) {
        let heard = Rc::new(RefCell::new(Vec::new()));
        let app = SortApp {
            content: crate::widget::Column::new()
                .push(Box::new(crate::widget::Button::new("send", "Send"))),
            heard: Rc::clone(&heard),
        };
        let mut harness = FrameHarness::for_root(
            crate::State::new(),
            app,
            (900, 600),
            crate::theme::Theme::dark().with_reduce_motion(true),
        );
        harness.host_window_slots();
        harness.relayout();
        (harness, heard)
    }

    #[test]
    fn a_menu_in_a_window_of_its_own_opens_on_one_click_and_stays_open() {
        let (mut harness, heard) = sort_app();
        let button = slot(&harness, "window-toolbar-action:sort")
            .and_then(|button| button.bounds)
            .expect("the sort menu stands in the row");
        let (x, y) = ((button.x0 + button.x1) / 2.0, (button.y0 + button.y1) / 2.0);
        harness.send_host_event(&crate::synthetic_input::cursor_moved(x, y));
        harness.send_host_event(&crate::synthetic_input::mouse_press());
        harness.send_host_event(&crate::synthetic_input::mouse_release());
        harness.relayout();
        let rows = |harness: &FrameHarness| {
            let inspection = harness.inspection();
            let row = inspection
                .root
                .children
                .iter()
                .find(|node| node.path == WINDOW_SLOTS);
            let mut names = Vec::new();
            fn walk(node: &crate::a11y::AccessNode, names: &mut Vec<String>) {
                if let Some(name) = &node.name {
                    names.push(name.clone());
                }
                for child in &node.children {
                    walk(child, names);
                }
            }
            if let Some(semantics) = row.and_then(|row| row.semantics.as_ref()) {
                walk(semantics, &mut names);
            }
            names
        };
        assert!(
            rows(&harness).iter().any(|name| name == "Title"),
            "one click opens the menu: {:?}",
            rows(&harness)
        );
        for _ in 0..4 {
            harness.relayout();
        }
        assert!(
            rows(&harness).iter().any(|name| name == "Title"),
            "the menu stays open: {:?}",
            rows(&harness)
        );
        assert!(
            heard.borrow().is_empty(),
            "opening the menu runs none of its rows"
        );
    }
}
