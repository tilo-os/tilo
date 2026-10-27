//! An app in a process of its own, searched and its History opened from the
//! window's bar over the real shell-ui connection: the app runs its own
//! runtime on one end of a socket, the desktop's window on the other, and
//! nothing passes between them but the protocol's frames.

#![cfg(all(feature = "native", unix))]

use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::net::UnixStream;

use vello::kurbo::{Point, Rect};
use winit::keyboard::NamedKey;

use crate::floating_surface::{
    FloatingSurfaceClientMessage, FloatingSurfaceHostMessage, FloatingSurfaceServiceClient,
};
use crate::synthetic_input::{char_key, named_key};
use crate::testing::Dispatch;
use crate::theme::Theme;
use crate::widget::find_bar::{FindMatch, FindOptions, Findable};
use crate::widget::{
    AppKind, HistoryControl, InPlace, TabDescriptor, TileContent, Widget, WindowGroup, WindowWidth,
};
use crate::window_toolbar::{HistoryOffer, SearchOffer, ToolbarEvent, WindowOffer};

use super::super::App;

/// The note the app shows: finds in its text, and keeps what it found and
/// which match is current.
#[derive(Default)]
struct Note {
    text: String,
    found: usize,
    current: Option<usize>,
    cleared: bool,
    rect: Rect,
}

impl Findable for Note {
    fn find(&mut self, query: &str, _options: FindOptions) -> Vec<FindMatch> {
        if query.is_empty() {
            self.found = 0;
            return Vec::new();
        }
        let found: Vec<FindMatch> = self
            .text
            .to_lowercase()
            .match_indices(&query.to_lowercase())
            .map(|(start, m)| FindMatch::new(start, start + m.len()))
            .collect();
        self.found = found.len();
        found
    }
    fn reveal(&mut self, index: usize) {
        self.current = Some(index);
    }
    fn clear_find(&mut self) {
        self.cleared = true;
        self.found = 0;
        self.current = None;
    }
}

impl Widget for Note {
    crate::provides!(dyn Findable);
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        use crate::widget::TaffyTreeExt;
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn name(&self) -> &'static str {
        "Note"
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn note(app: &mut App) -> &mut Note {
    fn find(widget: &mut dyn Widget) -> Option<&mut Note> {
        if widget
            .as_any_mut()
            .is_some_and(|any| any.downcast_mut::<Note>().is_some())
        {
            return widget.as_any_mut()?.downcast_mut::<Note>();
        }
        widget.children_mut().into_iter().find_map(find)
    }
    find(app.root_widget.as_mut()).expect("the app shows the note")
}

/// The shell's end of the app's connection: complete frames only.
struct ShellEnd {
    socket: UnixStream,
    pending: Vec<u8>,
}

impl ShellEnd {
    /// What the app sent since the last call.
    fn read(&mut self) -> Vec<FloatingSurfaceClientMessage> {
        let mut chunk = [0_u8; 4096];
        loop {
            match self.socket.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => self.pending.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("the app's connection broke: {error}"),
            }
        }
        let mut messages = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            if line.len() > 1 {
                messages.push(
                    serde_json::from_slice(&line[..line.len() - 1]).expect("a shell-ui frame"),
                );
            }
        }
        messages
    }

    fn send(&mut self, message: &FloatingSurfaceHostMessage) {
        let mut frame = serde_json::to_vec(message).unwrap();
        frame.push(b'\n');
        self.socket.write_all(&frame).unwrap();
    }
}

/// The desktop carries frames between the window and the app as the
/// shell's window-bar relay does: the app's offers to its tab, the window's
/// events back to the app.
fn relay(app: &mut App, shell: &mut ShellEnd, window: &mut WindowGroup) {
    for _ in 0..3 {
        for (tab, event) in window.take_toolbar_events() {
            assert_eq!(tab, 1, "events go to the tab the app shows");
            shell.send(&FloatingSurfaceHostMessage::ToolbarEvent { event });
        }
        app.pump_floating_surface_service();
        for message in shell.read() {
            match message {
                FloatingSurfaceClientMessage::WindowOffer { offer, .. } => {
                    window.set_tab_window_offer(1, Some(offer));
                }
                FloatingSurfaceClientMessage::OpenSearch { opening, .. } => {
                    window.open_search_for_app(1, opening);
                }
                _ => {}
            }
        }
    }
}

/// What the app last offered its window, read straight off the wire.
fn last_offer(shell: &mut ShellEnd, app: &mut App) -> Option<WindowOffer> {
    app.pump_floating_surface_service();
    shell
        .read()
        .into_iter()
        .filter_map(|message| match message {
            FloatingSurfaceClientMessage::WindowOffer { offer, .. } => Some(offer),
            _ => None,
        })
        .next_back()
}

#[test]
fn an_app_in_its_own_process_is_searched_and_its_history_opened_from_the_window_bar() {
    let (app_side, shell_side) = UnixStream::pair().unwrap();
    shell_side.set_nonblocking(true).unwrap();
    let mut shell = ShellEnd {
        socket: shell_side,
        pending: Vec::new(),
    };
    let mut app = super::super::framework_history_tests::app_with_root(
        crate::State::new(),
        Box::new(Note {
            text: "Cat cat concat cat".into(),
            ..Note::default()
        }),
    );
    app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());

    // The desktop's window shows the app's surface: nothing it can look
    // into, only what the app says over its connection.
    let mut window = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Notes"),
            TileContent::Stub {
                kind: AppKind::Editor,
                label: "Notes".into(),
            },
        )],
        1,
        WindowWidth::Normal,
    );
    window.set_focused(true);

    // The app says what its window offers: a document to search in place
    // and its History, closed.
    assert_eq!(
        last_offer(&mut shell, &mut app),
        Some(WindowOffer {
            search: SearchOffer::Document,
            count: None,
            history: Some(HistoryOffer::Closed),
            ..WindowOffer::default()
        })
    );
    assert!(
        app.floating.window_bar.presents(),
        "the bar presents the app's search and History, so it draws neither"
    );
    window.set_tab_window_offer(
        1,
        Some(WindowOffer {
            search: SearchOffer::Document,
            count: None,
            history: Some(HistoryOffer::Closed),
            ..WindowOffer::default()
        }),
    );

    // The app's Ctrl+F asks the bar for its search, and the bar's field
    // opens with the keyboard.
    crate::window_toolbar::open_search(crate::window_toolbar::SearchOpening::Find);
    relay(&mut app, &mut shell, &mut window);
    assert!(window.window_search().is_open());
    assert!(window.window_search().is_remote());
    assert!(window.window_search().has_keyboard());
    assert_eq!(window.window_search().in_place(), InPlace::Document);

    // What is typed in the bar highlights the app's note, and the bar shows
    // the count the app answered with.
    let mut typing = Dispatch::new();
    for key in ["c", "a", "t"] {
        assert!(
            typing.send(&mut window, &char_key(key)).handled,
            "typing goes to the bar's field"
        );
    }
    relay(&mut app, &mut shell, &mut window);
    assert_eq!(note(&mut app).found, 4, "the note highlights every match");
    assert_eq!(window.window_search().query(), "cat");
    assert_eq!(window.window_search().position(), Some((1, 4)), "1 of 4");

    // Enter steps to the next match in the app.
    typing.send(&mut window, &named_key(NamedKey::Enter));
    relay(&mut app, &mut shell, &mut window);
    assert_eq!(note(&mut app).current, Some(1));
    assert_eq!(window.window_search().position(), Some((2, 4)), "2 of 4");

    // Escape closes the search, and the app stops highlighting.
    typing.send(&mut window, &named_key(NamedKey::Escape));
    relay(&mut app, &mut shell, &mut window);
    assert!(!window.window_search().is_open());
    assert!(note(&mut app).cleared, "closing stops the highlights");

    // The window's History control opens the app's History panel, and reads
    // as on once the app says it is open.
    let mut frame = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        window,
        (900, 600),
        Theme::dark(),
    );
    assert_eq!(
        frame
            .root::<WindowGroup>()
            .bar_for_tests()
            .history_control(1),
        Some(HistoryControl::Closed),
        "the window's bar shows the app's History control"
    );
    frame
        .perform_semantic_named(
            "tabs",
            Some(crate::widget::tabs::WINDOW_HISTORY_TARGET),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    let window = frame.root_mut::<WindowGroup>();
    let events = window.take_toolbar_events();
    assert_eq!(events, vec![(1, ToolbarEvent::History)]);
    for (_, event) in events {
        shell.send(&FloatingSurfaceHostMessage::ToolbarEvent { event });
    }
    app.pump_floating_surface_service();
    // The app opens it on its next frame, as its runner draws one.
    super::super::framework_history_tests::layout(&mut app);
    assert_eq!(
        app.window_history_control(),
        Some(HistoryControl::Open),
        "the app's History panel is open"
    );
    let offer = last_offer(&mut shell, &mut app).expect("the app says History is open");
    assert_eq!(offer.history, Some(HistoryOffer::Open));
    window.set_tab_window_offer(1, Some(offer));
    frame.relayout();
    assert_eq!(
        frame
            .root::<WindowGroup>()
            .bar_for_tests()
            .history_control(1),
        Some(HistoryControl::Open)
    );
}

#[test]
fn an_apps_history_has_the_shell_describe_its_changes_and_keeps_the_answer_in_the_document() {
    use crate::floating_surface::HistoryChangeDescription;
    use automerge::transaction::Transactable as _;
    let (app_side, shell_side) = UnixStream::pair().unwrap();
    shell_side.set_nonblocking(true).unwrap();
    let mut shell = ShellEnd {
        socket: shell_side,
        pending: Vec::new(),
    };
    let mut state = crate::State::new();
    let body = state
        .doc
        .put_object(automerge::ROOT, "body", automerge::ObjType::Text)
        .unwrap();
    state.commit_action(None, None);
    state
        .doc
        .splice_text(&body, 0, 0, "Costs rose in March")
        .unwrap();
    state.commit_action(None, None);
    let mut app =
        super::super::framework_history_tests::app_with_root(state, Box::new(Note::default()));
    app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
    // The write is admitted at the policy's time, which a controlled test
    // app does not read from the machine.
    app.history
        .as_mut()
        .expect("the app registers History")
        .set_clock(|| Some(1_800_000_000));
    app.pump_floating_surface_service();
    let _ = shell.read();

    // History opens; the app asks the shell about its changes, with what
    // each wrote.
    shell.send(&FloatingSurfaceHostMessage::ToolbarEvent {
        event: ToolbarEvent::History,
    });
    app.pump_floating_surface_service();
    super::super::framework_history_tests::layout(&mut app);
    app.pump_floating_surface_service();
    let asked: Vec<_> = shell
        .read()
        .into_iter()
        .filter_map(|message| match message {
            FloatingSurfaceClientMessage::DescribeHistory { changes } => Some(changes),
            _ => None,
        })
        .flatten()
        .collect();
    let typed = asked
        .iter()
        .find(|change| change.wrote.contains("Costs rose in March"))
        .expect("the typing is asked about, with what it wrote")
        .clone();

    // The shell's answer reaches the document as one change.
    let changes = app.state.doc.get_changes(&[]).len();
    shell.send(&FloatingSurfaceHostMessage::HistoryDescriptions {
        described: vec![
            HistoryChangeDescription {
                change: typed.change.clone(),
                text: "Noted that costs rose in March".into(),
                generator: "local:test".into(),
            },
            HistoryChangeDescription {
                change: "00".repeat(16),
                text: "A change nobody asked about".into(),
                generator: "local:test".into(),
            },
        ],
    });
    app.pump_floating_surface_service();
    super::super::framework_history_tests::layout(&mut app);
    let written = crate::history_source::descriptions::read_descriptions(&app.state.doc);
    assert_eq!(
        written.values().map(String::as_str).collect::<Vec<_>>(),
        ["Noted that costs rose in March"],
        "only what was asked about is kept"
    );
    assert_eq!(
        app.state.doc.get_changes(&[]).len(),
        changes + 1,
        "one change carries the description"
    );

    // A described change is not asked about again.
    super::super::framework_history_tests::layout(&mut app);
    app.pump_floating_surface_service();
    assert!(shell.read().iter().all(|message| !matches!(
        message,
        FloatingSurfaceClientMessage::DescribeHistory { changes }
            if changes.iter().any(|change| change.change == typed.change)
    )));
}

/// An app with an address in its window's bar, as Browser has: it answers
/// what is typed there with suggestions and opens what the bar commits. It
/// reads the bar on its sync pass, as Browser does.
#[derive(Default)]
struct Omnibox {
    typed: String,
    opened: Vec<String>,
    rect: Rect,
}

impl Omnibox {
    fn declare(&self) {
        use crate::window_toolbar::{LocationSuggestion, ToolbarLocation, WindowToolbar};
        let suggestions = if self.typed.is_empty() {
            Vec::new()
        } else {
            vec![
                LocationSuggestion {
                    label: self.typed.clone(),
                    value: format!("https://{}/", self.typed),
                    icon: Some("globe".into()),
                    detail: String::new(),
                },
                LocationSuggestion {
                    label: self.typed.clone(),
                    value: format!("search:{}", self.typed),
                    icon: Some("search".into()),
                    detail: "DuckDuckGo".into(),
                },
            ]
        };
        crate::window_toolbar::declare(Some(WindowToolbar {
            location: Some(ToolbarLocation {
                label: "duckduckgo.com".into(),
                value: "https://duckduckgo.com/".into(),
                editable: true,
                suggestions,
                ..ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        }));
    }
}

impl Widget for Omnibox {
    fn sync(&mut self, _state: &crate::widget::AppModel, _path: &crate::widget::ModelPath) {
        for event in crate::window_toolbar::take_events() {
            match event {
                ToolbarEvent::Edit { text } => self.typed = text,
                ToolbarEvent::Commit { text } => {
                    self.typed.clear();
                    self.opened.push(text);
                }
                _ => {}
            }
        }
        self.declare();
    }
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        use crate::widget::TaffyTreeExt;
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn name(&self) -> &'static str {
        "Omnibox"
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn omnibox(app: &mut App) -> &mut Omnibox {
    fn find(widget: &mut dyn Widget) -> Option<&mut Omnibox> {
        if widget
            .as_any_mut()
            .is_some_and(|any| any.downcast_mut::<Omnibox>().is_some())
        {
            return widget.as_any_mut()?.downcast_mut::<Omnibox>();
        }
        widget.children_mut().into_iter().find_map(find)
    }
    find(app.root_widget.as_mut()).expect("the app shows its address")
}

/// One frame of the app as its native runner draws one after the shell
/// wrote to it: the connection pumped, the model updated and synced only as
/// the runner does it, then laid out and painted.
fn native_frame(app: &mut App) {
    if app.pump_floating_surface_service() {
        app.layout.layout_dirty = true;
    }
    app.update_native_model();
    app.sync_model(false);
    super::super::framework_history_tests::layout(app);
}

/// The desktop's relay for the window's bar, both ways: what was typed and
/// done in the bar to the app, the app's slots back to the window.
fn relay_bar(app: &mut App, shell: &mut ShellEnd, frame: &mut crate::headless::FrameHarness) {
    for _ in 0..3 {
        let window = frame.root_mut::<WindowGroup>();
        let mut events = window.take_toolbar_events();
        events.extend(window.take_location_drafts());
        for (tab, event) in events {
            assert_eq!(tab, 1, "events go to the tab the app shows");
            shell.send(&FloatingSurfaceHostMessage::ToolbarEvent { event });
        }
        native_frame(app);
        for message in shell.read() {
            if let FloatingSurfaceClientMessage::Toolbar { toolbar, .. } = message {
                frame.root_mut::<WindowGroup>().set_tab_toolbar(1, toolbar);
            }
        }
        frame.relayout();
    }
}

/// Where the window's bar painted `name` (a slot, a suggestion row).
fn bar_target(frame: &crate::headless::FrameHarness, name: &str) -> Option<Rect> {
    Widget::test_rects(frame.root::<WindowGroup>().bar_for_tests())
        .into_iter()
        .find(|(target, rect)| target == name && rect.area() > 0.0)
        .map(|(_, rect)| rect)
}

/// How many suggestions the window's bar lists.
fn bar_suggestions(frame: &crate::headless::FrameHarness) -> usize {
    Widget::test_rects(frame.root::<WindowGroup>().bar_for_tests())
        .into_iter()
        .filter(|(target, rect)| target.starts_with("window_suggestion:") && rect.area() > 0.0)
        .count()
}

/// An app with an address in its own process, and the desktop's window
/// whose bar edits it.
fn omnibox_window() -> (App, ShellEnd, crate::headless::FrameHarness) {
    let (app_side, shell_side) = UnixStream::pair().unwrap();
    shell_side.set_nonblocking(true).unwrap();
    let mut shell = ShellEnd {
        socket: shell_side,
        pending: Vec::new(),
    };
    let mut app = super::super::framework_history_tests::app_with_root(
        crate::State::new(),
        Box::new(Omnibox::default()),
    );
    app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
    let mut window = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Browser"),
            TileContent::Stub {
                kind: AppKind::Browser,
                label: "Browser".into(),
            },
        )],
        1,
        WindowWidth::Normal,
    );
    window.set_focused(true);
    let mut frame = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        window,
        (900, 600),
        Theme::dark(),
    );
    relay_bar(&mut app, &mut shell, &mut frame);
    assert!(
        bar_target(&frame, "window_location").is_some(),
        "the window's bar shows the app's address: declared {:?}, shown {:?}, targets {:?}",
        crate::window_toolbar::declared(),
        frame.root::<WindowGroup>().tab_toolbar(1),
        Widget::test_rects(frame.root::<WindowGroup>().bar_for_tests()),
    );
    (app, shell, frame)
}

/// Type `text` into the bar's address and let the app answer.
fn type_address(
    app: &mut App,
    shell: &mut ShellEnd,
    frame: &mut crate::headless::FrameHarness,
    text: &str,
) {
    let location = bar_target(frame, "window_location").expect("the address");
    frame.click_at(location.center().x, location.center().y);
    assert!(
        frame.root::<WindowGroup>().editing_location(),
        "a click on the address edits it"
    );
    for ch in text.chars() {
        let key = char_key(&ch.to_string());
        assert!(
            frame.dispatch_to_root(&key),
            "typing goes to the bar's address"
        );
    }
    relay_bar(app, shell, frame);
    assert_eq!(omnibox(app).typed, text, "the app hears what is typed");
}

#[test]
fn enter_in_the_bars_address_opens_what_was_typed_in_an_app_in_its_own_process() {
    let (mut app, mut shell, mut frame) = omnibox_window();
    type_address(&mut app, &mut shell, &mut frame, "youtube.com");
    assert_eq!(
        bar_suggestions(&frame),
        2,
        "the bar lists the app's suggestions under the address"
    );
    assert!(frame.dispatch_to_root(&named_key(NamedKey::Enter)));
    relay_bar(&mut app, &mut shell, &mut frame);
    assert_eq!(
        omnibox(&mut app).opened,
        ["youtube.com"],
        "Enter opens what was typed"
    );
    assert!(!frame.root::<WindowGroup>().editing_location());
}

#[test]
fn a_click_or_tap_on_a_bar_suggestion_opens_it_in_an_app_in_its_own_process() {
    for tap in [false, true] {
        let (mut app, mut shell, mut frame) = omnibox_window();
        type_address(&mut app, &mut shell, &mut frame, "youtube.com");
        let first =
            bar_target(&frame, "window_suggestion:0").expect("the first suggestion is on screen");
        let location = bar_target(&frame, "window_location").expect("the address");
        assert!(
            first.y0 >= location.y1 && (first.x0 - location.x0).abs() < 12.0,
            "the list hangs under the address, aligned to it: {first:?} under {location:?}"
        );
        if tap {
            frame.tap_at(first.center().x, first.center().y);
        } else {
            frame.click_at(first.center().x, first.center().y);
        }
        relay_bar(&mut app, &mut shell, &mut frame);
        assert_eq!(
            omnibox(&mut app).opened,
            ["https://youtube.com/"],
            "the chosen suggestion opens (tap: {tap})"
        );
    }
}
