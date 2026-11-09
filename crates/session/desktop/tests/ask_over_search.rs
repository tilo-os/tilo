//! The identity ask arriving by itself while the person is in the middle of something: the card
//! shows beside their avatar, but it never takes the keyboard from what they are doing. A search
//! the person opened keeps its keys — Enter takes the row under the highlight, Escape closes it —
//! until they move to the card themselves.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::{
    AvatarAsk, ChromeIntent, DesktopChromeSnapshot, DesktopChromeSource, OwnChip,
};
use tilo_desktop::desktop_host::{DesktopChrome, NoticeTiles, PermissiveRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use ui::headless::FrameHarness;

use ui::synthetic_input::{char_key, key_press};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn window(id: u64, title: &str, app: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

fn desk_tree() -> DesktopTree {
    let tabs = vec![window(20, "Notes", "so.tilo.notes")];
    let group = WindowGroup {
        id: 3,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// A host with an ask waiting on the person's avatar, which arrives when a test says so, and which
/// answers the bar's intents about it as a page does.
struct Asking {
    tree: DesktopTree,
    ask: RefCell<Option<AvatarAsk>>,
    intents: RefCell<Vec<ChromeIntent>>,
}

impl DesktopChromeSource for Asking {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        DesktopChromeSnapshot {
            own: Some(OwnChip {
                name: "Kim Lee".into(),
                rgb: (0xe0, 0x7a, 0x5f),
                identity: String::new(),
            }),
            ..DesktopChromeSnapshot::default()
        }
    }
    fn tree(&self) -> DesktopTree {
        self.tree.clone()
    }
    fn intent(&self, intent: ChromeIntent) -> bool {
        {
            let mut ask = self.ask.borrow_mut();
            match intent {
                ChromeIntent::OpenAvatarAsk | ChromeIntent::CollapseAvatarAsk => {
                    if let Some(ask) = ask.as_mut() {
                        ask.open = intent == ChromeIntent::OpenAvatarAsk;
                    }
                }
                ChromeIntent::AnswerAvatarAsk | ChromeIntent::PutOffAvatarAsk => *ask = None,
                _ => {}
            }
        }
        self.intents.borrow_mut().push(intent);
        true
    }
    fn avatar_ask(&self) -> Option<AvatarAsk> {
        self.ask.borrow().clone()
    }
}

/// The desk, and the chrome over it whose host offers the identity ask on demand.
struct Session {
    desk: FrameHarness,
    chrome: FrameHarness,
    handle: DesktopHandle,
    host: Rc<Asking>,
}

fn session(size: (u32, u32)) -> Session {
    ui::i18n::I18nManager::init("en-US");
    let tree = desk_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), size);
    desk.relayout();
    let host = Rc::new(Asking {
        tree: tree.clone(),
        ask: RefCell::new(None),
        intents: RefCell::default(),
    });
    let chrome_source: tilo_desktop::chrome::Chrome = host.clone();
    let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome_source);
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle.clone())),
        bar,
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    let mut chrome =
        FrameHarness::for_root(ui::state::State::new(), chrome, size, ui::Theme::dark());
    chrome.relayout();
    Session {
        desk,
        chrome,
        handle,
        host,
    }
}

impl Session {
    fn frame(&mut self) {
        self.desk.relayout();
        self.chrome.relayout();
    }

    fn search_chord(&mut self) {
        self.chrome
            .send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(
                ModifiersState::SUPER,
            )));
        self.chrome
            .send_host_event(&key_press(Key::Character("k".into()), None));
        self.chrome
            .send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(
                ModifiersState::empty(),
            )));
    }

    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.chrome
                .send_host_event(&char_key(&character.to_string()));
        }
    }

    /// Frames until every source of the open search has answered.
    fn settle_search(&mut self) {
        let background = ui::background::Background::current();
        let now = ui::time::now();
        let deadline = now.checked_add(Duration::from_secs(5)).unwrap_or(now);
        loop {
            self.frame();
            if !self.handle.search_expecting() {
                return;
            }
            assert!(ui::time::now() < deadline, "the search never finished");
            let now = ui::time::now();
            background.park(now.checked_add(Duration::from_millis(2)).unwrap_or(now));
        }
    }

    /// The ask arrives on its own, as an offer the page made while the person was busy does.
    fn ask_arrives(&mut self) {
        *self.host.ask.borrow_mut() = Some(AvatarAsk {
            question: "Keep your name and work?".into(),
            detail: "A passkey brings you back as yourself on this device.".into(),
            answer: "Keep with passkey".into(),
            open: true,
        });
        self.frame();
        self.frame();
    }
}

/// A search the person has open answers their Enter and their Escape even after the identity card
/// arrives unasked: the card shows, and the keyboard stays where the person was.
#[test]
fn an_ask_that_arrives_unasked_does_not_take_the_searchs_keys() {
    let mut session = session((1280, 800));
    session.frame();
    session.search_chord();
    session.frame();
    assert!(session.handle.search_open(), "the chord opened the palette");
    for _ in 0..3 {
        session.frame();
    }
    session.type_text("notes");
    session.settle_search();
    let (query, rows) = session.handle.search_rows();
    assert_eq!(query, "notes");
    assert!(!rows.is_empty(), "the desk's own window is a row");

    // The identity card opens on its own, over the desk, while the search is up.
    session.ask_arrives();
    assert!(
        session.host.ask.borrow().is_some(),
        "the card is up: {:?}",
        session.host.intents.borrow()
    );

    // The person's Escape still belongs to the search they opened: it clears what they typed
    // (search's own rule) rather than being swallowed by the card.
    session
        .chrome
        .send_host_event(&ui::synthetic_input::named_key(NamedKey::Escape));
    session.frame();
    assert!(
        !session
            .host
            .intents
            .borrow()
            .contains(&ChromeIntent::CollapseAvatarAsk),
        "Escape went to the search, not to the card: {:?}",
        session.host.intents.borrow()
    );
}

/// With rows under the highlight, the person's Enter takes the row, not the card's answer, after
/// the card arrived unasked.
#[test]
fn an_ask_that_arrives_unasked_does_not_answer_itself_on_the_persons_enter() {
    let mut session = session((1280, 800));
    session.frame();
    session.search_chord();
    session.frame();
    for _ in 0..3 {
        session.frame();
    }
    session.type_text("notes");
    session.settle_search();
    let (_, rows) = session.handle.search_rows();
    assert!(!rows.is_empty(), "the desk's own window is a row");

    session.ask_arrives();
    session
        .chrome
        .send_host_event(&ui::synthetic_input::named_key(NamedKey::Enter));
    session.frame();
    assert!(
        !session.host.intents.borrow().iter().any(|intent| matches!(
            intent,
            ChromeIntent::AnswerAvatarAsk | ChromeIntent::PutOffAvatarAsk
        )),
        "Enter took the row the person chose, not the card's answer: {:?}",
        session.host.intents.borrow()
    );
}

/// The card does take the keys once the person moves to it: pressing its answer answers it.
#[test]
fn the_card_answers_when_the_person_moves_to_it() {
    let mut session = session((1280, 800));
    session.frame();
    session.ask_arrives();
    // The card's answer is the pill's chip, pressed where it is drawn.
    let answer = session
        .chrome
        .a11y_node_named("Keep your name and work?: Keep with passkey");
    let answer = answer.expect("the bar publishes the ask's answer as a control");
    session
        .chrome
        .perform_a11y_action(answer, &ui::a11y::Action::Activate)
        .expect("the answer is pressed");
    assert!(
        session
            .host
            .intents
            .borrow()
            .contains(&ChromeIntent::AnswerAvatarAsk),
        "the answer reached the host: {:?}",
        session.host.intents.borrow()
    );
}

/// A card that arrives unasked never takes the keys of what the person is doing: the Escape they
/// type into the window in front stays theirs, and does not fold the card.
#[test]
fn an_ask_that_arrives_unasked_does_not_swallow_the_windows_escape() {
    let mut session = session((1280, 800));
    session.frame();
    // The person moved to the desk: the window in front has the keyboard, not the bar.
    session.ask_arrives();
    session
        .chrome
        .send_host_event(&ui::synthetic_input::named_key(NamedKey::Escape));
    session.frame();
    assert!(
        !session
            .host
            .intents
            .borrow()
            .contains(&ChromeIntent::CollapseAvatarAsk),
        "an unasked card answered an Escape the person typed into their window: {:?}",
        session.host.intents.borrow()
    );
}

/// Moving to the card — pressing the bar it hangs on — makes its Escape the person's: the card
/// folds then, because they are answering it.
#[test]
fn the_card_folds_on_escape_once_the_person_moved_to_the_bar() {
    let mut session = session((1280, 800));
    session.frame();
    session.ask_arrives();
    // A press on the bar is the person reading it: focus reaches it, as the compositor's does.
    let bar = session.chrome.root::<DesktopChrome>();
    let rect = ui::widget::Widget::get_rect(bar);
    let (x, y) = (rect.x0 + 4.0, rect.y0 + rect.height() / 2.0);
    session.chrome.click_at(x, y);
    session.frame();
    session
        .chrome
        .send_host_event(&ui::synthetic_input::named_key(NamedKey::Escape));
    session.frame();
    assert!(
        session
            .host
            .intents
            .borrow()
            .contains(&ChromeIntent::CollapseAvatarAsk),
        "the card the person moved to folds on Escape: {:?}",
        session.host.intents.borrow()
    );
}

/// Search the person closed and opened again starts them afresh: what they typed before is not
/// left in the field as plain text with the caret after it, where typing would append to a query
/// they no longer want.
#[test]
fn a_search_opened_again_does_not_keep_the_query_of_the_last_one() {
    let mut session = session((1280, 800));
    session.frame();
    session.search_chord();
    session.frame();
    for _ in 0..3 {
        session.frame();
    }
    session.type_text("notes");
    session.settle_search();
    assert_eq!(session.handle.search_rows().0, "notes");

    // Escape on a typed field clears it; the person closes the search with a second Escape.
    session
        .chrome
        .send_host_event(&ui::synthetic_input::named_key(NamedKey::Escape));
    session.frame();
    session
        .chrome
        .send_host_event(&ui::synthetic_input::named_key(NamedKey::Escape));
    session.frame();
    assert!(!session.handle.search_open(), "the search closed");

    session.search_chord();
    session.frame();
    for _ in 0..3 {
        session.frame();
    }
    assert!(session.handle.search_open(), "the chord opened it again");
    let (query, _) = session.handle.search_rows();
    assert_eq!(
        query, "",
        "the search reopened with the last one's text in its field: typing would append to it"
    );
}

/// Closing search by a press elsewhere and opening it again starts empty: the design sets an
/// empty search card (its groups named, the person's recents leading), so the last search's text
/// is not left as plain text with the caret after it, where typing would append to it.
#[test]
fn a_search_opened_again_after_a_press_away_starts_empty() {
    let mut session = session((1280, 800));
    session.frame();
    session.search_chord();
    session.frame();
    for _ in 0..3 {
        session.frame();
    }
    session.type_text("notes");
    session.settle_search();
    assert_eq!(session.handle.search_rows().0, "notes");

    // A press off the card closes search, as a person clicking their desk does.
    session.chrome.click_at(8.0, 780.0);
    session.frame();
    assert!(
        !session.handle.search_open(),
        "the press off the card closed search"
    );

    session.search_chord();
    session.frame();
    for _ in 0..3 {
        session.frame();
    }
    assert!(session.handle.search_open(), "the chord opened it again");
    assert_eq!(
        session.handle.search_rows().0,
        "",
        "the reopened field holds the last search's text: typing would append to it"
    );
    // The first thing typed is the query alone.
    session.type_text("s");
    session.settle_search();
    assert_eq!(session.handle.search_rows().0, "s");
}

/// Search closed with Escape on an empty field and opened again: the field starts empty, with the
/// caret in it, and the first thing typed is the query.
#[test]
fn a_search_closed_with_escape_opens_again_empty() {
    let mut session = session((1280, 800));
    session.frame();
    session.search_chord();
    session.frame();
    for _ in 0..3 {
        session.frame();
    }
    session.type_text("notes");
    session.settle_search();
    session
        .chrome
        .send_host_event(&ui::synthetic_input::named_key(NamedKey::Escape));
    session.frame();
    session
        .chrome
        .send_host_event(&ui::synthetic_input::named_key(NamedKey::Escape));
    session.frame();
    assert!(!session.handle.search_open());
    session.search_chord();
    session.frame();
    for _ in 0..3 {
        session.frame();
    }
    session.type_text("s");
    session.settle_search();
    assert_eq!(session.handle.search_rows().0, "s");
}

/// Choosing a row closes search; opening search again starts empty, so the first key typed is the
/// query rather than an addition to the search the person already took a row from.
#[test]
fn a_search_opened_again_after_a_choice_starts_empty() {
    let mut session = session((1280, 800));
    session.frame();
    session.search_chord();
    session.frame();
    for _ in 0..3 {
        session.frame();
    }
    session.type_text("notes");
    session.settle_search();
    assert_eq!(session.handle.search_rows().0, "notes");
    session
        .chrome
        .send_host_event(&ui::synthetic_input::named_key(NamedKey::Enter));
    session.frame();
    session.frame();
    assert!(
        !session.handle.search_open(),
        "Enter took the row and closed search"
    );

    session.search_chord();
    session.frame();
    for _ in 0..3 {
        session.frame();
    }
    assert!(session.handle.search_open(), "the chord opened it again");
    assert_eq!(
        session.handle.search_rows().0,
        "",
        "the reopened field holds the last search's text: typing would append to it"
    );
    session.type_text("c");
    session.settle_search();
    assert_eq!(session.handle.search_rows().0, "c");
}
