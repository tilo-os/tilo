//! The search palette over the windows: the chrome draws it, above whatever
//! windows stand in front, takes the keys while it is up and stands where the
//! search says it does — over the whole display, or over the window it was
//! opened from.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::rc::Rc;

use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::desktop_host::{DesktopChrome, DesktopRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, SearchHost, SearchHostFeed, TileSource};
use ui::headless::FrameHarness;
use ui::search::Scope;
use ui::synthetic_input::{char_key, key_press, named_key};
use ui::widget::TileHost;
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// The windows of the test desk, with their content as plain notices.
struct Notices;

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

struct Rules {
    handle: DesktopHandle,
}

impl DesktopRules for Rules {
    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        "Ada".into()
    }

    fn may(&self, _: &DesktopCommand) -> Result<(), String> {
        Ok(())
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        Ok(())
    }
}

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

/// A desk and the chrome that stands over it, as the session runs the two.
struct Session {
    desk: FrameHarness,
    chrome: FrameHarness,
    handle: DesktopHandle,
}

/// The desk and the chrome over it, with `host` answering for what lies
/// beyond the desk, as the session's own search host does.
fn session(tree: &DesktopTree, size: (u32, u32), host: Option<Rc<dyn SearchHost>>) -> Session {
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(Notices));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), size);
    desk.relayout();
    if let Some(host) = host {
        handle.set_search_host(host);
    }
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(Rules {
            handle: handle.clone(),
        }),
        TopBarChrome::from_tree(tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    let chrome = FrameHarness::for_root(ui::state::State::new(), chrome, size, ui::Theme::dark());
    Session {
        desk,
        chrome,
        handle,
    }
}

impl Session {
    /// One frame of both surfaces, in the order the compositor draws them:
    /// the desk takes up what happened, then the chrome draws over it.
    fn frame(&mut self) {
        self.desk.relayout();
        self.chrome.relayout();
    }

    /// The search chord, as a person presses it with a window in front.
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

    fn press(&mut self, key: NamedKey) {
        self.chrome.send_host_event(&named_key(key));
    }

    /// The card the palette stands in, as the chrome drew it, when the
    /// palette is up.
    fn card(&self) -> Option<ui::kurbo::Rect> {
        let search = self.handle.search_palette();
        let search = search.borrow();
        search.palette().map(ui::Widget::get_rect)
    }
}

/// What the person types goes to the open palette, not to the window behind
/// it, and Enter is the palette's too: it takes the row and leaves search.
#[test]
fn typing_and_enter_reach_the_palette_not_the_window() {
    let tree = desk_tree();
    let mut session = session(&tree, (1280, 800), None);
    session.frame();
    assert!(!session.handle.search_open());

    session.search_chord();
    session.frame();
    assert!(session.handle.search_open(), "the chord opened the palette");
    session.frame();

    for _ in 0..3 {
        session.frame();
    }
    // The window in front was never asked: it holds no field that would take
    // these keys, and search holds the keyboard instead.
    session.type_text("notes");
    session.frame();
    let (query, rows) = session.handle.search_rows();
    assert_eq!(query, "notes", "what was typed is the palette's query");
    assert!(!rows.is_empty(), "the desk's own window is a row");

    session.press(NamedKey::Enter);
    session.frame();
    assert!(
        !session.handle.search_open(),
        "Enter took the row in the palette and search closed"
    );
}

/// The card stands where the shell's own stood: three fifths of the display's
/// width, a little below its top, and as many rows tall as the display gives
/// it.
#[test]
fn the_card_follows_the_display() {
    let mut heights = Vec::new();
    for (width, height) in [(1000, 700), (1280, 800), (2000, 1200)] {
        let tree = desk_tree();
        let mut session = session(&tree, (width, height), Some(Rc::new(Many)));
        session.frame();
        session
            .handle
            .open_search(ui::search::Scope::Workspace, None);
        // The desk opens it, then the chrome lays the card out on the pace
        // it has: the frame after that draws it.
        session.frame();
        session.frame();
        let card = session.card().expect("the palette is up");
        let (width, height) = (f64::from(width), f64::from(height));
        let expected = (width * 0.6).clamp(360.0, 640.0);
        assert!(
            (card.width() - expected).abs() < 0.5,
            "a {width}x{height} display gives the card {card:?}, not {expected} wide"
        );
        assert!(
            (card.center().x - width / 2.0).abs() < 0.5,
            "the card {card:?} is centred across the display"
        );
        assert!(
            (card.y0 - (height * 0.12).max(24.0)).abs() < 0.5,
            "the card {card:?} stands a little below the display's top"
        );
        assert!(
            card.y1 <= height,
            "the card {card:?} stays on a {height} tall display"
        );
        heights.push(card.height());
    }
    // A taller display gives the card a longer list: the card takes the room
    // it has rather than a fixed number of rows.
    assert!(
        heights[2] > heights[0],
        "the card grows with the display: {heights:?}"
    );
    assert!(
        heights[2] > 500.0,
        "a 1200 tall display shows a long list: {heights:?}"
    );
}

/// A host whose search answers with more rows than any card holds, so the
/// card's own room decides how many it shows.
struct Many;

/// The rows [`Many`] answers with: more than fit on any display.
struct Plenty;

impl tilo_desktop_search::Source for Plenty {
    fn id(&self) -> &'static str {
        "plenty"
    }

    fn name(&self) -> String {
        "Plenty".into()
    }

    fn query(&self, _: &str) -> Vec<tilo_desktop_search::Hit> {
        (0..40)
            .map(|i| tilo_desktop_search::Hit {
                faces: Vec::new(),
                outcome: None,
                kind: tilo_desktop_search::ResourceKind::Note,
                provider: "plenty".into(),
                id: format!("note-{i}"),
                title: format!("Note {i}"),
                subtitle: None,
                facts: Vec::new(),
                score: ui::search::system::MAX_PROVIDER_SCORE,
                icon: None,
                row: None,
                preview: None,
                target: tilo_desktop_search::Target::Window { tab: 0, app: None },
            })
            .collect()
    }
}

impl SearchHost for Many {
    fn feed(&self, _: &str, _: &ui::search::Scope) -> SearchHostFeed {
        SearchHostFeed {
            sources: vec![std::sync::Arc::new(Plenty)],
            ..SearchHostFeed::default()
        }
    }

    fn performs(&self, _: &tilo_desktop_search::Target) -> bool {
        false
    }
}

/// A host whose search has a place of its own in every workspace: one in
/// the workspace in front, and one another workspace names.
#[derive(Default)]
struct Everywhere {
    /// The scope search asked it to cover, in order.
    scopes: std::cell::RefCell<Vec<Scope>>,
}

/// The place another workspace names, offered only when search covers every
/// workspace, and saying which one it is in.
struct Elsewhere {
    every_workspace: bool,
}

impl tilo_desktop_search::Source for Elsewhere {
    fn id(&self) -> &'static str {
        "continue"
    }

    fn name(&self) -> String {
        "Continue".into()
    }

    fn query(&self, _: &str) -> Vec<tilo_desktop_search::Hit> {
        if !self.every_workspace {
            return Vec::new();
        }
        vec![tilo_desktop_search::Hit {
            faces: Vec::new(),
            outcome: None,
            kind: tilo_desktop_search::ResourceKind::Note,
            provider: self.id().into(),
            id: "quarterly-plan".into(),
            title: "Quarterly plan".into(),
            subtitle: Some("in Planning".into()),
            facts: Vec::new(),
            score: ui::search::system::MAX_PROVIDER_SCORE,
            icon: None,
            row: None,
            preview: None,
            target: tilo_desktop_search::Target::Window { tab: 0, app: None },
        }]
    }
}

impl SearchHost for Everywhere {
    fn feed(&self, _: &str, scope: &Scope) -> SearchHostFeed {
        self.scopes.borrow_mut().push(scope.clone());
        SearchHostFeed {
            sources: vec![std::sync::Arc::new(Elsewhere {
                every_workspace: scope.everywhere(),
            })],
            ..SearchHostFeed::default()
        }
    }

    fn performs(&self, _: &tilo_desktop_search::Target) -> bool {
        false
    }
}

/// What a person searches is the workspace in front; the field says so and
/// takes it away from the start of the line, and the search then offers what
/// every workspace holds, saying which workspace a row is in. The key is the
/// palette's, routed there by the chrome over the window.
#[test]
fn a_search_of_the_workspace_in_front_widens_to_every_workspace() {
    let tree = desk_tree();
    let host = Rc::new(Everywhere::default());
    let mut session = session(&tree, (1280, 800), Some(host.clone()));
    session.frame();
    session.search_chord();
    session.frame();
    assert!(session.handle.search_open());
    for _ in 0..3 {
        session.frame();
    }
    assert_eq!(
        *host.scopes.borrow(),
        [ui::search::Scope::Workspace],
        "the search opens over the workspace in front"
    );
    let (_, rows) = session.handle.search_rows();
    assert!(
        rows.iter().all(|row| row.title != "Quarterly plan"),
        "another workspace's rows wait: {rows:?}"
    );

    session.press(NamedKey::Backspace);
    for _ in 0..3 {
        session.frame();
    }
    assert_eq!(
        *host.scopes.borrow(),
        [ui::search::Scope::Workspace, ui::search::Scope::Everywhere],
        "the same search, over every workspace"
    );
    let (_, rows) = session.handle.search_rows();
    assert!(
        rows.iter()
            .any(|row| row.title == "Quarterly plan"
                && row.subtitle.as_deref() == Some("in Planning")),
        "the row says which workspace it is in: {rows:?}"
    );
}
