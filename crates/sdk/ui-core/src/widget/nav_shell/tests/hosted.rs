use super::*;
use crate::widget::{NavFeed, NavItem, NavList, NavSection};
use crate::window_toolbar::ToolbarEvent;

fn places() -> Vec<NavSection> {
    let item = |id: &str, label: &str, icon: Icon| NavItem::new(id, label).icon(icon);
    vec![NavSection::untitled(
        "places",
        vec![
            item("home", "Home", Icon::Home),
            item("trending", "Trending", Icon::Flame),
        ],
    )]
}

const W: u32 = 1400;
const H: u32 = 800;

fn hosted() -> (FrameHarness, NavFeed, WindowBarLink) {
    let feed = NavFeed::new();
    let shell = NavShell::new()
        .with_id("hosted")
        .with_window_toolbar()
        .content_kind(ContentKind::Grid);
    let link = shell.window_bar_link();
    let shell = shell
        .sidebar(Box::new(
            NavList::new(Vec::new()).with_id("places").fed_by(&feed),
        ))
        .content(Probe::boxed("content").0);
    feed.show(places(), Some("home".into()));
    let mut h = harness(shell, W, H, Theme::dark());
    settle(&mut h, W, H);
    (h, feed, link)
}

/// The app shows its places through the feed; a row the person chooses
/// comes back to the app as that place's id.
#[test]
fn the_feed_shows_the_apps_places_and_hands_back_the_persons_choice() {
    let (mut h, feed, _) = hosted();
    let list = h
        .root::<NavShell>()
        .sidebar_ref::<NavList>()
        .expect("the sidebar is the list");
    assert_eq!(list.selected_id().as_deref(), Some("home"));
    let row = list.row_rect("trending").expect("the app's place is a row");
    h.click_at(row.center().x, row.center().y);
    assert_eq!(feed.take_chosen(), vec!["trending".to_owned()]);
    assert!(feed.take_chosen().is_empty(), "each choice comes back once");
}

/// The shell's own buttons are the app's slots in the window's bar: the
/// sidebar's toggle shows where the sidebar stands, and pressing it in the
/// bar hides and shows the sidebar. What is not the shell's stays the
/// app's.
#[test]
fn the_link_puts_the_shells_toggle_in_the_apps_window_bar() {
    let (mut h, _, link) = hosted();
    assert_eq!(link.slots().sidebar, Some(true), "docked and shown");
    assert!(link.event(&ToolbarEvent::ToggleSidebar));
    settle(&mut h, W, H);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert_eq!(link.slots().sidebar, Some(false));
    assert!(!link.is_sidebar_open());
    assert!(!link.event(&ToolbarEvent::Action { id: "app".into() }));
    assert!(link.event(&ToolbarEvent::ToggleSidebar));
    settle(&mut h, W, H);
    assert!(h.root::<NavShell>().is_sidebar_open());
}

/// A narrow window whose toolbar is the window bar's slots opens on its
/// list, as one with a header row does: the list fills the window, and Back
/// from an opened item comes back to it.
#[test]
fn a_narrow_window_with_its_toolbar_in_the_bar_opens_on_its_list() {
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .with_window_toolbar();
    let mut h = harness(shell, 560, 800, Theme::dark());
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    let list = child_rect(&h, "overview").expect("the list is a column");
    assert!(
        list.width() >= 500.0 && list.height() > 0.0,
        "the list fills the window: {list:?}"
    );
    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, 560, 800);
    assert!(h.root_mut::<NavShell>().back());
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    let list = child_rect(&h, "overview").expect("the list is a column");
    assert!(list.width() >= 500.0, "back on the list: {list:?}");
}

/// Whether the shell shows a Back part of its own.
fn shows_back_part(h: &FrameHarness) -> bool {
    h.root::<NavShell>()
        .part(Part::Back)
        .is_some_and(|back| back.get_rect().width() > 0.0)
}

/// Whether the shell's slots for the window's bar hold Back.
fn bar_back(h: &FrameHarness) -> bool {
    h.root::<NavShell>()
        .window_toolbar()
        .leading
        .iter()
        .any(|action| action.id == crate::widget::nav_shell::window_toolbar::WINDOW_BACK)
}

/// A phone-wide (420 px) window whose bar holds the sidebar's toggle opens
/// on its top-level page with nowhere to go back to: the toggle reaches the
/// sidebar, and no Back stands beside it, in the bar or in the shell.
#[test]
fn a_top_level_page_has_no_back_beside_the_bars_toggle() {
    const NARROW: u32 = 420;
    // Containers, Clocks: a sidebar of sections and the section's page.
    let shell = NavShell::new()
        .content_kind(ContentKind::List)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .with_window_toolbar();
    let mut h = harness(shell, NARROW, H, Theme::dark());
    settle(&mut h, NARROW, H);
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Screen);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
    assert_eq!(g.back_column, None, "Back goes nowhere");
    assert_eq!(g.back, Rect::ZERO);
    assert!(!shows_back_part(&h), "no Back part");
    assert!(!h.root::<NavShell>().can_go_back());
    assert!(!bar_back(&h), "no Back in the bar");
    let toolbar = h.root::<NavShell>().window_toolbar();
    assert_eq!(toolbar.sidebar, Some(false), "the toggle is the bar's");
    assert!(
        h.root_mut::<NavShell>()
            .window_toolbar_event(&ToolbarEvent::ToggleSidebar)
    );
    settle(&mut h, NARROW, H);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Sidebar,
        "the toggle reaches the sidebar"
    );
    assert!(!bar_back(&h), "the sidebar is a first screen too");
}

/// With a list beside its content, the list is the top-level page: no Back
/// on it, and Back in the bar once an item is open, which comes back to the
/// list.
#[test]
fn back_shows_once_there_is_somewhere_to_go_back_to() {
    const NARROW: u32 = 420;
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .with_window_toolbar();
    let mut h = harness(shell, NARROW, H, Theme::dark());
    settle(&mut h, NARROW, H);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    assert!(!bar_back(&h), "the list is the top-level page");
    assert!(!shows_back_part(&h));

    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, NARROW, H);
    assert!(bar_back(&h), "an open item goes back to the list");
    assert!(!shows_back_part(&h), "Back is the bar's alone");
    assert!(
        h.root_mut::<NavShell>()
            .window_toolbar_event(&ToolbarEvent::Action {
                id: crate::widget::nav_shell::window_toolbar::WINDOW_BACK.into()
            })
    );
    settle(&mut h, NARROW, H);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    assert!(!bar_back(&h));
}

/// A phone-wide (420 px) push stack whose app keeps the sidebar's toggle in
/// the window's bar (Passwords, Files) has its Back there too: no header row
/// stands under the bar to hold a lone Back chevron, on the list or on an
/// open item.
#[test]
fn a_narrow_stack_with_its_toggle_in_the_bar_draws_no_row_under_it() {
    const NARROW: u32 = 420;
    let no_row = |h: &FrameHarness, at: &str| {
        let g = geometry(h);
        assert_eq!(g.header_height, 0.0, "{at}: no header row");
        assert!(
            g.headers.iter().all(|header| *header == Rect::ZERO),
            "{at}: no header in any column"
        );
        assert_eq!(g.back, Rect::ZERO, "{at}: no Back chip");
        assert!(!shows_back_part(h), "{at}: no Back part");
    };
    // Passwords: places, the list, the item.
    let mut shell = NavShell::new()
        .content_kind(ContentKind::Page)
        .with_header_in_window_bar()
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0);
    shell.set_sidebar_toggle_in_window_bar(true);
    let mut h = harness(shell, NARROW, H, Theme::dark());
    settle(&mut h, NARROW, H);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    no_row(&h, "the list");
    let list = child_rect(&h, "overview").expect("the list shows");
    assert!(list.y0.abs() < 1e-9, "the list starts right under the bar");

    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, NARROW, H);
    no_row(&h, "an open item");
    assert!(h.root::<NavShell>().can_go_back());
    assert!(bar_back(&h), "Back is the bar's");
    let item = child_rect(&h, "content").expect("the item shows");
    assert!(item.y0.abs() < 1e-9, "the item starts right under the bar");
    assert!(h.root_mut::<NavShell>().back());
    settle(&mut h, NARROW, H);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);

    // Files: places and the folder.
    let mut shell = NavShell::new()
        .content_kind(ContentKind::List)
        .with_header_in_window_bar()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .content_header("", Vec::new());
    shell.set_sidebar_toggle_in_window_bar(true);
    let mut h = harness(shell, NARROW, H, Theme::dark());
    settle(&mut h, NARROW, H);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
    no_row(&h, "the folder");
    assert!(!bar_back(&h), "the folder is the top-level page");
}

/// Content that shows the feed a new place whenever it settles, as an app
/// shows what it took up.
struct SettleShower {
    feed: NavFeed,
    rect: Rect,
}

impl Widget for SettleShower {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn settle(&mut self, _edits: &mut crate::widget::Edits<'_>, _path: &automerge::ObjId) {
        let mut places = places();
        if let Some(section) = places.first_mut() {
            section.items.push(NavItem::new("later", "Later"));
        }
        self.feed.show(places, Some("home".into()));
    }

    fn name(&self) -> &'static str {
        "SettleShower"
    }
}

/// A narrow window on its list still settles the content it put away, so
/// what the content takes up shows in the list on screen.
#[test]
fn a_narrow_window_on_its_list_still_settles_the_content_behind_it() {
    let feed = NavFeed::new();
    let shell = NavShell::new()
        .with_window_toolbar()
        .content_kind(ContentKind::Reading)
        .sidebar(Box::new(
            NavList::new(Vec::new()).with_id("places").fed_by(&feed),
        ))
        .content(Box::new(SettleShower {
            feed: feed.clone(),
            rect: Rect::ZERO,
        }));
    feed.show(places(), Some("home".into()));
    let mut h = harness(shell, 480, 800, Theme::dark());
    settle(&mut h, 480, 800);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Screen);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    let list = h.root::<NavShell>().sidebar_ref::<NavList>();
    assert!(list.and_then(|list| list.row_rect("later")).is_some());
}

/// A choice the app does not take (a call it joins, not a place it opens)
/// is undone by the app's next show, even one that changes nothing.
#[test]
fn a_choice_the_app_does_not_take_is_undone_by_its_next_show() {
    let (mut h, feed, _) = hosted();
    let selected = |h: &FrameHarness| {
        h.root::<NavShell>()
            .sidebar_ref::<NavList>()
            .and_then(NavList::selected_id)
    };
    let row = h
        .root::<NavShell>()
        .sidebar_ref::<NavList>()
        .and_then(|list| list.row_rect("trending"))
        .expect("the row");
    h.click_at(row.center().x, row.center().y);
    assert_eq!(feed.take_chosen(), vec!["trending".to_owned()]);
    assert_eq!(selected(&h).as_deref(), Some("trending"));
    feed.show(places(), Some("home".into()));
    settle(&mut h, W, H);
    assert_eq!(selected(&h).as_deref(), Some("home"));
}

/// Content that asks the list, through the feed, to rename `id` when it is
/// pressed: the Rename of a menu the app shows.
struct RenameAsker {
    feed: NavFeed,
    id: &'static str,
    rect: Rect,
}

impl Widget for RenameAsker {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            }
        ) {
            self.feed.begin_rename(self.id, ctx.focus_target);
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "RenameAsker"
    }
}

/// A section the app lets the person rename and ask a menu of: a right
/// click on its header comes back as its menu, and the app's Rename opens
/// the field on its title with the keyboard there, the name coming back
/// through the feed.
#[test]
fn a_sections_menu_and_rename_come_back_through_the_feed() {
    let feed = NavFeed::new();
    let shell = NavShell::new()
        .with_id("hosted")
        .with_window_toolbar()
        .content_kind(ContentKind::Grid)
        .sidebar(Box::new(
            NavList::new(Vec::new())
                .with_id("places")
                .fed_by(&feed)
                .fed_menus(&feed)
                .fed_renames(&feed),
        ))
        .content(Box::new(RenameAsker {
            feed: feed.clone(),
            id: "projects",
            rect: Rect::ZERO,
        }));
    let mut sections = places();
    sections.push(
        NavSection::new(
            "projects",
            "Projects",
            vec![NavItem::new("garden", "Garden")],
        )
        .collapsible()
        .editable_title()
        .with_menu(),
    );
    feed.show(sections, Some("home".into()));
    let mut h = harness(shell, W, H, Theme::dark());
    settle(&mut h, W, H);
    let header = h
        .root::<NavShell>()
        .sidebar_ref::<NavList>()
        .and_then(|list| list.row_rect("projects"))
        .expect("the header is in view");
    h.send(&crate::synthetic_input::cursor_moved(
        header.center().x,
        header.center().y,
    ));
    for state in [
        winit::event::ElementState::Pressed,
        winit::event::ElementState::Released,
    ] {
        h.send(&crate::synthetic_input::mouse_input(
            state,
            winit::event::MouseButton::Right,
        ));
    }
    let menus = feed.take_menus();
    assert_eq!(menus.len(), 1, "the header asked for its menu: {menus:?}");
    assert_eq!(menus[0].0, "projects");
    assert!(
        header.contains(menus[0].1.center()),
        "its lift is the header"
    );
    // The app's Rename, from its own content.
    h.click_at(f64::from(W) - 40.0, f64::from(H) / 2.0);
    settle(&mut h, W, H);
    let list = h
        .root::<NavShell>()
        .sidebar_ref::<NavList>()
        .expect("the sidebar is the list");
    let field = list
        .tree()
        .rename_field()
        .expect("the title's field is open");
    assert_eq!(field.value(), "Projects");
    assert!(header.contains(field.get_rect().center()), "over the title");
    h.key("End");
    for _ in "Projects".chars() {
        h.key("Backspace");
    }
    h.type_text("Archive");
    h.key("Enter");
    assert_eq!(
        feed.take_renames(),
        vec![("projects".to_owned(), "Archive".to_owned())]
    );
}
