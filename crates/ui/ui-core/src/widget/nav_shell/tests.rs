use super::*;
use crate::app::driver::Driver;
use crate::state::State;
use crate::widget::Text;
use std::cell::Cell;
use std::rc::Rc as SharedRc;

mod adaptive;
mod capabilities;
mod chosen;
mod content_swap;
mod counts;
mod hosted;
mod inspector_sheet;
mod listed_actions;
mod nothing_listed;
mod page_title;
mod places;
mod sidebar_button;
mod sidebar_scroll;
mod touch;

use crate::testing::TestLeaf;

fn mail_shell() -> NavShell {
    NavShell::new()
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .overview(TestLeaf::fill("overview").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .sidebar_header("Mailboxes", Vec::new())
        .overview_header("Inbox", Vec::new())
        .content_header("Design review", Vec::new())
}

fn harness(shell: NavShell, w: u32, h: u32, theme: Theme) -> Driver {
    Driver::for_root(State::new(), shell, (w, h), theme)
}

fn settle(h: &mut Driver, w: u32, height: u32) {
    h.root_mut::<NavShell>().snap();
    h.relayout_at((w, height)).expect("layout settles");
    h.root_mut::<NavShell>().snap();
    h.relayout_at((w, height)).expect("layout settles");
    h.relayout_at((w, height)).expect("layout settles");
}

fn geometry(h: &Driver) -> NavGeometry {
    h.root::<NavShell>().geometry().clone()
}

/// Where the widget with test id `id` stands in the frame, at any depth under the shell.
fn rect_named(h: &Driver, id: &str) -> Option<Rect> {
    fn find(node: &crate::headless::InspectionNode, id: &str) -> Option<Rect> {
        if node.test_id.as_deref() == Some(id) && node.rect[2] > 0.0 && node.rect[3] > 0.0 {
            let [x, y, w, height] = node.rect;
            return Some(Rect::new(x, y, x + w, y + height));
        }
        node.children.iter().find_map(|child| find(child, id))
    }
    find(&h.inspection().root, id)
}

fn child_rect(h: &Driver, id: &str) -> Option<Rect> {
    h.root::<NavShell>()
        .children()
        .into_iter()
        .find(|child| child.test_id() == Some(id))
        .map(Widget::get_rect)
}

fn notes_shell() -> NavShell {
    NavShell::new()
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .sidebar_header("Notes", Vec::new())
        .content_header("", Vec::new())
}

fn files_shell() -> NavShell {
    NavShell::new()
        .content_kind(ContentKind::List)
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .sidebar_header("Places", Vec::new())
        .content_header("Home", Vec::new())
}

fn settings_shell() -> NavShell {
    NavShell::new()
        .content_kind(ContentKind::Page)
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .sidebar_header("Settings", Vec::new())
        .content_header("Appearance", Vec::new())
}

fn document_shell() -> NavShell {
    notes_shell().content_kind(ContentKind::Document)
}

tilo_test_env::scenario! {
    fn the_shell_docks_folds_and_pushes_as_the_window_and_text_ask;

    // At any width the shell picks the layout in which everything shown is usable: docked while
    // the sidebar fits beside a column of content, one column at a time below that, and a drawer
    // only for a content-primary app.
    phase the_sidebar_docks_while_the_contents_real_minimum_fits;
    {
        let theme = Theme::dark();
        let m = Metrics::resolve(&theme);
        use SidebarPlacement::{Docked, Drawer, Screen};
        for (name, make, kind, narrow) in [
            (
                "files",
                files_shell as fn() -> NavShell,
                ContentKind::List,
                Screen,
            ),
            ("settings", settings_shell, ContentKind::Page, Screen),
            ("notes", document_shell, ContentKind::Document, Drawer),
        ] {
            let min = m.beside_sidebar_min(kind);
            for width in [320_u32, 360, 480, 520, 560, 600, 720, 840, 900, 1400] {
                let mut h = harness(make(), width, 700, theme.clone());
                settle(&mut h, width, 700);
                let g = geometry(&h);
                let w = f64::from(width);
                assert!(
                    (g.breakpoint - (m.sidebar_min + min)).abs() < 1e-6,
                    "{name} {width}: the sidebar at its minimum plus a column of content"
                );
                let docked = w >= m.sidebar_min + min;
                assert_eq!(
                    g.sidebar,
                    if docked { Docked } else { narrow },
                    "{name} {width}"
                );
                let content = child_rect(&h, "content");
                if docked {
                    let content = content.expect("the content is laid out beside the sidebar");
                    let sidebar = child_rect(&h, "sidebar").expect("the sidebar is usable");
                    assert!(content.width() >= min - 1e-6, "{name} {width}: {content:?}");
                    assert!(
                        (sidebar.width() - m.sidebar.min(w - min)).abs() < 0.5,
                        "{name} {width}: the sidebar shrinks toward its minimum first ({sidebar:?})"
                    );
                    assert!(sidebar.x1 <= content.x0 + 0.5, "side by side");
                } else if narrow == Screen {
                    assert_eq!(g.stack, [NavPane::Sidebar, NavPane::Content]);
                    let (front, shown, away) = if name == "files" {
                        (NavPane::Content, "content", "sidebar")
                    } else {
                        (NavPane::Sidebar, "sidebar", "content")
                    };
                    assert_eq!(h.root::<NavShell>().front_pane(), front, "{name} {width}");
                    assert_eq!(
                        child_rect(&h, shown).map(|r| r.width()),
                        Some(w),
                        "{name} {width}: it fills the window"
                    );
                    assert!(child_rect(&h, away).is_none(), "one at a time");
                    if name == "files" {
                        assert_eq!(g.back, Rect::ZERO, "{name} {width}: the list is a place");
                        assert!(
                            g.toggle.width() > 0.0,
                            "{name} {width}: the toggle leads to the places"
                        );
                    }
                } else {
                    assert_eq!(
                        content.map(|r| r.width()),
                        Some(w),
                        "{name} {width}: the document fills the window"
                    );
                }
                assert_eq!(g.open_drawer(), None, "{name} {width}");
            }
        }
        // A docked Files sidebar works down to a column of content, under the
        // breakpoint a painted app guesses at (840).
        assert!(
            m.sidebar_min + m.beside_sidebar_min(ContentKind::List)
                < f64::from(crate::tokens::Metrics::BASE.compact_breakpoint)
        );
    }


    // A window in a strip is a window of its own: the shell decides from the window's width, never
    // the display's.
    phase a_window_in_a_strip_folds_its_sidebar_rather_than_squeeze_the_list;
    {
        let theme = Theme::dark();
        let m = Metrics::resolve(&theme);
        let at = |width: u32| {
            let mut h = harness(files_shell(), width, 700, theme.clone());
            settle(&mut h, width, 700);
            (geometry(&h).sidebar, child_rect(&h, "content"))
        };
        let (sidebar, content) = at(616);
        assert_eq!(
            sidebar,
            SidebarPlacement::Screen,
            "a 616-wide place folds its sidebar: a list squeezed beside it is a phone's screen"
        );
        assert_eq!(
            content.map(|r| r.width()),
            Some(616.0),
            "the list takes the whole window"
        );
        let (sidebar, content) = at(720);
        assert_eq!(sidebar, SidebarPlacement::Docked);
        let content = content.expect("the content stands beside the sidebar");
        assert!(
            content.width() >= m.beside_sidebar_min(ContentKind::List) - 0.5,
            "the content keeps a column ({} wide)",
            content.width()
        );
    }


    // A window opened narrow has no place to keep yet, so it opens on a list,
    // whole screen: the content's own list where it has one, else the sidebar.
    // Never the content, which nothing has chosen.
    phase a_fresh_narrow_window_opens_on_its_list;
    {
        let mut h = harness(mail_shell(), 360, 700, Theme::dark());
        settle(&mut h, 360, 700);
        let g = geometry(&h);
        assert_eq!(g.sidebar, SidebarPlacement::Screen);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
        assert_eq!(
            g.overview_column.width(),
            360.0,
            "the inbox fills the screen"
        );
        assert!(g.content_column.width() <= 0.0 || g.content_column.x0 >= 360.0);
        let shell = NavShell::new()
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .content(TestLeaf::fill("content").boxed().0)
            .content_header("General", Vec::new());
        let mut h = harness(shell, 360, 700, Theme::dark());
        settle(&mut h, 360, 700);
        let g = geometry(&h);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
        assert_eq!(g.sidebar_column.width(), 360.0, "the pages fill the screen");
    }

    // The class a shell shows is the rect's, held through the hysteresis band so
    // a window resting on a threshold does not flutter.
    phase the_shell_follows_the_window_and_the_text_scale;
    {
        for (scale, theme) in [
            (1.0, Theme::dark()),
            (2.0, Theme::dark().with_text_scale(2.0)),
        ] {
            let m = Metrics::resolve(&theme);
            for width in [1280_u32, 800, 560] {
                let mut h = harness(mail_shell(), width, 800, theme.clone());
                settle(&mut h, width, 800);
                let g = geometry(&h);
                let w = f64::from(width);
                let beside = m.overview + m.beside_sidebar_min(ContentKind::Page);
                let pair = m.overview + m.page_min;
                let breakpoint = m.sidebar_min + beside;
                assert!((g.breakpoint - breakpoint).abs() < 1e-6, "{scale}x {width}");
                let expect_docked = w >= breakpoint;
                assert_eq!(
                    g.sidebar,
                    if expect_docked {
                        SidebarPlacement::Docked
                    } else {
                        SidebarPlacement::Screen
                    },
                    "{scale}x {width}: the sidebar docks only beside unsqueezed content"
                );
                assert_eq!(
                    g.side_by_side,
                    expect_docked || w >= pair,
                    "{scale}x {width}: overview and content stand side by side only while both fit"
                );
                assert!(g.header_height >= f64::from(theme.tokens.metrics.header_height));
                if expect_docked {
                    assert!((g.sidebar_column.width() - m.sidebar.min(w - beside)).abs() < 0.5);
                    assert_eq!(g.sidebar_column.x0, 0.0);
                } else {
                    let list = if g.stack.contains(&NavPane::Overview) {
                        g.overview_column
                    } else {
                        g.sidebar_column
                    };
                    assert_eq!(
                        list.width(),
                        w,
                        "the list is a whole screen: {scale}x {width}, stack {:?}",
                        g.stack
                    );
                }
                for id in ["sidebar", "overview", "content"] {
                    if let Some(rect) = child_rect(&h, id) {
                        assert!(
                            (rect.y0 - g.header_height).abs() < 0.5,
                            "{scale}x {width}: {id} starts under the header row ({})",
                            rect.y0
                        );
                    }
                }
                assert!(
                    (g.toggle.x0 - m.inset).abs() < 0.5,
                    "{scale}x {width}: the toggle leads ({:?})",
                    g.toggle
                );
                assert!(g.toggle.y0 > 0.0 && g.toggle.y1 < g.header_height);
                assert_eq!(
                    g.back,
                    Rect::ZERO,
                    "{scale}x {width}: this is the screen the window opened on, with no way back"
                );
            }
        }
    }


    phase the_inspector_docks_beside_the_content_and_rises_as_a_sheet_when_narrow;
    {
        let shell = NavShell::new()
            .content(TestLeaf::fill("content").boxed().0)
            .inspector(TestLeaf::fill("inspector").boxed().0);
        let mut h = harness(shell, 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert_eq!(g.inspector, InspectorPlacement::Docked);
        assert_eq!(g.inspector_column.x1, 1280.0);
        assert!(g.inspector_toggle.width() > 0.0);
        assert!(
            child_rect(&h, "content").is_some_and(|r| r.x1 <= g.inspector_column.x0 + 0.5),
            "the content ends where the inspector starts"
        );

        settle(&mut h, 600, 800);
        let g = geometry(&h);
        assert_eq!(g.inspector, InspectorPlacement::Sheet);
        assert_eq!(
            g.inspector_column,
            Rect::ZERO,
            "closed by default as a sheet"
        );
        h.root_mut::<NavShell>().toggle_inspector();
        settle(&mut h, 600, 800);
        assert!(geometry(&h).inspector_column.width() > 0.0);
    }

    phase a_view_chooses_the_narrow_form_and_opens_it;
    {
        // A view that asks for a drawer: the inspector comes in from the trailing edge over
        // the content rather than up from the foot, and the view opens it.
        let shell = NavShell::new()
            .content(TestLeaf::fill("content").boxed().0)
            .inspector(TestLeaf::fill("inspector").boxed().0)
            .inspector_collapse(InspectorCollapse::Drawer);
        let mut h = harness(shell, 600, 800, Theme::dark());
        settle(&mut h, 600, 800);
        let g = geometry(&h);
        assert_eq!(g.inspector, InspectorPlacement::Drawer);
        assert_eq!(g.inspector_column, Rect::ZERO, "closed until the view opens it");
        assert_eq!(g.sheet_covered, 0.0, "a drawer covers no page foot");

        h.root_mut::<NavShell>().set_inspector_open(true);
        settle(&mut h, 600, 800);
        let g = geometry(&h);
        assert!(
            g.inspector_column.x1 <= 600.0 && g.inspector_column.x1 > 590.0,
            "against the trailing edge, one gutter in: {:?}",
            g.inspector_column
        );
        assert!(
            g.inspector_column.x0 > 0.0 && g.inspector_column.width() > 0.0,
            "a panel over the content, not a column taken from it"
        );
        assert_eq!(
            child_rect(&h, "content").map(|r| r.x1),
            Some(600.0),
            "the content keeps the whole window"
        );

        // And a view that asks for the sidebar as a drawer gets one even where the content's
        // kind would have made it a screen of the push stack.
        let list = NavShell::new()
            .content_kind(ContentKind::List)
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .content(TestLeaf::fill("content").boxed().0)
            .sidebar_collapse(SidebarCollapse::Drawer);
        let mut h = harness(list, 400, 700, Theme::dark());
        settle(&mut h, 400, 700);
        assert_eq!(geometry(&h).sidebar, SidebarPlacement::Drawer);
        h.root_mut::<NavShell>().set_sidebar_open(true);
        settle(&mut h, 400, 700);
        assert!(
            geometry(&h).sidebar_column.width() > 0.0,
            "the view opened the drawer"
        );
    }


    // Narrowing a docked window keeps the person's place: they land on the
    // page they were on, Back goes to the list, and widening docks again with
    // the same page open.
    phase the_open_page_is_kept_across_docked_push_and_docked;
    {
        let list = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
            "pages",
            vec![
                crate::widget::NavItem::new("general", "General"),
                crate::widget::NavItem::new("appearance", "Appearance"),
            ],
        )])
        .selected("appearance");
        let shell = NavShell::new()
            .sidebar(Box::new(list))
            .content(TestLeaf::fill("content").boxed().0)
            .content_header("Appearance", Vec::new());
        let mut h = harness(shell, 1280, 700, Theme::dark());
        settle(&mut h, 1280, 700);
        let point = geometry(&h).content_column.center();
        h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
        h.dispatch_to_root(&crate::synthetic_input::mouse_press());
        settle(&mut h, 360, 700);
        let g = geometry(&h);
        assert_eq!(g.sidebar, SidebarPlacement::Screen);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
        assert_eq!(child_rect(&h, "content").map(|r| r.width()), Some(360.0));
        assert!(g.back.width() > 0.0, "Back goes to the list");
        assert!(h.root_mut::<NavShell>().back());
        settle(&mut h, 360, 700);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
        assert!(
            h.a11y_node_named("Appearance").is_some(),
            "the list still marks the open page"
        );
        settle(&mut h, 1280, 700);
        let g = geometry(&h);
        assert_eq!(g.sidebar, SidebarPlacement::Docked);
        assert!(g.content_column.width() > 0.0 && g.sidebar_column.width() > 0.0);
        assert_eq!(g.stack, Vec::<NavPane>::new());
    }
}

/// Keyboard focus survives a switch to one column at a time: a person
/// arrowing through the list lands on the list screen, focus where it was.
#[test]
fn narrowing_keeps_the_keyboard_where_it_was() {
    let list = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
        "pages",
        vec![
            crate::widget::NavItem::new("general", "General"),
            crate::widget::NavItem::new("appearance", "Appearance"),
        ],
    )])
    .selected("appearance");
    let shell = NavShell::new()
        .sidebar(Box::new(list))
        .content(TestLeaf::fill("content").boxed().0);
    let mut h = harness(shell, 1280, 700, Theme::dark());
    settle(&mut h, 1280, 700);
    let row = h.a11y_node_named("Appearance").expect("the list row");
    h.perform_a11y_action(row, &crate::a11y::Action::Focus)
        .expect("focus the list");
    assert!(h.focused().is_some());
    settle(&mut h, 360, 700);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Sidebar,
        "the list the keyboard is in stays in front"
    );
    assert!(h.focused().is_some(), "focus is kept");
    assert!(h.a11y_node_named("Appearance").is_some());
}

/// A page the person chose in a wide window is the page a narrow one shows: the focus the
/// sidebar keeps is the row they pressed, not a wish to go back to the list
/// (`narrowing_keeps_the_keyboard_where_it_was` keeps a list the keyboard is in).
#[test]
fn narrowing_shows_the_page_chosen_in_the_sidebar() {
    let shell = || {
        NavShell::new()
            .sidebar(Box::new(crate::widget::NavList::new(vec![
                crate::widget::NavSection::untitled(
                    "pages",
                    vec![
                        crate::widget::NavItem::new("general", "General"),
                        crate::widget::NavItem::new("appearance", "Appearance"),
                    ],
                ),
            ])))
            .content(TestLeaf::fill("content").boxed().0)
    };
    let mut h = harness(shell(), 1280, 700, Theme::dark());
    settle(&mut h, 1280, 700);
    // A press on the row: it takes the keyboard, and chooses its page.
    let row = h.a11y_node_named("Appearance").expect("the sidebar row");
    h.perform_a11y_action(row, &crate::a11y::Action::Focus)
        .expect("the row takes the keyboard");
    let row = h.a11y_node_named("Appearance").expect("the sidebar row");
    h.perform_a11y_action(row, &crate::a11y::Action::Activate)
        .expect("the row chooses its page");
    settle(&mut h, 1280, 700);
    settle(&mut h, 360, 700);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Content,
        "the chosen page stays in front"
    );
}

tilo_test_env::scenario! {
    fn the_sidebar_toggles_floats_and_resizes_at_its_edge;

    phase the_sidebar_toggle_moves_and_the_choice_is_remembered_per_size_class;
    {
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        let before = geometry(&h);
        assert!(h.root::<NavShell>().is_sidebar_open());
        assert!(before.toggle.width() > 0.0);

        h.root_mut::<NavShell>().toggle_sidebar();
        settle(&mut h, 1280, 800);
        let hidden = geometry(&h);
        assert!(!h.root::<NavShell>().is_sidebar_open());
        assert_eq!(hidden.sidebar_column, Rect::ZERO);
        assert_eq!(hidden.toggle, before.toggle, "the toggle stays top-leading");
        assert_eq!(hidden.toggle_column, Some(Column::Overview));
        assert!(
            child_rect(&h, "sidebar").is_none(),
            "a hidden sidebar takes no input"
        );
        assert_eq!(child_rect(&h, "overview").map(|r| r.x0), Some(0.0));

        settle(&mut h, 560, 800);
        assert_eq!(geometry(&h).sidebar, SidebarPlacement::Screen);
        settle(&mut h, 1280, 800);
        assert!(
            !h.root::<NavShell>().is_sidebar_open(),
            "the wide window remembers the sidebar was hidden"
        );
        h.root_mut::<NavShell>().toggle_sidebar();
        settle(&mut h, 1280, 800);
        assert_eq!(
            geometry(&h).sidebar_column.width(),
            before.sidebar_column.width()
        );
        assert_eq!(
            child_rect(&h, "overview").map(|r| r.x0),
            Some(before.sidebar_column.x1)
        );
    }


    phase the_toggle_says_what_it_does_to_whoever_reads_the_window;
    {
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        let name = |h: &Driver| {
            h.root::<NavShell>()
                .toggle
                .accessibility()
                .and_then(|node| node.name)
        };
        assert_eq!(name(&h).as_deref(), Some("Hide sidebar"));
        h.root_mut::<NavShell>().toggle_sidebar();
        settle(&mut h, 1280, 800);
        assert_eq!(name(&h).as_deref(), Some("Show sidebar"));
        // A reader or an agent shown the window presses the toggle through the same
        // accessibility control, and it does what its name says.
        let toggle = h
            .a11y_node_named("Show sidebar")
            .expect("the toggle stands to be pressed");
        h.perform_a11y_action(toggle, &crate::a11y::Action::Activate)
            .expect("the toggle answers");
        settle(&mut h, 1280, 800);
        assert!(h.root::<NavShell>().is_sidebar_open(), "the sidebar is open");
        assert_eq!(name(&h).as_deref(), Some("Hide sidebar"));
    }

    phase the_system_shortcut_toggles_the_sidebar;
    {
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        h.root_mut::<NavShell>()
            .set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Linux));
        settle(&mut h, 1280, 800);
        let key = crate::synthetic_input::key_input(&Key::Character("L".into()), ElementState::Pressed);
        let modifiers = winit::event::Modifiers::from(
            winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT,
        );
        assert!(h.dispatch_to_root_with(&key, &modifiers));
        assert!(!h.root::<NavShell>().is_sidebar_open());
        assert!(h.dispatch_to_root_with(&key, &modifiers));
        assert!(h.root::<NavShell>().is_sidebar_open());
    }
}

/// A content-primary app's drawer floats over the document without a scrim:
/// the document stays visible and exposed, and a press on it puts the
/// drawer away without reaching it.
#[test]
fn an_open_drawer_floats_over_the_content_without_a_scrim() {
    let (content, content_events) = TestLeaf::fill("content").boxed();
    let shell = NavShell::new()
        .content_kind(ContentKind::Document)
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(content)
        .sidebar_header("Notes", Vec::new())
        .content_header("", Vec::new());
    let mut h = harness(shell, 480, 600, Theme::dark());
    settle(&mut h, 480, 600);
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 480, 600);
    let g = geometry(&h);
    let gap = f64::from(Theme::dark().tokens.spacing.s2);
    assert!(
        (g.sidebar_column.x0 - gap).abs() < 0.5,
        "the drawer floats s2 in"
    );
    assert_eq!(g.open_drawer(), Some(Column::Sidebar));
    let ids: Vec<_> = h
        .root::<NavShell>()
        .children()
        .iter()
        .filter_map(|child| child.test_id().map(str::to_owned))
        .collect();
    assert!(
        ids.contains(&"content".to_owned()) && ids.contains(&"sidebar".to_owned()),
        "not modal: the document stays exposed beside the drawer ({ids:?})"
    );

    let seen = content_events.events();
    let point = Point::new(g.sidebar_column.x1 + 40.0, 300.0);
    h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
    let handled = h.dispatch_to_root(&crate::synthetic_input::mouse_press());
    assert!(handled);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert_eq!(
        content_events.events(),
        seen + 1,
        "the document heard the pointer move, not the dismissing press"
    );
}

tilo_test_env::scenario! {
    fn a_narrow_window_pushes_its_screens_and_comes_back;

    phase push_mode_makes_the_sidebar_the_first_screen;
    {
        let shell = NavShell::new()
            .mode(NavMode::Push)
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .content(TestLeaf::fill("content").boxed().0);
        let mut h = harness(shell, 360, 800, Theme::dark());
        settle(&mut h, 360, 800);
        let g = geometry(&h);
        assert_eq!(g.sidebar, SidebarPlacement::Screen);
        assert_eq!(g.stack, [NavPane::Sidebar, NavPane::Content]);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
        assert!(
            g.toggle.width() > 0.0,
            "the sidebar's screen carries the toggle that puts it away"
        );
        assert_eq!(child_rect(&h, "sidebar").map(|r| r.width()), Some(360.0));

        h.root_mut::<NavShell>().show(NavPane::Content);
        settle(&mut h, 360, 800);
        assert!(geometry(&h).back.width() > 0.0);
        assert!(h.root_mut::<NavShell>().back());
        settle(&mut h, 360, 800);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);

        settle(&mut h, 1280, 800);
        assert_eq!(geometry(&h).sidebar, SidebarPlacement::Docked);
    }

    phase narrow_windows_push_the_content_over_the_overview;
    {
        let mut h = harness(mail_shell(), 560, 800, Theme::dark());
        settle(&mut h, 560, 800);
        let g = geometry(&h);
        assert_eq!(
            g.stack,
            [NavPane::Sidebar, NavPane::Overview, NavPane::Content]
        );
        assert_eq!(
            h.root::<NavShell>().front_pane(),
            NavPane::Overview,
            "a narrow window opens on the content's list"
        );
        assert_eq!(
            g.back,
            Rect::ZERO,
            "the list the window opens on has nowhere to go back to"
        );
        assert!(g.toggle.width() > 0.0, "it holds the way to the mailboxes");

        h.root_mut::<NavShell>().show(NavPane::Sidebar);
        settle(&mut h, 560, 800);
        let g = geometry(&h);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
        assert!(
            g.back.width() > 0.0,
            "the mailboxes go back to the list they covered"
        );
        assert_eq!(g.toggle, Rect::ZERO, "one affordance holds the corner");
        assert!(h.root_mut::<NavShell>().back());
        settle(&mut h, 560, 800);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);

        h.root_mut::<NavShell>().show(NavPane::Content);
        settle(&mut h, 560, 800);
        let g = geometry(&h);
        assert!(g.back.width() > 0.0, "a pushed screen has a Back button");
        assert_eq!(g.toggle, Rect::ZERO, "one affordance holds the corner");
        assert_eq!(g.content_column.x0, 0.0);
        assert_eq!(g.overview_column, Rect::ZERO, "the overview has slid away");

        let esc =
            crate::synthetic_input::key_input(&Key::Named(NamedKey::Escape), ElementState::Pressed);
        assert!(h.dispatch_to_root(&esc));
        settle(&mut h, 560, 800);
        assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    }
}

/// Escape goes back the way the bar's Back does, into an app's own page stack too: a page the app
/// opened over its list pops, and the way back to the list goes with it.
#[test]
fn escape_pops_the_apps_page_stack() {
    let can = SharedRc::new(Cell::new(true));
    let read = SharedRc::clone(&can);
    let write = SharedRc::clone(&can);
    let shell = NavShell::new()
        .content_kind(ContentKind::Document)
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .content_header("Call the landlord", Vec::new())
        .on_back(move || read.get(), move || write.set(false));
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    assert!(
        h.root::<NavShell>().can_go_back(),
        "the page has a way back"
    );
    let esc =
        crate::synthetic_input::key_input(&Key::Named(NamedKey::Escape), ElementState::Pressed);
    assert!(h.dispatch_to_root(&esc), "Escape is the way back");
    settle(&mut h, 1280, 800);
    assert!(!can.get(), "the app's page stack popped");
    assert!(
        !h.root::<NavShell>().can_go_back(),
        "and the way back went with the page"
    );
}

#[test]
fn choosing_a_destination_in_a_narrow_window_moves_on() {
    let list = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
        "places",
        vec![
            crate::widget::NavItem::new("inbox", "Inbox"),
            crate::widget::NavItem::new("sent", "Sent"),
        ],
    )]);
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Box::new(list))
        .content(TestLeaf::fill("content").boxed().0);
    let mut h = harness(shell, 360, 800, Theme::dark());
    settle(&mut h, 360, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    let g = geometry(&h);
    let row = crate::widget::nav_list::nav_row::nav_row_height(&Theme::dark());
    let point = Point::new(60.0, g.body(Column::Sidebar).y0 + row * 1.5);
    let device = winit::event::DeviceId::dummy();
    h.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    });
    for state in [ElementState::Pressed, ElementState::Released] {
        h.dispatch_to_root(&WindowEvent::MouseInput {
            device_id: device,
            state,
            button: MouseButton::Left,
        });
    }
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Content,
        "choosing Sent pushes the content"
    );
}

/// Frames as a window's host draws them: the one an input asked for, then
/// more only while the shell asks.
fn pump(h: &mut Driver, w: u32, height: u32) {
    h.relayout_at((w, height)).expect("layout");
    for _ in 0..32 {
        let shell = h.root::<NavShell>();
        if !shell.needs_animation_frame() {
            return;
        }
        h.relayout_at((w, height)).expect("layout");
    }
}

/// A toggle that lands in one tick (a key, or the first frame after a
/// busy device let the whole slide's time pass) is laid out on screen: the
/// shell asks its host for the frame that lays the landed pose out, and
/// the one that drops what that pose hides.
#[test]
fn a_toggle_that_lands_at_once_reaches_the_screen() {
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>()
        .set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Linux));
    settle(&mut h, 1280, 800);
    let open = geometry(&h).sidebar_column.width();
    assert!(open > 0.0);
    let key = crate::synthetic_input::key_input(&Key::Character("L".into()), ElementState::Pressed);
    let modifiers = winit::event::Modifiers::from(
        winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT,
    );
    assert!(h.dispatch_to_root_with(&key, &modifiers));
    pump(&mut h, 1280, 800);
    assert!(
        geometry(&h).sidebar_column.width() < 0.5,
        "the shortcut collapsed the sidebar on screen: {:?}",
        geometry(&h).sidebar_column
    );
    assert!(h.dispatch_to_root_with(&key, &modifiers));
    pump(&mut h, 1280, 800);
    assert!((geometry(&h).sidebar_column.width() - open).abs() < 0.5);

    let _arrived = crate::motion::testing::settled();
    let toggle = geometry(&h).toggle.center();
    h.click_at(toggle.x, toggle.y);
    pump(&mut h, 1280, 800);
    assert!(
        geometry(&h).sidebar_column.width() < 0.5,
        "the button collapsed the sidebar on screen"
    );
    let toggle = geometry(&h).toggle.center();
    h.click_at(toggle.x, toggle.y);
    pump(&mut h, 1280, 800);
    assert!((geometry(&h).sidebar_column.width() - open).abs() < 0.5);
}

#[test]
fn dragging_the_trailing_edge_resizes_and_below_its_minimum_hides() {
    let mut h = harness(mail_shell(), 1400, 800, Theme::dark());
    settle(&mut h, 1400, 800);
    let g = geometry(&h);
    let edge = g.resize_handle.center();
    let device = winit::event::DeviceId::dummy();
    let move_to = |h: &mut Driver, x: f64| {
        h.dispatch_to_root(&WindowEvent::CursorMoved {
            device_id: device,
            position: winit::dpi::PhysicalPosition::new(x, edge.y),
        })
    };
    move_to(&mut h, edge.x);
    assert!(h.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: device,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }));
    assert!(move_to(&mut h, edge.x + 60.0));
    settle(&mut h, 1400, 800);
    let wider = geometry(&h).sidebar_column.width();
    assert!(
        (wider - (g.sidebar_column.width() + 60.0)).abs() < 1.0,
        "{wider}"
    );

    move_to(&mut h, 20.0);
    h.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: device,
        state: ElementState::Released,
        button: MouseButton::Left,
    });
    settle(&mut h, 1400, 800);
    assert!(
        !h.root::<NavShell>().is_sidebar_open(),
        "dragged below its minimum, the sidebar hides"
    );
}

tilo_test_env::scenario! {
    fn header_rows_and_the_window_bar_hold_titles_status_and_actions;



    phase history_is_one_glyph_in_the_window_top_trailing_corner;
    {
        {
            let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
            settle(&mut h, 1280, 800);
            assert_eq!(geometry(&h).history, Rect::ZERO);
            assert!(child_rect(&h, "nav-shell-history").is_none());
        }

        {
            let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
            h.root_mut::<NavShell>()
                .set_history_control(Some(HistoryControl::Closed));
            settle(&mut h, 1280, 800);
            let g = geometry(&h);
            let m = Metrics::resolve(&Theme::dark());
            assert!(g.history.width() > 0.0);
            assert!(
                (g.history.x1 - (1280.0 - m.row.gutter)).abs() < 1e-9,
                "the history chip keeps the gutter the title starts at"
            );
            assert!(g.history.y1 <= g.header_height);
            let button = h
                .root::<NavShell>()
                .children()
                .into_iter()
                .find(|child| child.test_id() == Some("nav-shell-history"))
                .and_then(Widget::accessibility)
                .unwrap();
            assert_eq!(button.state.checked, Some(false));
            assert!(
                g.actions_band(Column::Content).x1 <= g.history.x0,
                "the app's actions end before it"
            );
        }

        {
            let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
            h.root_mut::<NavShell>()
                .set_history_control(Some(HistoryControl::Open));
            settle(&mut h, 1280, 800);
            let button = h
                .root::<NavShell>()
                .children()
                .into_iter()
                .find(|child| child.test_id() == Some("nav-shell-history"))
                .and_then(Widget::accessibility)
                .unwrap();
            assert_eq!(button.state.checked, Some(true));
        }

        {
            let shell = NavShell::new()
                .content(TestLeaf::fill("content").boxed().0)
                .inspector(TestLeaf::fill("inspector").boxed().0);
            let mut h = harness(shell, 1280, 800, Theme::dark());
            h.root_mut::<NavShell>()
                .set_history_control(Some(HistoryControl::Closed));
            settle(&mut h, 1280, 800);
            let g = geometry(&h);
            assert!(g.inspector_toggle.width() > 0.0);
            assert!(g.inspector_toggle.x1 < g.history.x0, "{g:?}");
            assert!((g.inspector_toggle.y0 - g.history.y0).abs() < 0.5);
        }

        {
            let shell = NavShell::new().content(TestLeaf::fill("content").boxed().0);
            let mut h = harness(shell, 1024, 700, Theme::dark());
            h.root_mut::<NavShell>()
                .set_history_control(Some(HistoryControl::Closed));
            settle(&mut h, 1024, 700);
            let g = geometry(&h);
            let m = Metrics::resolve(&Theme::dark());
            assert!(g.header_height > 0.0);
            assert!((g.history.x1 - (1024.0 - m.row.gutter)).abs() < 1e-9);
        }

        {
            let mut h = harness(mail_shell(), 560, 800, Theme::dark());
            h.root_mut::<NavShell>()
                .set_history_control(Some(HistoryControl::Closed));
            settle(&mut h, 560, 800);
            let g = geometry(&h);
            assert!(g.history.width() > 0.0 && g.history.x1 <= 560.0, "{g:?}");
        }
    }

    phase a_status_the_person_can_act_on_has_its_button_beside_it_in_the_row;
    {
        const NOT_SAVED: &str = "Couldn't save this change to your history";
        let mut resource = tilo_actions::ResourceSpec::new("test.unsaved", "test.unsaved", NOT_SAVED);
        resource.actions.push(
            tilo_actions::ActionSpec::operation(
                "retry",
                "Retry",
                tilo_actions::OperationKind::Effect,
                "test.unsaved.retry",
                &(),
            )
            .unwrap(),
        );
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "retry").unwrap();
        let status =
            crate::widget::WindowStatus::says(NOT_SAVED).with_action(crate::widget::StatusAction {
                label: "Retry".into(),
                binding,
            });
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        assert!(h.root_mut::<NavShell>().set_sync_status(Some(&status)));
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        let button = h
            .root::<NavShell>()
            .children()
            .into_iter()
            .find(|child| child.test_id() == Some(crate::widget::STATUS_ACTION_ID))
            .map(|child| (child.get_rect(), child.name()))
            .expect("a button beside the status");
        let (button, name) = button;
        assert_eq!(name, "Button", "a design-system button, not painted text");
        assert!(button.width() > 0.0 && button.y1 <= g.header_height);
        assert!(button.x1 <= g.history.x0, "{button:?} before History {g:?}");
        let text = h
            .root::<NavShell>()
            .children()
            .into_iter()
            .find(|child| child.name() == "HeaderStatus")
            .map(|child| child.get_rect())
            .unwrap();
        assert!(text.width() > 0.0, "the message still shows");
        assert!(
            text.x1 <= button.x0,
            "the message {text:?} reads before its button {button:?}"
        );

        let offline = crate::widget::WindowStatus::says("Working offline");
        assert!(h.root_mut::<NavShell>().set_sync_status(Some(&offline)));
        settle(&mut h, 1280, 800);
        assert!(
            h.root::<NavShell>()
                .children()
                .into_iter()
                .all(|child| child.test_id() != Some(crate::widget::STATUS_ACTION_ID)),
            "no button without something to do"
        );
    }


    // A notice is the window bar's: it shows there in the location's place,
    // leaves on its own, and paints nothing over the window.
    phase a_toast_is_the_window_bars_notice_and_paints_nothing_over_the_window;
    {
        use crate::window_toolbar::{ToolbarNotice, notice, notify};
        notify(None);
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        let theme = Theme::dark();
        let mut text = crate::font_catalog::fixture_renderer();
        let mut shell = h.root_mut::<NavShell>();
        let _ = present_shell(&mut shell, &mut text, &theme, 1.0);
        let _ = present_shell(&mut shell, &mut text, &theme, 1.0);
        shell.toast("Saved", ToastKind::Success);
        assert_eq!(
            notice(),
            Some(ToolbarNotice::new("Saved", ToastKind::Success)),
            "the window's bar shows the notice"
        );
        assert_eq!(shell.toast_text(), Some("Saved"));
        let (_, damage) = present_shell(&mut shell, &mut text, &theme, 1.0);
        assert!(
            damage.is_empty(),
            "nothing is painted over the window: {damage:?}"
        );
        shell.dismiss_toast();
        assert_eq!(notice(), None, "taken down, it leaves the bar");
        assert_eq!(shell.toast_text(), None);
    }

    // Maximized, the window's controls join the header row at its trailing end: one bar, the app's
    // header and the window's controls side by side.
    phase the_window_bar_merges_into_the_header_row;
    {
        let theme = Theme::dark();
        let width = crate::widget::merged_row_width();
        let mut h = harness(mail_shell(), 1280, 800, theme);
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        assert!(
            h.root_mut::<NavShell>()
                .set_window_bar(Some(crate::widget::WindowBar {
                    controls_width: width,
                    lists_actions: false,
                })),
            "a header row takes the window's bar"
        );
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        let m = Metrics::resolve(&Theme::dark());
        assert!(g.header_height > 0.0, "a permanent header stays");
        assert!((g.window_controls.width() - width).abs() < 1e-9);
        assert!((g.window_controls.x1 - (1280.0 - m.inset)).abs() < 1e-9);
        assert_eq!(g.window_controls.y0, 0.0);
        assert_eq!(g.window_controls.y1, g.header_height, "the row's height");
        assert_eq!(g.window_row, Rect::new(0.0, 0.0, 1280.0, g.header_height));
        assert!(
            g.history.x1 <= g.window_controls.x0 - m.gap + 1e-9,
            "History stands before the window's controls: {g:?}"
        );
        assert!(g.actions_band(Column::Content).x1 <= g.history.x0);
        let part = h
            .root::<NavShell>()
            .window_bar_part()
            .expect("the row reports its part");
        assert_eq!(part.controls, g.window_controls);
        assert_eq!(
            child_rect(&h, "content").map(|r| r.y0),
            Some(g.header_height),
            "the content stands below a permanent header"
        );

        h.root_mut::<NavShell>().set_window_bar(None);
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert_eq!(g.window_controls, Rect::ZERO);
        assert!(
            (g.history.x1 - (1280.0 - m.row.gutter)).abs() < 1e-9,
            "the history chip keeps the gutter the title starts at"
        );
        assert!(h.root::<NavShell>().window_bar_part().is_none());
    }


    phase a_window_bar_that_presents_search_takes_the_headers_own_search_button_away;
    {
        {
            let mut shell = notes_shell_with_search();
            assert!(WindowHeader::set_window_presents_search(&mut shell, true));
            let mut h = harness(shell, 1280, 800, Theme::dark());
            settle(&mut h, 1280, 800);
            assert!(!has_child(&h, crate::widget::search_request::APP_SEARCH_BUTTON_ID));
            assert!(has_child(&h, "note-menu"), "the app's other actions stay");
        }

        {
            let mut h = harness(notes_shell_with_search(), 1280, 800, Theme::dark());
            settle(&mut h, 1280, 800);
            assert!(
                has_child(&h, crate::widget::search_request::APP_SEARCH_BUTTON_ID),
                "outside a window bar the header keeps its search button"
            );
            assert!(WindowHeader::set_window_presents_search(
                &mut *h.root_mut::<NavShell>(),
                true
            ));
            settle(&mut h, 1280, 800);
            assert!(!has_child(&h, crate::widget::search_request::APP_SEARCH_BUTTON_ID));
            assert!(has_child(&h, "note-menu"));
        }

        {
            let mut h = harness(notes_shell_with_search(), 1280, 800, Theme::dark());
            WindowHeader::set_window_presents_search(&mut *h.root_mut::<NavShell>(), true);
            settle(&mut h, 1280, 800);
            h.root_mut::<NavShell>().set_content_header(
                "Groceries",
                vec![Box::new(crate::widget::search_request::search_button(
                    crate::widget::search_request::SearchRequest::this_app(),
                ))],
            );
            settle(&mut h, 1280, 800);
            assert!(!has_child(&h, crate::widget::search_request::APP_SEARCH_BUTTON_ID));
        }
    }

    // A shell whose toolbar is its window bar's parts draws no header row, at
    // any width, whatever titles and actions it was given; its own buttons
    // become parts and run from the bar.
    phase a_toolbar_in_the_window_bar_leaves_no_header_row_and_its_buttons_are_parts;
    {
        use crate::window_toolbar::ToolbarEvent;
        let shell = mail_shell().with_window_toolbar();
        let mut h = harness(shell, 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert_eq!(
            g.header_height, 0.0,
            "no header row beside the window's bar"
        );
        assert_eq!(g.toggle, Rect::ZERO);
        assert_eq!(child_rect(&h, "content").map(|r| r.y0 < 1.0), Some(true));
        let toolbar = h.root::<NavShell>().window_toolbar();
        assert_eq!(
            toolbar.sidebar,
            Some(true),
            "the sidebar's toggle is a part"
        );
        assert!(
            toolbar.leading.is_empty(),
            "a wide window has nowhere to go back to"
        );
        assert!(
            h.root_mut::<NavShell>()
                .window_toolbar_event(&ToolbarEvent::ToggleSidebar)
        );
        settle(&mut h, 1280, 800);
        assert!(!h.root::<NavShell>().is_sidebar_open());
        assert!(
            !h.root_mut::<NavShell>()
                .window_toolbar_event(&ToolbarEvent::Action { id: "app".into() }),
            "the app's own actions are the app's"
        );

        settle(&mut h, 560, 800);
        let front = h.root::<NavShell>().front_pane();
        assert_ne!(front, NavPane::Sidebar, "a pushed pane is in front");
        let g = geometry(&h);
        assert_eq!(g.header_height, 0.0);
        assert_eq!(g.back, Rect::ZERO, "no Back button of the shell's own");
        let toolbar = h.root::<NavShell>().window_toolbar();
        assert_eq!(
            toolbar.leading.first().map(|action| action.id.as_str()),
            Some(super::window_toolbar::WINDOW_BACK)
        );
        assert!(
            h.root_mut::<NavShell>()
                .window_toolbar_event(&ToolbarEvent::Action {
                    id: super::window_toolbar::WINDOW_BACK.into()
                })
        );
        settle(&mut h, 560, 800);
        assert_ne!(
            h.root::<NavShell>().front_pane(),
            front,
            "the bar's Back pops the pane in front"
        );
    }

    // A shown title that says something else (an item's title as it is typed) is the same part,
    // measured and painted again: the window keeps its tree. A count is drawn in that part too.
    // A header's actions that each take their change in place (a label, whether one is enabled)
    // keep the row as it is; an action that comes or goes changes it.
}

tilo_test_env::scenario! {
    fn the_shell_keeps_its_tree_and_remeasures_what_changed;

    // Before any layout the shell builds its tree for the screens a regular window shows.
    phase the_shell_continues_and_reports_input_as_its_content;
    {
        let shell = NavShell::new()
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .content(Box::new(ResumableContent(crate::widget::Spacer::new())));
        let mut h = harness(shell, 900, 600, Theme::dark());
        settle(&mut h, 900, 600);
        let document = automerge::AutoCommit::new();
        let metadata =
            crate::widget::capability::capability::<dyn crate::widget::Continuable>(h.root_widget())
                .and_then(|continuable| continuable.continuation_metadata(&document, &automerge::ROOT))
                .expect("the content's continuation");
        assert_eq!(metadata.bytes, b"line 3");
        assert!(
            crate::widget::capability::capability_mut::<dyn crate::widget::Continuable>(
                &mut *h.root_mut::<NavShell>()
            )
            .is_some()
        );
        assert_eq!(
            h.native_input_snapshot(),
            Some(serde_json::json!({ "caret_line": 3 }))
        );
    }

    // A change a part carries out in place is the shell's only where the shell can have it carried
    // out: in a screen it lays out, in front or not, it is remounted there; a screen outside the
    // tree is built whole when it comes back.
}

#[test]
fn narrowing_keeps_the_list_an_app_showed_and_its_actions_answer() {
    let mut h = harness(mail_shell(), 1600, 800, Theme::dark());
    settle(&mut h, 1600, 800);
    h.root_mut::<NavShell>().show(NavPane::Overview);
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    let list = geometry(&h).overview_column;
    assert!(list.width() > 0.0 && list.x0 >= 0.0, "{list:?}");
    let point = list.center();
    h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
    h.dispatch_to_root(&crate::synthetic_input::mouse_press());
    h.dispatch_to_root(&crate::synthetic_input::mouse_release());
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);

    // Otherwise the page the app opened stays in front.
    let mut h = harness(mail_shell(), 1600, 800, Theme::dark());
    settle(&mut h, 1600, 800);
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
}

#[test]
fn the_navigation_list_keeps_its_typed_identity_and_selected_activation_opens_content() {
    let chosen = SharedRc::new(std::cell::RefCell::new(Vec::new()));
    let seen = SharedRc::clone(&chosen);
    let list = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
        "places",
        vec![crate::widget::NavItem::new("inbox", "Inbox")],
    )])
    .selected("inbox")
    .on_choose("places", move |id| seen.borrow_mut().push(id));
    assert!(
        Widget::as_any(&list)
            .unwrap()
            .is::<crate::widget::NavList>()
    );
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Box::new(list))
        .content(TestLeaf::fill("content").boxed().0);
    let mut h = harness(shell, 360, 800, Theme::dark());
    settle(&mut h, 360, 800);
    assert_eq!(
        h.root_mut::<NavShell>()
            .sidebar_mut::<crate::widget::NavList>()
            .expect("typed NavList access through the shared decorator")
            .selected_id()
            .as_deref(),
        Some("inbox")
    );
    h.relayout_at((360, 800))
        .expect("typed inspection republished");
    let inbox = h
        .a11y_node_named("Inbox")
        .expect("published navigation row");
    h.perform_a11y_action(inbox, &crate::a11y::Action::Activate)
        .expect("activating the already-selected destination");
    settle(&mut h, 360, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
    assert_eq!(
        chosen.borrow().as_slice(),
        ["inbox"],
        "activation answers through the view action, as a press does"
    );
}

#[test]
fn push_navigation_keeps_back_operable_before_the_incoming_screen_is_visible() {
    let list = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
        "places",
        vec![crate::widget::NavItem::new("inbox", "Inbox")],
    )])
    .selected("inbox");
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Box::new(list))
        .content(TestLeaf::fill("content").boxed().0);
    let mut h = harness(shell, 360, 800, Theme::dark());
    settle(&mut h, 360, 800);
    let outgoing = h.a11y_node_named("Inbox").expect("initial navigation row");
    {
        let mut shell = h.root_mut::<NavShell>();
        shell.show(NavPane::Content);
        shell.inputs.borrow_mut().push_pos = Some(0.0);
    }
    h.relayout_at((360, 800)).expect("incoming page frame");
    assert!(geometry(&h).side_by_side, "no overview splits the content");
    assert!(
        h.a11y_node_named("Inbox").is_none(),
        "outgoing navigation rows are inert even while their pixels remain visible"
    );
    assert!(
        h.perform_a11y_action(outgoing, &crate::a11y::Action::Activate)
            .is_err(),
        "a stale AT target cannot activate the outgoing screen"
    );
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
    let back = h
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.label() == Some("Back")
                && node.role() == crate::accesskit::Role::Button
                && node.supports_action(crate::accesskit::Action::Click)
        })
        .expect("Back remains exported during the transition");
    assert!(geometry(&h).back.x0 >= 0.0 && geometry(&h).back.x1 <= 360.0);
    h.perform_a11y_action(back.0, &crate::a11y::Action::Activate)
        .expect("Back cancels the in-flight navigation through AT");
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
}

/// A notice's button stands in the window's bar; pressing it there reports
/// the notice once and takes it down.
#[test]
fn a_notices_button_is_the_window_bars_and_a_press_reports_it() {
    use crate::window_toolbar::{ToolbarEvent, notice, notify};
    notify(None);
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let mut shell = h.root_mut::<NavShell>();
    shell.toast_with_action("Deleted Groceries", ToastKind::Info, "undo-2", "Undo");
    let shown = notice().expect("the bar shows the notice");
    let button = shown.actions.into_iter().next().expect("with its button");
    assert_eq!(button.name, "Undo");
    crate::window_toolbar::deliver(ToolbarEvent::Action { id: button.id });
    assert!(
        crate::window_toolbar::take_events().is_empty(),
        "the press is the notice's, not among the app's part events"
    );
    drop(shell);
    settle(&mut h, 1280, 800);
    let mut shell = h.root_mut::<NavShell>();
    assert_eq!(
        shell.take_toast_action().as_deref(),
        Some("Deleted Groceries"),
        "the press names the notice"
    );
    assert_eq!(shell.take_toast_action(), None, "once");
    assert_eq!(shell.toast_text(), None, "and it goes");
    assert_eq!(notice(), None, "from the bar too");
}

/// A window that builds its shell anew keeps the notice it had up: what a shell holds is shown
/// again in the shell built in its place, with its button, until it leaves on its own.
#[test]
fn a_shell_built_in_place_shows_the_notice_the_one_before_it_held() {
    use crate::window_toolbar::{notice, notify};
    notify(None);
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().toast_with_action(
        "Deleted Groceries",
        ToastKind::Info,
        "undo-2",
        "Undo",
    );
    let held = h
        .root_mut::<NavShell>()
        .held_notice()
        .expect("the shell holds the notice it shows");
    // The shell built in its place shows what the one before it held.
    let mut rebuilt = NavShell::new();
    rebuilt.restore_notice(held);
    assert_eq!(rebuilt.toast_text(), Some("Deleted Groceries"));
    let shown = notice().expect("the bar shows it again");
    assert_eq!(shown.text, "Deleted Groceries");
    assert_eq!(
        shown.actions.first().map(|action| action.name.as_str()),
        Some("Undo"),
        "with the button it offered"
    );
    assert!(rebuilt.held_notice().is_some(), "still up, not consumed");
}

/// One frame of the shell as a host presents it at `scale`: its widgets and
/// overlays painted magnified, and the damage it reports, in the host's
/// scene pixels as a host repaints it (rounded out by a pixel).
fn present_shell(
    shell: &mut NavShell,
    text: &mut TextRenderer,
    theme: &Theme,
    scale: f64,
) -> (Scene, Vec<Rect>) {
    let mut scene = Scene::new();
    let presence = crate::presence::PresenceCtx::empty();
    let transform = Affine::scale(scale);
    shell.draw(&mut crate::draw::DrawCtx::new(
        &mut scene,
        transform,
        text,
        theme,
        presence.draw_env(),
    ));
    shell.draw_overlay(&mut crate::draw::DrawCtx::new(
        &mut scene,
        transform,
        text,
        theme,
        presence.draw_env(),
    ));
    let mut damage = Vec::new();
    shell.collect_damage(&mut damage);
    shell.mark_drawn();
    let damage = damage
        .into_iter()
        .map(|rect| transform.transform_rect_bbox(rect).inflate(1.0, 1.0))
        .collect();
    (scene, damage)
}

fn has_child(h: &Driver, id: &str) -> bool {
    h.root::<NavShell>()
        .children()
        .into_iter()
        .any(|child| child.test_id() == Some(id))
}

fn notes_shell_with_search() -> NavShell {
    let actions: Vec<Element> = vec![
        Box::new(Button::icon_only(
            "note-menu",
            crate::icons::Icon::MoreHorizontal,
            "Note actions",
        )),
        Box::new(crate::widget::search_request::search_button(
            crate::widget::search_request::SearchRequest::this_app(),
        )),
    ];
    NavShell::new()
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .sidebar_header("Notes", Vec::new())
        .content_header("", actions)
}

fn sidebar_resize_control(h: &Driver) -> Option<crate::headless::Control> {
    h.controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some("Resize sidebar"))
}

#[test]
fn the_sidebar_resize_edge_is_an_accessible_separator() {
    let mut h = harness(mail_shell(), 1400, 800, Theme::dark());
    settle(&mut h, 1400, 800);
    let resize = sidebar_resize_control(&h).expect("the docked sidebar's edge is a control");
    h.operate_control(
        &resize.id,
        &crate::headless::ControlInput::SetValue("25%".into()),
    )
    .expect("set-value resizes the sidebar");
    settle(&mut h, 1400, 800);
    let width = geometry(&h).sidebar_column.width();
    assert!((width - 350.0).abs() < 1.0, "laid out width {width}");
    assert!(
        h.operate_control(
            &resize.id,
            &crate::headless::ControlInput::SetValue("NaN%".into())
        )
        .is_err()
    );
    settle(&mut h, 1400, 800);
    assert_eq!(geometry(&h).sidebar_column.width(), width);

    settle(&mut h, 400, 800);
    assert!(
        sidebar_resize_control(&h).is_none(),
        "a sidebar that is not docked has no resize edge"
    );
}

#[test]
fn a_focused_sidebar_resize_edge_follows_the_arrow_keys_until_a_press() {
    let mut h = harness(mail_shell(), 1400, 800, Theme::dark());
    settle(&mut h, 1400, 800);
    let resize = sidebar_resize_control(&h).expect("the docked sidebar's edge is a control");
    h.operate_control(&resize.id, &crate::headless::ControlInput::Focus)
        .expect("focus the edge");
    let before = geometry(&h).sidebar_column.width();
    h.key("ArrowRight");
    settle(&mut h, 1400, 800);
    let wider = geometry(&h).sidebar_column.width();
    assert!(wider > before, "{wider} after {before}");

    let content = geometry(&h).content_column.center();
    h.click_at(content.x, content.y);
    h.key("ArrowRight");
    settle(&mut h, 1400, 800);
    assert_eq!(geometry(&h).sidebar_column.width(), wider);
}

struct ResumableContent(crate::widget::Spacer);

impl crate::widget::Decorator for ResumableContent {
    type Inner = crate::widget::Spacer;

    fn inner(&self) -> &crate::widget::Spacer {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut crate::widget::Spacer {
        &mut self.0
    }

    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn crate::widget::Continuable>() {
            request.provide::<dyn crate::widget::Continuable>(self);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn crate::widget::Continuable>() {
            request.provide::<dyn crate::widget::Continuable>(self);
        }
    }

    fn native_input_snapshot(
        &self,
        _state: &AppModel,
        _path: &ModelPath,
    ) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "caret_line": 3 }))
    }
}

impl crate::widget::Continuable for ResumableContent {
    fn continuation_metadata(
        &self,
        _state: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Option<::collaboration::ContinuationMetadata> {
        Some(::collaboration::ContinuationMetadata {
            schema_version: 1,
            bytes: b"line 3".to_vec(),
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        _metadata: &::collaboration::ContinuationMetadata,
        _state: &mut crate::widget::Edits<'_>,
        _path: &automerge::ObjId,
    ) -> bool {
        true
    }
}
