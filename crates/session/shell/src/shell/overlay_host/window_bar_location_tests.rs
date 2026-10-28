//! An app's address edited in its window's bar, through the desktop's own
//! input path: the pointer and keys go through `FrameBuilder::dispatch_event`
//! as the compositor hands them over while the bar's field holds them, and
//! what the person chose leaves for the app on its connection.

use automerge::{AutoCommit, ROOT};
use vello::kurbo::{Point, Rect};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::NamedKey;

use crate::shell::frame::FrameBuilder;
use crate::shell::state::TileContent;
use ui::synthetic_input::{char_key, named_key};
use ui::window_toolbar::{LocationSuggestion, ToolbarEvent, ToolbarLocation, WindowToolbar};

const APP: &str = "so.tilo.browser";

/// The browser's slots: its address, and the suggestions it answers `typed`
/// with (none for nothing typed).
fn slots(typed: &str) -> WindowToolbar {
    let suggestions = if typed.is_empty() {
        Vec::new()
    } else {
        vec![
            LocationSuggestion {
                label: typed.into(),
                value: format!("https://{typed}/"),
                icon: Some("globe".into()),
                detail: String::new(),
            },
            LocationSuggestion {
                label: typed.into(),
                value: format!("https://duckduckgo.com/?q={typed}"),
                icon: Some("search".into()),
                detail: "Search with DuckDuckGo".into(),
            },
        ]
    };
    WindowToolbar {
        location: Some(ToolbarLocation {
            label: "duckduckgo.com".into(),
            value: "https://duckduckgo.com/".into(),
            editable: true,
            suggestions,
            ..ToolbarLocation::default()
        }),
        ..WindowToolbar::default()
    }
}

struct Desk {
    fb: FrameBuilder,
    tab: u64,
}

impl Desk {
    /// The focused window shows the browser, as its own process's window.
    fn new() -> Self {
        let mut fb = FrameBuilder::new((1600, 900));
        let tab = crate::shell::window_actions::active_tab_id(&fb.chrome()).expect("a focused tab");
        fb.with_chrome(|st| {
            let mut tree = st.tree_mut();
            for t in tree
                .desktop
                .workspaces
                .iter_mut()
                .flat_map(|workspace| &mut workspace.strips)
                .flat_map(|strip| &mut strip.window_groups)
                .flat_map(|group| &mut group.tabs)
            {
                if t.id == tab {
                    t.tile = TileContent::Foreign {
                        app_id: APP.into(),
                        surface_id: 9_101,
                    };
                }
            }
        });
        let mut desk = Self { fb, tab };
        desk.settle();
        desk.declare("");
        desk
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            let mut doc = AutoCommit::new();
            self.fb.pump_apps(&mut doc, &ROOT);
            self.fb.relayout();
            let _ = self.fb.paint();
        }
    }

    fn declare(&mut self, typed: &str) {
        self.fb
            .host_mut()
            .declare_tab_toolbar_for_tests(self.tab, Some(slots(typed)));
        self.settle();
    }

    fn dispatch(&mut self, event: &WindowEvent) {
        let mut doc = AutoCommit::new();
        let mut focus = None;
        self.fb.dispatch_event(
            event,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            &mut focus,
        );
    }

    fn click(&mut self, p: Point) {
        self.dispatch(&ui::synthetic_input::cursor_moved(p.x, p.y));
        self.dispatch(&ui::synthetic_input::mouse_press());
        self.dispatch(&ui::synthetic_input::mouse_release());
    }

    fn tap(&mut self, p: Point) {
        for phase in [
            winit::event::TouchPhase::Started,
            winit::event::TouchPhase::Ended,
        ] {
            self.dispatch(&WindowEvent::Touch(winit::event::Touch {
                device_id: winit::event::DeviceId::dummy(),
                phase,
                location: winit::dpi::PhysicalPosition::new(p.x, p.y),
                force: None,
                id: 3,
            }));
        }
    }

    fn named(&self, name: &str) -> Option<Rect> {
        self.fb
            .widget_registry()
            .get(name)
            .copied()
            .filter(|rect| rect.area() > 0.0)
    }

    /// What the bar sent the app since the last call.
    fn sent(&mut self) -> Vec<ToolbarEvent> {
        self.fb
            .host_mut()
            .take_toolbar_outbox()
            .into_iter()
            .map(|(_, event)| event)
            .collect()
    }

    /// Click the address and type `text`; the app answers with its
    /// suggestions.
    fn type_address(&mut self, text: &str) {
        let location = self
            .named("window_location")
            .expect("the bar shows the address");
        self.click(location.center());
        self.settle();
        assert!(
            self.fb.host().window_location_editing(),
            "a click on the address edits it"
        );
        for ch in text.chars() {
            self.dispatch(&char_key(&ch.to_string()));
        }
        self.settle();
        assert!(
            self.sent()
                .iter()
                .any(|event| *event == ToolbarEvent::Edit { text: text.into() }),
            "the app hears what is typed"
        );
        self.declare(text);
    }
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the desk (a frame builder) is what the test drives, up to its last assertion"
)]
fn enter_in_a_windows_bar_address_commits_what_was_typed() {
    let mut desk = Desk::new();
    desk.type_address("youtube.com");
    let first = desk
        .named("window_suggestion:0")
        .expect("the bar lists the app's suggestions");
    let location = desk.named("window_location").expect("the address");
    assert!(
        first.y0 >= location.y1 && (first.x0 - location.x0).abs() < 12.0,
        "the list hangs under the address: {first:?} under {location:?}"
    );
    desk.dispatch(&named_key(NamedKey::Enter));
    desk.settle();
    assert_eq!(
        desk.sent(),
        vec![ToolbarEvent::Commit {
            text: "youtube.com".into()
        }],
        "Enter commits what was typed to the app"
    );
    assert!(!desk.fb.host().window_location_editing());
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the desk (a frame builder) is what the test drives, up to its last assertion"
)]
fn a_click_or_tap_on_a_windows_bar_suggestion_commits_it() {
    for tap in [false, true] {
        let mut desk = Desk::new();
        desk.type_address("youtube.com");
        let second = desk
            .named("window_suggestion:1")
            .expect("the bar lists the app's suggestions");
        if tap {
            desk.tap(second.center());
        } else {
            desk.click(second.center());
        }
        desk.settle();
        assert_eq!(
            desk.sent(),
            vec![ToolbarEvent::Commit {
                text: "https://duckduckgo.com/?q=youtube.com".into()
            }],
            "the chosen suggestion goes to the app (tap: {tap})"
        );
    }
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the desk (a frame builder) is what the test drives, up to its last assertion"
)]
fn arrows_choose_a_windows_bar_suggestion_and_escape_puts_the_address_back() {
    let mut desk = Desk::new();
    desk.type_address("youtube.com");
    desk.dispatch(&named_key(NamedKey::ArrowDown));
    desk.dispatch(&named_key(NamedKey::ArrowDown));
    desk.dispatch(&named_key(NamedKey::Enter));
    desk.settle();
    assert_eq!(
        desk.sent(),
        vec![ToolbarEvent::Commit {
            text: "https://duckduckgo.com/?q=youtube.com".into()
        }]
    );

    let mut desk = Desk::new();
    desk.type_address("youtube.com");
    desk.dispatch(&named_key(NamedKey::Escape));
    desk.settle();
    assert!(!desk.fb.host().window_location_editing());
    assert!(
        desk.named("window_suggestion:0").is_none(),
        "the list goes with the edit"
    );
    assert!(
        desk.sent()
            .iter()
            .all(|event| !matches!(event, ToolbarEvent::Commit { .. })),
        "Escape commits nothing"
    );
}
