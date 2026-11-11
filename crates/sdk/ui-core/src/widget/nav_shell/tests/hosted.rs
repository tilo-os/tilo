use super::*;
use crate::widget::{NavFeed, NavItem, NavList, NavSection};
use crate::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar, declare, declared,
};

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
/// the window's bar (Passwords, Files) has no header row: the toggle stands
/// in the bar's corner, and Back beside it while there is somewhere to go
/// back to.
#[test]
fn a_narrow_stack_with_its_toggle_in_the_bar_draws_no_row_under_it() {
    const NARROW: u32 = 420;
    // The shell keeps the row's chevron beside the bar's toggle, where
    // Back goes somewhere; a top-level page has none to draw.
    let no_row = |h: &FrameHarness, at: &str, back_room: bool| {
        let g = geometry(h);
        assert_eq!(g.header_height, 0.0, "{at}: no header row");
        assert!(
            g.headers.iter().all(|header| *header == Rect::ZERO),
            "{at}: no header in any column"
        );
        assert_eq!(shows_back_part(h), back_room, "{at}: a Back of its own");
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
    no_row(&h, "the list", false);
    let list = child_rect(&h, "overview").expect("the list shows");
    assert!(list.y0.abs() < 1e-9, "the list starts right under the bar");

    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, NARROW, H);
    no_row(&h, "an open item", true);
    assert!(h.root::<NavShell>().can_go_back());
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
    no_row(&h, "the folder", false);
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
        .list()
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

/// A shell that hands its own header to the window's bar, showing the
/// place "Home".
fn place_shell(w: u32, height: u32) -> FrameHarness {
    let shell = NavShell::new()
        .with_id("places")
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Places", Vec::new())
        .content_header("Home", Vec::new());
    let mut h = harness(shell, w, height, Theme::dark());
    settle(&mut h, w, height);
    h.present_in_window_bar();
    settle(&mut h, w, height);
    h
}

/// The shell's slots in the window's bar name the place it shows. Another
/// part of the window may declare them for a while (a mode's commands, an
/// inspector's); when that part goes (the place changed to one without it)
/// the shell takes the slots back, so the bar names the place the shell
/// shows rather than the tab's own title, which still says where the
/// window opened.
#[test]
fn the_bar_names_the_place_again_once_another_parts_slots_go() {
    let other = |label: &str| WindowToolbar {
        location: Some(ToolbarLocation {
            label: label.to_owned(),
            ..ToolbarLocation::default()
        }),
        actions: vec![ToolbarAction::new(
            "place.share",
            "share",
            "Share this place",
        )],
        ..WindowToolbar::default()
    };
    let label = || {
        declared()
            .and_then(|toolbar| toolbar.location)
            .map(|location| location.label)
    };
    let mut h = place_shell(W, H);
    // A part of the window that is not the shell declares the slots: the
    // shell steps aside.
    declare(Some(other("Shared sessions")));
    settle(&mut h, W, H);
    assert_eq!(label().as_deref(), Some("Shared sessions"));
    // That part goes, and the place moves on in the same breath.
    declare(None);
    h.root_mut::<NavShell>()
        .set_content_header("History", Vec::new());
    settle(&mut h, W, H);
    assert_eq!(
        label().as_deref(),
        Some("History"),
        "the bar names the place the shell shows once the other part's slots go"
    );
    // The shell's own header actions are the bar's again.
    assert!(
        declared().is_some_and(|toolbar| toolbar.location.is_some()),
        "the shell declares the window's slots again"
    );
}

/// A shell that stepped aside for another part keeps the slots while that
/// part declares them, even as the place changes under it.
#[test]
fn another_parts_slots_stand_while_it_holds_them() {
    let mut h = place_shell(W, H);
    declare(Some(WindowToolbar {
        location: Some(ToolbarLocation {
            label: "Shared sessions".into(),
            ..ToolbarLocation::default()
        }),
        ..WindowToolbar::default()
    }));
    settle(&mut h, W, H);
    h.root_mut::<NavShell>()
        .set_content_header("History", Vec::new());
    settle(&mut h, W, H);
    assert_eq!(
        declared()
            .and_then(|toolbar| toolbar.location)
            .map(|location| location.label)
            .as_deref(),
        Some("Shared sessions"),
        "the other part's slots stand while it holds them"
    );
    declare(None);
    settle(&mut h, W, H);
}

/// The bar's Back is the window's, so it reaches the first screen too: from
/// a place it goes back to the app's navigation, whose screen names itself
/// in its own header row, so the bar says nothing of it twice.
#[test]
fn the_window_bars_back_returns_to_the_navigation_that_names_itself() {
    let label = || {
        declared()
            .and_then(|toolbar| toolbar.location)
            .map(|location| location.label)
    };
    let sidebar_header = |h: &FrameHarness| geometry(h).sidebar_header();
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Places", Vec::new())
        .content_header("Home", Vec::new());
    let mut h = harness(shell, 360, 800, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, 360, 800);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Sidebar,
        "a pushed window opens on its navigation"
    );
    assert!(
        sidebar_header(&h).height() > 0.0,
        "the navigation screen carries its own header row"
    );
    assert_eq!(label(), None, "the bar does not name the navigation again");

    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, 360, 800);
    assert_eq!(label().as_deref(), Some("Home"), "and follows to the place");

    h.press_window_slot(ToolbarEvent::Back);
    settle(&mut h, 360, 800);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Sidebar,
        "the bar's Back reaches the navigation screen"
    );
    assert!(
        sidebar_header(&h).height() > 0.0,
        "the navigation names itself again"
    );
    assert_eq!(label(), None, "the bar says nothing of it twice");
}

/// A sidebar's header row — the words that name its places and the buttons that add to them —
/// stands at the top of the sidebar at rest, with the window's bar hidden over the window's top
/// edge, as it does while the bar is out: it is the app's own navigation, never the bar's, so
/// nothing the sidebar offers waits on reaching for the bar.
#[test]
fn a_sidebars_header_stands_with_the_window_bar_hidden() {
    let shell = NavShell::new()
        .with_id("sidebar-header")
        .content_kind(ContentKind::Page)
        .sidebar_header(
            "Places",
            vec![Box::new(Button::icon_only(
                "places.add",
                Icon::Plus,
                "Add a place",
            ))],
        )
        .sidebar(Box::new(NavList::new(places()).with_id("places")))
        .content(Probe::boxed("content").0);
    let mut h = harness(shell, W, H, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, W, H);
    let bar_carries =
        declared().is_some_and(|toolbar| toolbar.location.is_some() || !toolbar.actions.is_empty());
    assert!(
        !bar_carries,
        "the bar carries neither the sidebar's title nor its buttons: {:?}",
        declared()
    );

    let shown = geometry(&h).sidebar_header();
    assert!(
        shown.height() > 0.0 && shown.width() > 0.0,
        "the sidebar's header row stands while the bar is out: {shown:?}"
    );
    h.present_window_bar_at_rest();
    settle(&mut h, W, H);
    let at_rest = geometry(&h).sidebar_header();
    assert_eq!(
        at_rest, shown,
        "the sidebar's header row stands where it was with the bar hidden"
    );
    let list = child_rect(&h, "places").expect("the places stand in the sidebar");
    assert!(
        list.y0 + 1e-9 >= at_rest.height(),
        "the places start under their header row: {list:?} beside {at_rest:?}"
    );
}

/// An app that declares no slots of its own gets them from its shell: while the
/// desktop's bar hosts the window, the column in front names the window's
/// location, the columns' headers' buttons are the bar's actions and run from
/// it, and the shell draws no header row under the bar. The sidebar's own
/// header is the app's navigation: it keeps its row, and its button, at the
/// top of the sidebar.
#[test]
fn a_hosted_shell_hands_its_columns_to_the_window_bar() {
    use crate::window_toolbar::{ToolbarEvent, declared};
    let pressed = SharedRc::new(Cell::new(0));
    let count = SharedRc::clone(&pressed);
    let add = Button::icon_only("mail.new-mailbox", Icon::Plus, "New mailbox")
        .on_effect(move || count.set(count.get() + 1));
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Mailboxes", vec![Box::new(add)])
        .overview_header("Inbox", Vec::new())
        .content_header(
            "Design review",
            vec![Box::new(crate::widget::Row::new().push(Box::new(
                Button::icon_only("mail.reply", Icon::Reply, "Reply"),
            )))],
        );
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    assert!(
        geometry(&h).header_height > 0.0,
        "with no desktop bar, the header row stands"
    );
    assert_eq!(declared(), None, "and the shell declares nothing");

    h.present_in_window_bar();
    settle(&mut h, 1280, 800);
    assert_eq!(
        geometry(&h).header_height,
        0.0,
        "no header row under the bar"
    );
    let toolbar = declared().expect("the shell declares the window's slots");
    assert_eq!(
        toolbar
            .location
            .as_ref()
            .map(|location| location.label.as_str()),
        Some("Design review"),
        "the content in front names the window"
    );
    assert_eq!(
        toolbar.sidebar,
        Some(true),
        "the sidebar's toggle is a slot"
    );
    assert!(
        geometry(&h).sidebar_header().height() > 0.0,
        "the sidebar keeps its own header row"
    );
    assert!(
        !toolbar
            .actions
            .iter()
            .any(|action| action.id == "mail.new-mailbox"),
        "the sidebar's button stays in its row, never in the bar"
    );
    let reply = toolbar
        .actions
        .iter()
        .find(|action| action.id == "mail.reply")
        .expect("the content's button is the bar's action");
    assert_eq!(reply.icon, "reply");
    assert!(!reply.menu, "a button with an icon stands in the row");
    // The bar's press is a press on the app: the frame that takes it runs
    // what it published, as the window's bar hands it over.
    h.press_window_slot(ToolbarEvent::Action {
        id: reply.id.clone(),
    });
    // The sidebar's own button stands in its row, where a person presses it,
    // and runs what the app published.
    let button = rect_named(&h, "mail.new-mailbox").expect("the sidebar's button shows in its row");
    assert!(
        button.y0 < geometry(&h).sidebar_header().y1,
        "the button stands in the sidebar's header row: {button:?}"
    );
    h.click_at(button.center().x, button.center().y);
    assert_eq!(pressed.get(), 1, "the sidebar's press runs the button");
}

/// A header button bound to an action, as framework apps build theirs, runs
/// when pressed in the window's bar, though the shell lays no header row out
/// under it. A column the shell does not have offers none of its buttons, as
/// its header row would show none, and the sidebar's own buttons stay in the
/// sidebar's row.
#[test]
fn a_header_action_runs_from_the_window_bar() {
    use crate::window_toolbar::{ToolbarEvent, declared};
    let pressed = SharedRc::new(Cell::new(0));
    let count = SharedRc::clone(&pressed);
    let add = Button::icon_only("mail.new-mailbox", Icon::Plus, "New mailbox")
        .on_own_device_effect(move || count.set(count.get() + 1));
    let compose = Button::icon_only("mail.compose", Icon::Plus, "Compose");
    let keep = Button::icon_only("mail.keep", Icon::Archive, "Keep");
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Mailboxes", vec![Box::new(keep)])
        .overview_header("Inbox", vec![Box::new(compose)])
        .content_header("Design review", vec![Box::new(add)]);
    let mut h = harness(shell, 1280, 800, Theme::dark());
    h.set_permission_ctx(crate::permissions::PermissionContext::member(
        "ada",
        crate::permissions::Role::Editor,
    ));
    h.present_in_window_bar();
    settle(&mut h, 1280, 800);
    let toolbar = declared().expect("the shell declares the window's slots");
    let ids: Vec<&str> = toolbar.actions.iter().map(|a| a.id.as_str()).collect();
    assert!(ids.contains(&"mail.new-mailbox"), "{ids:?}");
    assert!(
        !ids.contains(&"mail.compose"),
        "a column the shell does not have offers no buttons: {ids:?}"
    );
    assert!(
        !ids.contains(&"mail.keep"),
        "the sidebar's own buttons stay in its row: {ids:?}"
    );

    h.press_window_slot(ToolbarEvent::Action {
        id: "mail.new-mailbox".into(),
    });
    h.idle();
    assert!(
        h.control_failures().is_empty(),
        "{:?}",
        h.control_failures()
    );
    assert_eq!(h.refusal_said(), None);
    assert_eq!(pressed.get(), 1, "the bar's press runs the button's action");
}
