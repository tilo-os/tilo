//! An app in a process of its own, searched, its History opened and its address edited from
//! the window's bar over the real shell-ui connection: the app runs its own runtime on one end
//! of a socket, the shell on the other, and nothing passes between them but the protocol's
//! frames.

#![cfg(all(feature = "native", unix))]

use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::net::UnixStream;

use vello::kurbo::{Point, Rect};

use crate::floating_surface::{
    FloatingSurfaceClientMessage, FloatingSurfaceHostMessage, FloatingSurfaceServiceClient,
};
use crate::widget::HistoryControl;
use crate::widget::find_bar::{FindMatch, FindOptions, Findable};
use crate::window_toolbar::{HistoryOffer, SearchOffer, ToolbarEvent, WindowOffer};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;

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
    /// What the last find asked for, as replacing runs it again.
    query: String,
    options: FindOptions,
}

impl Findable for Note {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.query = query.to_owned();
        self.options = options;
        if query.is_empty() {
            self.found = 0;
            return Vec::new();
        }
        let haystack = if options.match_case {
            self.text.clone()
        } else {
            self.text.to_lowercase()
        };
        let needle = if options.match_case {
            query.to_owned()
        } else {
            query.to_lowercase()
        };
        let found: Vec<FindMatch> = haystack
            .match_indices(&needle)
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
    fn supports_replace(&self) -> bool {
        true
    }
    fn replace_all(&mut self, with: &str) -> usize {
        let found = self.find(&self.query.clone(), self.options);
        for found in found.iter().rev() {
            self.text.replace_range(found.start..found.end, with);
        }
        found.len()
    }
}

impl Widget for Note {
    tilo_ui_base::provides!(dyn Findable);
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        use tilo_ui_base::widget::TaffyTreeExt;
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
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

/// The shell hands the app what the person did in its window's bar, as a frame on its
/// connection, and the app answers it in its next pass.
fn tell(app: &mut App, shell: &mut ShellEnd, event: ToolbarEvent) {
    shell.send(&FloatingSurfaceHostMessage::ToolbarEvent {
        event,
        after_input: None,
    });
    app.pump_floating_surface_service();
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

    // The app says what its window offers: a document to search in place
    // and its History, closed.
    assert_eq!(
        last_offer(&mut shell, &mut app),
        Some(WindowOffer {
            search: SearchOffer::Document,
            count: None,
            history: Some(HistoryOffer::Closed),
            find: crate::widget::FindAbilities {
                replace: true,
                ..crate::widget::FindAbilities::default()
            },
            ..WindowOffer::default()
        })
    );
    assert!(
        app.floating.window_bar.presents(),
        "the bar presents the app's search and History, so it draws neither"
    );

    // The app's Ctrl+F asks the shell for its window's search.
    crate::window_toolbar::open_search(crate::window_toolbar::SearchOpening::Find);
    app.pump_floating_surface_service();
    assert!(
        shell
            .read()
            .iter()
            .any(|message| matches!(message, FloatingSurfaceClientMessage::OpenSearch { .. })),
        "the app asks for the bar's search"
    );

    // What is typed in the bar highlights the app's note, and the app answers with the count.
    tell(
        &mut app,
        &mut shell,
        ToolbarEvent::Search {
            query: "cat".into(),
        },
    );
    assert_eq!(note(&mut app).found, 4, "the note highlights every match");
    let offer = last_offer(&mut shell, &mut app).expect("the app answers the search");
    assert_eq!(
        offer.count,
        Some(crate::window_toolbar::SearchCount { shown: 1, total: 4 }),
        "1 of 4"
    );

    // Enter steps to the next match in the app.
    tell(
        &mut app,
        &mut shell,
        ToolbarEvent::SearchStep { forward: true },
    );
    assert_eq!(note(&mut app).current, Some(1));
    let offer = last_offer(&mut shell, &mut app).expect("the app answers the step");
    assert_eq!(
        offer.count,
        Some(crate::window_toolbar::SearchCount { shown: 2, total: 4 }),
        "2 of 4"
    );

    // Escape closes the search, and the app stops highlighting.
    tell(&mut app, &mut shell, ToolbarEvent::SearchClose);
    assert!(note(&mut app).cleared, "closing stops the highlights");

    // The window's History control opens the app's History panel, and the app says it is open.
    tell(&mut app, &mut shell, ToolbarEvent::History);
    // The app opens it on its next frame, as its runner draws one.
    super::super::framework_history_tests::layout(&mut app);
    assert_eq!(
        app.window_history_control(),
        Some(HistoryControl::Open),
        "the app's History panel is open"
    );
    let offer = last_offer(&mut shell, &mut app).expect("the app says History is open");
    assert_eq!(offer.history, Some(HistoryOffer::Open));
}

/// The bar's options and replacement are the app's own search: matching case narrows what it
/// finds, and replacing every match is one change of the document, which the app says in its
/// offer.
#[test]
fn the_bar_options_and_replacement_are_the_apps_search() {
    use crate::window_toolbar::SearchCount;

    let mut app = super::super::framework_history_tests::app_with_root(
        crate::State::new(),
        Box::new(Note {
            text: "Cat cat concat cat".into(),
            ..Note::default()
        }),
    );
    let count = |app: &mut App| app.window_offer().count;
    app.receive_bar_event(
        ToolbarEvent::Search {
            query: "cat".into(),
        },
        None,
    );
    assert_eq!(
        count(&mut app),
        Some(SearchCount { shown: 1, total: 4 }),
        "any case finds all four"
    );
    assert!(app.window_offer().find.replace, "the note replaces");
    app.receive_bar_event(
        ToolbarEvent::SearchOptions {
            options: FindOptions {
                match_case: true,
                ..FindOptions::default()
            },
        },
        None,
    );
    assert_eq!(
        count(&mut app),
        Some(SearchCount { shown: 1, total: 3 }),
        "matching case leaves the three that are lowercase"
    );
    assert!(app.window_offer().options.match_case, "the bar reads it");
    app.receive_bar_event(
        ToolbarEvent::SearchReplace {
            with: "dog".into(),
            all: true,
        },
        None,
    );
    app.take_window_offer_change();
    assert_eq!(note(&mut app).text, "Cat dog condog dog");
    assert_eq!(
        count(&mut app),
        Some(SearchCount { shown: 0, total: 0 }),
        "nothing matches any more"
    );
    app.receive_bar_event(ToolbarEvent::SearchClose, None);
    assert_eq!(count(&mut app), None);
    assert_eq!(
        app.window_offer().options,
        FindOptions::default(),
        "a closed find runs as it opens"
    );
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
    fn sync(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) {
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
        use tilo_ui_base::widget::TaffyTreeExt;
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
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

/// An app with an address in its own process, on one end of its shell-ui connection.
fn omnibox_app() -> (App, ShellEnd) {
    let (app_side, shell_side) = UnixStream::pair().unwrap();
    shell_side.set_nonblocking(true).unwrap();
    let shell = ShellEnd {
        socket: shell_side,
        pending: Vec::new(),
    };
    let mut app = super::super::framework_history_tests::app_with_root(
        crate::State::new(),
        Box::new(Omnibox::default()),
    );
    app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
    (app, shell)
}

/// Hand the app `event` from its window's bar, and read back the parts it declares in answer.
fn bar_event(
    app: &mut App,
    shell: &mut ShellEnd,
    event: ToolbarEvent,
) -> Option<crate::window_toolbar::WindowToolbar> {
    shell.send(&FloatingSurfaceHostMessage::ToolbarEvent {
        event,
        after_input: None,
    });
    // The app answers on its next frame, and what it declared there reaches the connection on
    // the frame after.
    let mut declared = None;
    for _ in 0..3 {
        native_frame(app);
        for message in shell.read() {
            if let FloatingSurfaceClientMessage::Toolbar { toolbar, .. } = message {
                declared = toolbar;
            }
        }
    }
    declared
}

/// What the bar's address offers under what was typed, as the app last declared it.
fn suggestions(toolbar: Option<&crate::window_toolbar::WindowToolbar>) -> Vec<String> {
    toolbar
        .and_then(|toolbar| toolbar.location.as_ref())
        .map(|location| {
            location
                .suggestions
                .iter()
                .map(|suggestion| suggestion.value.clone())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn enter_in_the_bars_address_opens_what_was_typed_in_an_app_in_its_own_process() {
    let (mut app, mut shell) = omnibox_app();
    let declared = bar_event(
        &mut app,
        &mut shell,
        ToolbarEvent::Edit {
            text: "youtube.com".into(),
        },
    );
    assert_eq!(
        omnibox(&mut app).typed,
        "youtube.com",
        "the app hears what is typed"
    );
    assert_eq!(
        suggestions(declared.as_ref()),
        ["https://youtube.com/", "search:youtube.com"],
        "the app answers what is typed with suggestions for the bar to list"
    );
    let declared = bar_event(
        &mut app,
        &mut shell,
        ToolbarEvent::Commit {
            text: "youtube.com".into(),
        },
    );
    assert_eq!(
        omnibox(&mut app).opened,
        ["youtube.com"],
        "Enter opens what was typed"
    );
    assert!(
        suggestions(declared.as_ref()).is_empty(),
        "an opened address lists nothing under it"
    );
}

#[test]
fn a_chosen_bar_suggestion_opens_it_in_an_app_in_its_own_process() {
    let (mut app, mut shell) = omnibox_app();
    let declared = bar_event(
        &mut app,
        &mut shell,
        ToolbarEvent::Edit {
            text: "youtube.com".into(),
        },
    );
    let first = suggestions(declared.as_ref())
        .into_iter()
        .next()
        .expect("the app suggests an address");
    bar_event(&mut app, &mut shell, ToolbarEvent::Commit { text: first });
    assert_eq!(
        omnibox(&mut app).opened,
        ["https://youtube.com/"],
        "the chosen suggestion opens"
    );
}
