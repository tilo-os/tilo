//! The search palette over the windows: the chrome draws it, above whatever windows stand in front,
//! takes the keys while it is up and stands where the search says it does — over the whole display,
//! or over the window it was opened from.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::rc::Rc;
use std::time::Duration;

use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::desktop_host::{DesktopChrome, NoticeTiles, PermissiveRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, SearchHost, SearchHostFeed};
use ui::headless::FrameHarness;
use ui::search::Scope;
use ui::synthetic_input::{char_key, key_press, named_key};
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

/// A desk and the chrome that stands over it, as the session runs the two.
struct Session {
    desk: FrameHarness,
    chrome: FrameHarness,
    handle: DesktopHandle,
}

/// The desk and the chrome over it, with `host` answering for what lies beyond the desk, as the
/// session's own search host does.
fn session(tree: &DesktopTree, size: (u32, u32), host: Option<Rc<dyn SearchHost>>) -> Session {
    session_parts(tree, size, host, false)
}

/// [`session`], with the chrome recording the damage parts a host that keeps its frame compares
/// (the web page's shell tracks them).
fn session_parts(
    tree: &DesktopTree,
    size: (u32, u32),
    host: Option<Rc<dyn SearchHost>>,
    track_parts: bool,
) -> Session {
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), size);
    desk.relayout();
    if let Some(host) = host {
        handle.set_search_host(host);
    }
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle.clone())),
        TopBarChrome::from_tree(tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    let mut chrome =
        FrameHarness::for_root(ui::state::State::new(), chrome, size, ui::Theme::dark());
    chrome.track_scene_parts(track_parts);
    if track_parts {
        // The frame the target starts from is drawn here, so the first frame a test presents is
        // compared with it.
        chrome.relayout();
    }
    Session {
        desk,
        chrome,
        handle,
    }
}

impl Session {
    /// One frame of both surfaces, in the order the compositor draws them: the desk takes up what
    /// happened, then the chrome draws over it.
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

    /// Frames until every source of the open search has answered: a source answers on a worker, and
    /// its answer wakes the desk, so a test pumps what the wake asks for and draws the frame it
    /// brings.
    fn settle_search(&mut self) {
        let background = ui::background::Background::current();
        let now = ui::time::now();
        let deadline = now.checked_add(Duration::from_secs(5)).unwrap_or(now);
        loop {
            self.frame();
            // A source still out does not make the search "still searching"
            // once its ready rows are shown, so wait on the sources.
            if !self.handle.search_expecting() {
                return;
            }
            assert!(ui::time::now() < deadline, "the search never finished");
            let now = ui::time::now();
            background.park(now.checked_add(Duration::from_millis(2)).unwrap_or(now));
        }
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

    /// The card the palette stands in, as the chrome drew it, when the palette is up.
    fn card(&self) -> Option<ui::kurbo::Rect> {
        let search = self.handle.search_palette();
        let search = search.borrow();
        search.palette().map(ui::Widget::get_rect)
    }
}

/// What the person types goes to the open palette, not to the window behind it, and Enter is the
/// palette's too: it takes the row and leaves search.
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
    // The window in front was never asked: it holds no field that would take these keys, and search
    // holds the keyboard instead.
    session.type_text("notes");
    session.settle_search();
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

/// A row a reader hears is a row the person can take: activating it by name reaches the card as the
/// click on it does, and search closes.
#[test]
fn an_assistive_activation_takes_the_row_it_names() {
    let tree = desk_tree();
    let mut session = session(&tree, (1280, 800), None);
    session.frame();
    session.search_chord();
    session.frame();
    assert!(session.handle.search_open(), "the chord opened the palette");
    for _ in 0..3 {
        session.frame();
    }
    session.type_text("notes");
    session.settle_search();
    let (_, rows) = session.handle.search_rows();
    assert!(!rows.is_empty(), "the desk's own window is a row");

    let row = session
        .chrome
        .a11y_node_named(&rows[0].title)
        .expect("the row is read by its title");
    session
        .chrome
        .perform_a11y_action(row, &ui::a11y::Action::Activate)
        .expect("the reader takes the row");
    session.frame();
    assert!(
        !session.handle.search_open(),
        "the row was taken and search closed, as it does for the person's click"
    );
}

/// The card stands where the shell's own stood: three fifths of the display's width, a little below
/// its top, and as many rows tall as the display gives it.
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
        // The desk opens it, then the chrome lays the card out on the pace it has: the frame after
        // that draws it.
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
    // A taller display gives the card a longer list: the card takes the room it has rather than a
    // fixed number of rows.
    assert!(
        heights[2] > heights[0],
        "the card grows with the display: {heights:?}"
    );
    assert!(
        heights[2] > 500.0,
        "a 1200 tall display shows a long list: {heights:?}"
    );
}

/// A host whose search answers with more rows than any card holds, so the card's own room decides
/// how many it shows.
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
            sources: vec![tilo_desktop_search::Held::new(Plenty)],
            ..SearchHostFeed::default()
        }
    }

    fn performs(&self, _: &tilo_desktop_search::Target) -> bool {
        false
    }
}

/// A host whose search has a place of its own in every workspace: one in the workspace in front,
/// and one another workspace names.
#[derive(Default)]
struct Everywhere {
    /// The scope search asked it to cover, in order.
    scopes: std::cell::RefCell<Vec<Scope>>,
}

/// The place another workspace names, offered only when search covers every workspace, and saying
/// which one it is in.
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
            sources: vec![tilo_desktop_search::Held::new(Elsewhere {
                every_workspace: scope.everywhere(),
            })],
            ..SearchHostFeed::default()
        }
    }

    fn performs(&self, _: &tilo_desktop_search::Target) -> bool {
        false
    }
}

/// What a person searches is the workspace in front; the field says so and takes it away from the
/// start of the line, and the search then offers what every workspace holds, saying which workspace
/// a row is in. The key is the palette's, routed there by the chrome over the window.
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
    session.settle_search();
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

/// The chrome draws what a retained target keeps, in the parts a host compares it by, and says what
/// of it the next frame repaints.
struct Presented {
    frames: ui::retained_raster::RetainedFrames,
    key: ui::retained_raster::TargetKey,
}

impl Presented {
    fn new(session: &Session, (width, height): (u32, u32)) -> Self {
        Self {
            frames: ui::retained_raster::RetainedFrames::default(),
            key: ui::retained_raster::TargetKey {
                width,
                height,
                to_device: ui::kurbo::Affine::IDENTITY,
                base_color: session.chrome.frame_render_input().background,
                generation: 0,
            },
        }
    }

    fn next(&mut self, session: &mut Session) -> ui::retained_raster::Repaint {
        session.frame();
        let scene = session.chrome.frame_render_input().scene;
        let parts = session.chrome.scene_parts();
        let repaint = self.frames.repaint(scene, parts, self.key);
        self.frames.presented(scene, parts, self.key);
        repaint
    }
}

/// The scrim search dims the desktop with covers every window, so a host that keeps its frame must
/// repaint it when search closes as it did when search opened: a close that repaints nothing leaves
/// the dim on screen.
#[test]
fn closing_search_repaints_the_whole_scrim() {
    use ui::retained_raster::Repaint;
    let tree = desk_tree();
    let size = (1280, 800);
    let mut session = session_parts(&tree, size, None, true);
    let mut shown = Presented::new(&session, size);
    assert_eq!(
        shown.next(&mut session),
        Repaint::Full(ui::retained_raster::Unknown::NoFrame),
        "nothing was presented before"
    );

    session.handle.open_search(Scope::Workspace, None);
    shown.next(&mut session);
    for _ in 0..3 {
        shown.next(&mut session);
    }
    // The chrome draws search over the whole area it stands on, so the dim covers the desktop.
    let scrim = ui::kurbo::Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1));

    session.handle.toggle_search();
    let repaint = shown.next(&mut session);
    let repainted: f64 = match repaint {
        Repaint::Full(_) => scrim.area(),
        Repaint::Regions(ref regions) => regions.iter().map(ui::kurbo::Rect::area).sum(),
    };
    assert!(
        repainted >= scrim.area() * 0.99,
        "closing search repainted {repainted} of the {scrim:?} it dimmed: {repaint:?}"
    );
}

/// Typing into the open palette damages the card, not the display under it: the palette's own
/// paint is one bounded part of the chrome's scene, so a key (and the caret it moves) repaints the
/// card and the scrim stays as it was. A palette whose commands fall outside every part cannot be
/// proven sparse, and then every frame of a search repaints the whole dimmed display — on a page
/// drawing in software that is seconds a frame, and the bar the person typed into answers after
/// seconds instead of at the pace of a key.
#[test]
fn typing_in_the_palette_repaints_the_card_not_the_display() {
    use ui::retained_raster::Repaint;
    let tree = desk_tree();
    let mut session = session_parts(&tree, (1280, 800), Some(Rc::new(Many)), true);
    session.frame();
    session.handle.open_search(Scope::Workspace, None);
    session.frame();
    session.frame();
    let card = session.card().expect("the palette is up");
    let painted = ui::widget::floating_surface::painted_bounds(card, &ui::Theme::dark());
    let mut shown = Presented::new(&session, (1280, 800));
    // The frame that opens search draws the scrim and the card: everything changes.
    shown.next(&mut session);
    for _ in 0..2 {
        shown.next(&mut session);
    }
    // No key typed yet: the settled card damages only what its own caret or a result changes,
    // never the display it dims.
    session.type_text("n");
    let repaint = shown.next(&mut session);
    let regions = match repaint {
        Repaint::Regions(regions) => regions,
        Repaint::Full(unknown) => panic!(
            "a key in the open palette repaints the whole display ({unknown:?}), not the card \
             it was typed into, {painted:?}"
        ),
    };
    for region in &regions {
        assert!(
            painted
                .union(painted.inflate(2.0, 2.0))
                .contains_rect(*region),
            "{region:?} is beyond the card's paint {painted:?}: a keystroke in the palette \
             repaints the page around it"
        );
    }
}

/// A search the desk is still waiting on asks for the frames that finish it, from the chrome that
/// draws it. The palette's own rows and an application's answer arrive off the frame (a worker's
/// answer wakes the desk; a provider's answer comes on the host's clock), and a desk with nothing
/// else to draw presents none — so the answer is looked for on a deadline, the same one the desk
/// keeps for its own pass. Without it the palette shows the rows it had when the person stopped
/// typing, and Enter on an app's row comes long after the person pressed it.
#[test]
fn a_search_still_filling_in_asks_for_frames_before_the_bar_ticks() {
    let tree = desk_tree();
    let mut session = session_parts(&tree, (1280, 800), Some(Rc::new(Many)), true);
    session.frame();
    session.search_chord();
    session.frame();
    assert!(session.handle.search_open(), "the chord opened the palette");
    session.type_text("note");
    // No frame drawn since the query: the search still owes its rows.
    let now = ui::time::now();
    assert!(
        session.handle.search_searching(),
        "the search is still filling in"
    );
    let wake = session
        .chrome
        .frame_schedule()
        .next_wake_deadline
        .expect("a search still filling in asks to be looked at again");
    assert!(
        wake <= now.checked_add(ui::time::Duration::from_secs(1)).unwrap(),
        "the wake is at {wake:?}, not within a second of {now:?}: the page would wait for the \
         bar's next minute to finish the search"
    );
}

/// A desk whose shown strip holds `n` windows, all at rest.
fn desk_tree_with(n: u64) -> DesktopTree {
    let groups: Vec<WindowGroup> = (0..n)
        .map(|i| {
            let tab = window(
                i.saturating_add(20),
                &format!("Window {i}"),
                "so.tilo.notes",
            );
            WindowGroup {
                id: i.saturating_add(3),
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }
        })
        .collect();
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: groups,
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

/// A keystroke in the open palette must not rebuild the page's composition, however many windows
/// are open. The page is one composition — the desk with its windows under the chrome that draws
/// the card over them, as the browser's shell stands the two in one tree. The card a person types
/// into is a leaf whose rows are painted, not widgets mounted into that tree: a row that came or
/// went is the card's paint, and the card's own size is re-solved in place. A frame that discarded
/// the composition instead would re-encode and re-measure every window's whole scene for a change
/// none of them holds — the cost growing with every window open, so a key typed into the palette is
/// answered later the more the person has open, seconds late on a page drawing in software.
///
/// The measure is the frame's own accounting (`RelayoutStats::rebuilt`): a rebuild discards the
/// taffy tree and re-solves every widget, a frame that keeps it re-solves only what it marked.
#[test]
fn a_keystroke_in_the_palette_does_not_rebuild_the_page_with_many_windows() {
    use std::cell::RefCell;
    use ui::widget::Stack;

    for windows in [1u64, 6, 12, 24] {
        let tree = desk_tree_with(windows);
        let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
        handle.set_search_host(Rc::new(Many));
        let chrome = DesktopChrome::new(
            handle.clone(),
            "g".into(),
            Rc::new(PermissiveRules::new(handle.clone())),
            TopBarChrome::from_tree(&tree),
            TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
            false,
        );
        // The page's own composition: the desk under the chrome over it, one tree, so a rebuild of
        // either discards the whole.
        let root = Stack::new().push(Box::new(surface)).push(Box::new(chrome));
        let mut page = FrameHarness::for_root(
            ui::state::State::new(),
            root,
            (1280, 800),
            ui::Theme::dark(),
        );
        page.track_scene_parts(true);
        page.relayout();
        assert!(!handle.search_open());
        handle.open_search(Scope::Workspace, None);
        {
            let background = ui::background::Background::current();
            let start = ui::time::now();
            let deadline = start.checked_add(Duration::from_secs(5)).unwrap_or(start);
            loop {
                page.relayout();
                if !handle.search_expecting() {
                    break;
                }
                assert!(ui::time::now() < deadline, "the search never finished");
                let now = ui::time::now();
                background.park(now.checked_add(Duration::from_millis(2)).unwrap_or(now));
            }
        }
        assert!(handle.search_open(), "the palette is up");
        // A frame at rest with the card's rows in: this one laid the card out.
        page.relayout();
        // One key into the card: its rows change, and nothing a window holds does. Every frame
        // from the key on is drawn, and none may rebuild: the card's rows are its own paint and
        // size, and a rebuild re-encodes every open window's scene for a change none of them holds.
        page.send_host_event(&char_key("x"));
        {
            let background = ui::background::Background::current();
            let start = ui::time::now();
            let deadline = start.checked_add(Duration::from_secs(5)).unwrap_or(start);
            let mut drawn = 0;
            loop {
                page.relayout();
                drawn += 1;
                assert!(
                    !page.last_relayout().rebuilt,
                    "frame {drawn} after a keystroke in the open palette rebuilt the page's \
                     composition with {windows} windows open: every window's scene is measured and \
                     drawn again for a change in the card alone"
                );
                if !handle.search_expecting() && drawn > 2 {
                    break;
                }
                assert!(ui::time::now() < deadline, "the search never finished");
                let now = ui::time::now();
                background.park(now.checked_add(Duration::from_millis(2)).unwrap_or(now));
            }
        }
        let _ = RefCell::new(());
    }
}
