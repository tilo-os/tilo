use super::*;
use tilo_ui_app::widget::{NavFeed, NavItem, NavList, NavSection};
use tilo_ui_app::window_toolbar::ToolbarEvent;

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

fn hosted() -> (Driver, NavFeed, WindowBarLink) {
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
        .content(TestLeaf::fill("content").boxed().0);
    feed.show(places(), Some("home".into()));
    let mut h = harness(shell, W, H, Theme::dark());
    settle(&mut h, W, H);
    (h, feed, link)
}

tilo_test_env::scenario! {
    fn a_hosted_shell_shares_its_places_and_toggle_with_the_window_bar;

    // A narrow window whose toolbar is the window bar's parts opens on its
    // list, as one with a header row does: the list fills the window, and Back
    // from an opened item comes back to it.
    phase a_narrow_window_with_its_toolbar_in_the_bar_opens_on_its_list;
    {
        let shell = NavShell::new()
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .overview(TestLeaf::fill("overview").boxed().0)
            .content(TestLeaf::fill("content").boxed().0)
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

    // A phone-wide (420 px) window whose bar holds the sidebar's toggle opens on its top-level
    // page with nowhere to go back to: the toggle reaches the sidebar, and no Back stands beside
    // it, in the bar or in the shell.
    phase a_top_level_page_has_no_back_beside_the_bars_toggle;
    {
        const NARROW: u32 = 420;
        let shell = NavShell::new()
            .content_kind(ContentKind::List)
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .content(TestLeaf::fill("content").boxed().0)
            .with_window_toolbar();
        let mut h = harness(shell, NARROW, H, Theme::dark());
        settle(&mut h, NARROW, H);
        let g = geometry(&h);
        assert_eq!(g.sidebar, SidebarPlacement::Screen);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
        assert_eq!(g.test_back_column(), None, "Back goes nowhere");
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
        assert!(
            bar_back(&h),
            "the places' screen offers the way back to the place it covered"
        );
        assert_eq!(
            h.root::<NavShell>().window_toolbar().sidebar,
            Some(true),
            "and the toggle puts the places away"
        );
    }

    // With a list beside its content, the list is the top-level page: no Back
    // on it, and Back in the bar once an item is open, which comes back to the
    // list.
    phase back_shows_once_there_is_somewhere_to_go_back_to;
    {
        const NARROW: u32 = 420;
        let shell = NavShell::new()
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .overview(TestLeaf::fill("overview").boxed().0)
            .content(TestLeaf::fill("content").boxed().0)
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
                    id: WINDOW_BACK.into()
                })
        );
        settle(&mut h, NARROW, H);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
        assert!(!bar_back(&h));
    }



    // The app shows its places through the feed; a row the person chooses
    // comes back to the app as that place's id.
    phase the_feed_shows_the_apps_places_and_hands_back_the_persons_choice;
    {
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
}

/// The shell's own buttons are the app's parts in the window's bar: the sidebar's toggle shows
/// where the sidebar stands, and pressing it in the bar hides and shows the sidebar.
#[test]
fn the_link_puts_the_shells_toggle_in_the_apps_window_bar() {
    let (mut h, _, link) = hosted();
    assert_eq!(link.parts().sidebar, Some(true), "docked and shown");
    assert!(link.event(&ToolbarEvent::ToggleSidebar));
    settle(&mut h, W, H);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert_eq!(link.parts().sidebar, Some(false));
    assert!(!link.is_sidebar_open());
    assert!(!link.event(&ToolbarEvent::Action { id: "app".into() }));
    assert!(link.event(&ToolbarEvent::ToggleSidebar));
    settle(&mut h, W, H);
    assert!(h.root::<NavShell>().is_sidebar_open());
}

fn shows_back_part(h: &Driver) -> bool {
    h.root::<NavShell>()
        .test_part(Part::Back)
        .is_some_and(|back| back.get_rect().width() > 0.0)
}

fn bar_back(h: &Driver) -> bool {
    h.root::<NavShell>()
        .window_toolbar()
        .leading
        .iter()
        .any(|action| action.id == WINDOW_BACK)
}

/// A choice the app does not take (a call it joins, not a place it opens)
/// is undone by the app's next show, even one that changes nothing.
#[test]
fn a_choice_the_app_does_not_take_is_undone_by_its_next_show() {
    let (mut h, feed, _) = hosted();
    let selected = |h: &Driver| {
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

    tilo_ui_base::rect_geometry!();

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_scene, _transform, _text, _theme, mut env) = ctx.parts();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
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
    h.send(&tilo_ui_base::synthetic_input::cursor_moved(
        header.center().x,
        header.center().y,
    ));
    for state in [
        winit::event::ElementState::Pressed,
        winit::event::ElementState::Released,
    ] {
        h.send(&tilo_ui_base::synthetic_input::mouse_input(
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
    h.click_at(f64::from(W) - 40.0, f64::from(H) / 2.0);
    settle(&mut h, W, H);
    let list = h
        .root::<NavShell>()
        .sidebar_ref::<NavList>()
        .expect("the sidebar is the list");
    let field = list
        .test_list()
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

/// A notice the app asks the window's bar to show says what an operation came to. It is no press
/// on the sidebar's toggle: the sidebar stands as it stood, and nothing is kept as the person's
/// choice for it, so it still stands after the window was narrow and is wide again.
#[test]
fn a_notice_asked_through_the_bar_leaves_the_sidebar_as_it_is() {
    let shell = NavShell::new()
        .with_window_toolbar()
        .content_kind(ContentKind::Reading);
    let link = shell.window_bar_link();
    let shell = shell
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0);
    let mut h = harness(shell, 1100, 760, Theme::dark());
    settle(&mut h, 1100, 760);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Docked);
    assert!(
        h.root::<NavShell>().is_sidebar_open(),
        "a wide window opens with its sidebar docked and open"
    );

    link.toast("Saved", tilo_ui_app::widget::toast::ToastKind::Success);
    settle(&mut h, 1100, 760);
    assert!(
        h.root::<NavShell>().is_sidebar_open(),
        "a notice is no press on the sidebar's toggle"
    );
    assert!(
        child_rect(&h, "sidebar").is_some_and(|r| r.width() > 0.0),
        "the sidebar stands where it stood"
    );

    settle(&mut h, 360, 760);
    settle(&mut h, 1100, 760);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Docked);
    assert!(
        h.root::<NavShell>().is_sidebar_open(),
        "nothing was kept as the person's choice for the wide window"
    );
}

/// An app that shows one thing alone (the view a message carries, mounted in that message's
/// place) has nothing of its window around it: at a width where the sidebar docks, the shell
/// places none and the bar offers no toggle for one. Taken back, the sidebar stands as the
/// person left it.
#[test]
fn a_window_that_shows_one_thing_alone_places_no_sidebar_at_any_width() {
    let shell = NavShell::new()
        .with_window_toolbar()
        .content_kind(ContentKind::Reading);
    let link = shell.window_bar_link();
    let shell = shell
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0);
    let mut h = harness(shell, 1100, 760, Theme::dark());
    settle(&mut h, 1100, 760);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Docked);
    assert_eq!(link.parts().sidebar, Some(true), "docked, with its toggle");

    link.show_content_alone(true);
    settle(&mut h, 1100, 760);
    assert_eq!(
        geometry(&h).sidebar,
        SidebarPlacement::None,
        "the one thing has no sidebar beside it"
    );
    assert_eq!(
        link.parts().sidebar,
        None,
        "no toggle for a sidebar it does not show"
    );
    assert!(child_rect(&h, "content").is_some_and(|r| r.width() >= 1099.0));
    assert!(
        child_rect(&h, "sidebar").is_none_or(|r| r.area() <= 0.0),
        "nothing of the window's places beside the one thing"
    );

    link.show_content_alone(false);
    settle(&mut h, 1100, 760);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Docked);
    assert_eq!(
        link.parts().sidebar,
        Some(true),
        "the sidebar stands as the person left it"
    );
}

/// Hosted in a window frame, every column's header row stands at the window's top at one
/// height, each column's button stands in its own row before the window's controls, the bar
/// takes none of them over, and a press in a row runs its button.
#[test]
fn a_hosted_shell_keeps_every_columns_header_in_its_own_row() {
    let pressed = SharedRc::new(Cell::new(0));
    let count = SharedRc::clone(&pressed);
    let compose =
        Button::new("mail.compose", "Compose").on_click(move || count.set(count.get() + 1));
    let shell = NavShell::new()
        .content_kind(ContentKind::Page)
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .overview(TestLeaf::fill("overview").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .sidebar_header(
            "Mailboxes",
            vec![Box::new(Button::icon_only(
                "mail.new-mailbox",
                Icon::Plus,
                "New mailbox",
            ))],
        )
        .overview_header("Inbox", vec![Box::new(compose)])
        .content_header(
            "Design review",
            vec![Box::new(Button::icon_only(
                "mail.reply",
                Icon::Reply,
                "Reply",
            ))],
        );
    let mut h = Driver::in_window_frame(State::new(), Box::new(shell), (1280, 800), Theme::dark());
    for _ in 0..2 {
        if let Some(shell) = h.widget_mut::<NavShell>() {
            shell.snap();
        }
        h.relayout();
    }
    assert_eq!(
        tilo_ui_app::window_toolbar::declared(),
        None,
        "the bar declares none of them"
    );
    assert!(h.row_holds_bar(), "the header row is the window's bar");
    let g = h
        .widget_mut::<NavShell>()
        .expect("the frame hosts the shell")
        .geometry()
        .clone();
    let rows = [Column::Sidebar, Column::Overview, Column::Content].map(|c| g.test_header(c));
    for row in rows {
        assert!(
            row.width() > 0.0 && row.y0.abs() < 1e-9,
            "at the top: {rows:?}"
        );
        assert!(
            (row.height() - rows[0].height()).abs() < 1e-9,
            "one height: {rows:?}"
        );
    }
    let targets = h.targets();
    let shown = |id: &str| targets.get(id).copied().filter(|rect| rect.area() > 0.0);
    for (id, row) in ["mail.new-mailbox", "mail.compose", "mail.reply"]
        .into_iter()
        .zip(rows)
    {
        let button = shown(id).expect("the button shows in its row");
        assert!(
            button.x0 >= row.x0 - 1e-9 && button.x1 <= row.x1 + 1e-9 && button.y1 <= row.y1,
            "{id} stands in its own column's row: {button:?} in {row:?}"
        );
    }
    assert!(shown("mail.reply").is_some_and(|reply| reply.x1 <= g.window_controls.x0));
    let compose = shown("mail.compose").expect("Compose shows");
    h.click_at(compose.center().x, compose.center().y);
    assert_eq!(pressed.get(), 1, "the press in the row runs the button");
}

/// A conversation floated as a card shows only itself: no list over it and no toggle for one,
/// whatever the app remembered of its sidebar. Back in the strip, its list is a press away.
#[test]
fn a_narrow_window_floating_as_a_card_shows_its_content_alone() {
    let shell = NavShell::new()
        .with_window_toolbar()
        .content_kind(ContentKind::Reading);
    let link = shell.window_bar_link();
    let shell = shell
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0);
    let mut h = harness(shell, 360, 520, Theme::dark());
    tilo_ui_app::window_toolbar::deliver(ToolbarEvent::Floating { floating: true });
    link.stage_sidebar(true);
    link.put_drawer_away();
    settle(&mut h, 360, 520);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::None);
    assert_eq!(
        link.parts().sidebar,
        None,
        "no toggle for a sidebar it does not show"
    );
    assert!(child_rect(&h, "content").is_some_and(|r| r.width() >= 359.0));
    assert!(
        child_rect(&h, "sidebar").is_none_or(|r| r.area() <= 0.0),
        "no list over the card"
    );

    tilo_ui_app::window_toolbar::deliver(ToolbarEvent::Floating { floating: false });
    settle(&mut h, 376, 520);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Screen);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
    assert!(
        link.parts().sidebar.is_some(),
        "in the strip its list is a press away"
    );
}
