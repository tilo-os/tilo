//! The page's top bar, pressed as a person presses it: the desk and the chrome over it, with the
//! bar raising its requests into a queue the host carries out on its next pass, as the web page's
//! shell does. The workspace's name opens its menu and the person's face opens their own card,
//! whatever else stands up when they press: search over the windows, or the ask about keeping who
//! they are folded into the avatar's dot.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::cell::RefCell;
use std::rc::Rc;

use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::{ChromeIntent, OwnChip};
use tilo_desktop::desktop_host::{DesktopChrome, NoticeTiles};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::Standing;
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::host::chrome::{ChromeSource, Facts, HostChrome, Notices};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use ui::headless::FrameHarness;
use ui::kurbo::Point;
use ui::synthetic_input::{char_key, named_key};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

const PERSON: &str = "ada";

fn desk_tree() -> DesktopTree {
    let tabs = vec![Tab {
        id: 20,
        title: "Notes".into(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }];
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

/// What the page knows of its desk: its person, and the queue the bar raises its requests into.
struct Page {
    handle: DesktopHandle,
    authors: RefCell<Authors>,
    intents: RefCell<Vec<ChromeIntent>>,
}

impl ChromeSource for Page {
    fn handle(&self) -> &DesktopHandle {
        &self.handle
    }

    fn person(&self) -> &str {
        PERSON
    }

    fn with_standing<R>(&self, answer: impl FnOnce(&Standing<'_>) -> R) -> R {
        answer(&Standing {
            person: PERSON,
            role: None,
            members: None,
        })
    }

    fn with_authors<R>(&self, answer: impl FnOnce(&mut Authors) -> R) -> R {
        answer(&mut self.authors.borrow_mut())
    }

    fn own_chip(&self) -> OwnChip {
        OwnChip {
            name: "Ada Lovelace".into(),
            rgb: (0xe0, 0x7a, 0x5f),
            identity: String::new(),
        }
    }

    fn notices(&self) -> Notices {
        Notices::default()
    }

    fn facts(&self, _facts: &mut Facts) {}

    fn take_intent(&self, intent: ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        true
    }
}

/// The desk, the chrome over it, and what the page did with the bar's requests.
struct Shell {
    desk: FrameHarness,
    chrome: FrameHarness,
    page: Rc<Page>,
    /// The times the page was asked for the person's own card.
    cards: usize,
}

fn shell() -> Shell {
    let tree = desk_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    desk.relayout();
    let page = Rc::new(Page {
        handle: handle.clone(),
        authors: RefCell::default(),
        intents: RefCell::default(),
    });
    let source = Rc::new(HostChrome::new(Rc::clone(&page)));
    let bar = TopBarChrome::from_tree(&tree).with_chrome(source.clone());
    let chrome = DesktopChrome::new(
        handle,
        "g".into(),
        source,
        bar,
        TiloMenu::new(AboutVariant::Browser, &AboutLinks::default()),
        false,
    );
    let chrome = FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::Theme::light(),
    );
    let mut shell = Shell {
        desk,
        chrome,
        page,
        cards: 0,
    };
    shell.frame();
    shell
}

impl Shell {
    /// One pass of the page: what the bar asked for is carried out — the person's own card is the
    /// page's to show, the rest the chrome's — then the desk takes up what happened and the chrome
    /// draws over it.
    fn frame(&mut self) {
        let intents = std::mem::take(&mut *self.page.intents.borrow_mut());
        for intent in intents {
            if intent == ChromeIntent::ToggleProfile {
                self.cards = self.cards.saturating_add(1);
            } else {
                self.chrome.root_mut::<DesktopChrome>().apply_intent(intent);
            }
        }
        self.desk.relayout();
        self.chrome.relayout();
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame();
        }
    }

    /// A press and release of the primary button at `at`, as a person clicks.
    fn click(&mut self, at: Point) {
        self.chrome.click_at(at.x, at.y);
        self.frames(3);
    }

    fn workspace_name(&self) -> Point {
        self.chrome
            .root::<DesktopChrome>()
            .bar()
            .workspace_menu_rect()
            .center()
    }

    fn own_face(&self) -> Point {
        self.chrome
            .root::<DesktopChrome>()
            .bar()
            .own_avatar_rect()
            .center()
    }

    fn menu_open(&self) -> bool {
        self.chrome.root::<DesktopChrome>().menu_open()
    }

    fn open_search_and_type(&mut self, text: &str) {
        self.chrome
            .send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(
                ModifiersState::SUPER,
            )));
        self.chrome.send_host_event(&ui::synthetic_input::key_press(
            Key::Character("k".into()),
            None,
        ));
        self.chrome
            .send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(
                ModifiersState::empty(),
            )));
        self.frames(3);
        for character in text.chars() {
            self.chrome
                .send_host_event(&char_key(&character.to_string()));
        }
        self.frames(3);
    }

    /// The ask about keeping who they are, as the page's identity code puts it.
    fn ask_to_keep(&mut self) {
        let action: tilo_desktop_surface::HostAction = serde_json::from_value(serde_json::json!({
            "id": "save-recovery-key",
            "title": "Remember me",
            "source": "identity",
        }))
        .unwrap();
        self.page
            .handle
            .ask_personally(action, "Keep your name and work?".into(), String::new());
        self.frames(2);
    }
}

/// The workspace's name opens its menu, and the person's face their own card, on a desk with
/// nothing else up.
#[test]
fn the_bars_name_and_face_open_the_menu_and_the_card() {
    let mut shell = shell();
    let name = shell.workspace_name();
    shell.click(name);
    assert!(shell.menu_open(), "the workspace's name opens its menu");
    shell.chrome.send_host_event(&named_key(NamedKey::Escape));
    shell.frames(2);
    assert!(!shell.menu_open(), "Escape closes it");

    let face = shell.own_face();
    shell.click(face);
    assert_eq!(
        shell.cards, 1,
        "the face asks the page for the own card once"
    );
}

/// Search is up, the query typed and cleared by Escape: the bar stands clear of the search's
/// scrim and stays live, so a press on the workspace's name leaves search and opens the menu — one
/// press, not one to leave search and another to open what was pressed.
#[test]
fn a_press_on_the_bar_over_search_leaves_search_and_does_what_it_pressed() {
    let mut shell = shell();
    shell.open_search_and_type("workspace");
    assert!(shell.page.handle.search_open(), "search is up");
    shell.chrome.send_host_event(&named_key(NamedKey::Escape));
    shell.frames(3);
    assert!(
        shell.page.handle.search_open(),
        "Escape clears the typed query and leaves search up"
    );

    let name = shell.workspace_name();
    shell.click(name);
    assert!(!shell.page.handle.search_open(), "the press left search");
    assert!(shell.menu_open(), "and opened the workspace's menu");

    shell.chrome.send_host_event(&named_key(NamedKey::Escape));
    shell.frames(2);
    shell.open_search_and_type("notes");
    let face = shell.own_face();
    shell.click(face);
    assert!(
        !shell.page.handle.search_open(),
        "the face's press left search"
    );
    assert_eq!(shell.cards, 1, "and asked for the own card");
}

/// The ask about keeping who they are stands beside the face; a press elsewhere folds it into
/// the face's dot. The face opens the person's own card all the same — which offers what the ask
/// asks — open or folded, and a press on the workspace's name while it is open both folds it and
/// opens the menu.
#[test]
fn the_face_opens_the_own_card_whatever_the_ask_beside_it() {
    let mut shell = shell();
    shell.ask_to_keep();
    assert!(
        shell.page.handle.personal_ask().is_some_and(|ask| ask.open),
        "the ask stands open beside the face"
    );
    let name = shell.workspace_name();
    shell.click(name);
    assert!(shell.menu_open(), "the name's press opens the menu");
    assert!(
        shell
            .page
            .handle
            .personal_ask()
            .is_some_and(|ask| !ask.open),
        "and folds the ask into the dot"
    );
    shell.chrome.send_host_event(&named_key(NamedKey::Escape));
    shell.frames(2);

    let face = shell.own_face();
    shell.click(face);
    assert_eq!(shell.cards, 1, "the face opens the own card past the dot");
    assert!(
        shell
            .page
            .handle
            .personal_ask()
            .is_some_and(|ask| !ask.open),
        "the ask waits folded"
    );
}
