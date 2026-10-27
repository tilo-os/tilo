//! The app's window-bar slots in a row of the frame's own, as a window of its
//! own shows them outside a Tilo session.
//!
//! A browser page that mounts one app is such a window: no desktop draws a
//! bar over it, so nothing would show the slots the app declares
//! ([`crate::window_toolbar`]), and an app whose controls are its slots
//! would have none. [`FrameHarness::host_window_slots`] makes the frame host
//! them in the same [`SlotRow`] the native runner draws across a window of
//! its own: the app's slots, its notice, the window's History in the row's
//! menu and the window's search at its end. The app's frame is laid out
//! under the row; the row is painted over it, takes the pointer and the
//! keyboard where they are its, and joins the frame's inspection as one
//! toolbar ([`WINDOW_SLOTS`]) whose slots a reader, a script or an agent
//! activates by their `window-toolbar-*` targets.

use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use super::{FrameHarness, InspectionNode};
use crate::search::SearchRequest;
use crate::slot_row::{RowInput, RowPlace, SlotRow, SlotRowHost};
use crate::widget::{HistoryControl, InPlace, Widget};
use crate::window_toolbar::{SearchOffer, SearchOpening, ToolbarEvent};

/// The name the row goes by in the frame's inspection and in its semantic
/// actions: its slots are performed on this owner with their targets.
pub const WINDOW_SLOTS: &str = "window-slots";

/// The row the frame hosts, and what it showed when last refreshed.
pub(super) struct HostedSlots {
    row: SlotRow,
    /// How far below the window's top the app's frame starts.
    inset: f64,
    /// The slots' revision the row last showed
    /// ([`crate::window_toolbar::revision`]).
    revision: u64,
}

/// The frame lent to its row: the app the row serves. A search of the app's
/// own the row asks for waits until the row is back in the frame.
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
        let ran = crate::standalone_history_host::history_control_binding(control)
            .invoke(())
            .map_err(|error| error.to_string())
            .and_then(|intent| {
                crate::history_mode::embedded::run_door_control(history, &intent)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = ran {
            tracing::warn!(%error, "window slots: History did not run");
        }
        self.frame.invalidate_local();
    }

    fn deliver(&mut self, event: ToolbarEvent) {
        let hands_keyboard = crate::window_toolbar::hands_keyboard_to_app(&event);
        crate::window_toolbar::deliver(event);
        if hands_keyboard {
            self.frame.hand_keyboard_to_app();
        }
    }

    fn slots_acted(&mut self) {
        self.frame.invalidate_local();
    }
}

impl FrameHarness {
    /// Host the app's window-bar slots in a row across the top of the
    /// frame, as a window of its own outside a Tilo session does (a browser
    /// page that mounts one app): the app is told its slots, search and
    /// History are hosted, its frame is laid out under the row, and the row
    /// is painted, routed and inspected with it. A frame standing in for the
    /// desktop's bar ([`Self::present_in_window_bar`]) already hosts them.
    pub fn host_window_slots(&mut self) {
        self.invalidate_local();
        if self.window_bar || self.slot_row.is_some() {
            return;
        }
        self.slot_row = Some(Box::new(HostedSlots {
            row: SlotRow::new(),
            inset: 0.0,
            revision: crate::window_toolbar::revision().wrapping_sub(1),
        }));
        crate::window_toolbar::set_hosted(true);
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            history.set_open_button(false);
            history.set_window_presents(true);
        }
        self.document_history.remove_fallback_bar();
        self.invalidate_layout_tree();
        self.relayout();
    }

    /// Whether the frame hosts the app's window-bar slots in a row of its
    /// own ([`Self::host_window_slots`]).
    #[must_use]
    pub fn hosts_window_slots(&self) -> bool {
        self.slot_row.is_some()
    }

    /// How far below the window's top the app's frame starts: the row's
    /// height while the frame hosts the slots.
    pub(super) fn slot_row_inset(&self) -> f64 {
        self.slot_row.as_ref().map_or(0.0, |hosted| hosted.inset)
    }

    /// Run `serve` with the row and the frame lent to it, then open the
    /// search of the app's own it asked for.
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

    /// Keep the row current before a frame lays out: what the app declares,
    /// the window's History and width. Returns whether the row changed, so
    /// a frame already drawn must be drawn again.
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
            // The app's frame starts at the row's new height.
            self.invalidate_layout_tree();
        }
        self.with_slot_row(|row, lent| row.drive_search(lent));
        if changed {
            self.invalidate_local();
        }
        changed
    }

    /// Whether the row asks for a frame: the app declared other slots since
    /// it last showed them, or a menu or the search's caret moves.
    pub(super) fn slot_row_wants_frame(&self) -> (bool, bool) {
        self.slot_row.as_ref().map_or((false, false), |hosted| {
            (
                hosted.revision != crate::window_toolbar::revision(),
                hosted.row.needs_frame(),
            )
        })
    }

    /// Paint the row over the app's frame, its menus and the search's
    /// options over the app. Unchanged, the row repaints nothing: while no
    /// menu is open and the search is closed it paints only its band.
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

    /// Hand `event` to the row when it is the row's, before the app sees
    /// it. Returns whether the row took it.
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
        let Some((took, redraw)) = self.with_slot_row(|row, lent| row.route(lent, event, &input))
        else {
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

    /// The row as the frame's inspection shows it: one toolbar of the
    /// app's slots and the window's search.
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
            layout: None,
            semantics: Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Toolbar)
                    .with_name(self.window_tab().title)
                    .with_bounds(window)
                    .with_children(hosted.row.accessibility()),
            ),
            children: Vec::new(),
        })
    }

    /// Perform `action` on the row's part `target` (a `window-toolbar-*`
    /// slot or a `window-search-*` part), as assistive technology and
    /// automation do, and show what the app made of it, as a press on the
    /// slot does.
    pub(super) fn perform_slot_row_semantic(
        &mut self,
        target: Option<&str>,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        let target = target.ok_or("a window slot is performed by its target")?;
        let ran = self
            .with_slot_row(|row, lent| row.perform(lent, target, action))
            .ok_or("this frame hosts no window slots")?;
        if !ran {
            return Err(format!("the window slot {target} took no {action:?} now"));
        }
        // What the app asked of the host when its sync took the press (the
        // Undo beside a notice asks for an undo) is done and shown, as
        // after a press on a slot.
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

    /// An app whose one control is a slot in the window's bar: a call's
    /// microphone, named for what a press does, as Chat declares it.
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

    /// The slot `target` as the frame's inspection shows it.
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

    // A window of its own outside a session (a browser page mounting one
    // app) hosts the app's slots in a row of its own: the app hears they are
    // hosted, its frame starts under the row, and the row's slots are in
    // the frame's semantics, named for what they do.
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

    // Activating the slot through the semantics a reader and an agent use
    // hands the app the press as the desktop's bar does, and the row shows
    // what the app declares after it.
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
}
