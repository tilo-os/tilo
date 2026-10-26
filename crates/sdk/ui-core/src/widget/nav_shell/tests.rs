use super::*;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::widget::Text;
use std::cell::Cell;
use std::rc::Rc as SharedRc;

mod adaptive;
mod band;
mod capabilities;
mod hosted;
mod inspector_sheet;
mod nothing_listed;

/// A pane that fills what it is given and counts the input it receives.
struct Probe {
    id: &'static str,
    deadline: Option<Instant>,
    animating: bool,
    rect: Rect,
    events: SharedRc<Cell<usize>>,
}

impl Probe {
    fn boxed(id: &'static str) -> (Element, SharedRc<Cell<usize>>) {
        let events = SharedRc::new(Cell::new(0));
        (
            Box::new(Self {
                id,
                deadline: None,
                animating: false,
                rect: Rect::ZERO,
                events: SharedRc::clone(&events),
            }),
            events,
        )
    }
}

impl Widget for Probe {
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
        self.animating = false;
    }

    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
        self.events.set(self.events.get().saturating_add(1));
    }

    fn needs_animation_frame(&self) -> bool {
        self.animating
    }

    fn needs_redraw(&self) -> bool {
        self.animating
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn is_layout_dirty(&self) -> bool {
        // This fixture fills a static rectangle and has no intrinsic state.
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Probe"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id)
    }
}

fn mail_shell() -> NavShell {
    NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Mailboxes", Vec::new())
        .overview_header("Inbox", Vec::new())
        .content_header("Design review", Vec::new())
}

fn harness(shell: NavShell, w: u32, h: u32, theme: Theme) -> FrameHarness {
    FrameHarness::for_root(State::new(), shell, (w, h), theme)
}

fn settle(h: &mut FrameHarness, w: u32, height: u32) {
    h.root_mut::<NavShell>().snap();
    // A resize that switches the layout starts its motion; land it.
    h.relayout_at((w, height)).expect("layout settles");
    h.root_mut::<NavShell>().snap();
    h.relayout_at((w, height)).expect("layout settles");
    h.relayout_at((w, height)).expect("layout settles");
}

fn geometry(h: &FrameHarness) -> NavGeometry {
    h.root::<NavShell>().geometry().clone()
}

fn child_rect(h: &FrameHarness, id: &str) -> Option<Rect> {
    h.root::<NavShell>()
        .children()
        .into_iter()
        .find(|child| child.test_id() == Some(id))
        .map(Widget::get_rect)
}

fn notes_shell() -> NavShell {
    NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Notes", Vec::new())
        .content_header("", Vec::new())
}

fn files_shell() -> NavShell {
    NavShell::new()
        .content_kind(ContentKind::List)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Places", Vec::new())
        .content_header("Home", Vec::new())
}

fn settings_shell() -> NavShell {
    NavShell::new()
        .content_kind(ContentKind::Page)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Settings", Vec::new())
        .content_header("Appearance", Vec::new())
}

fn document_shell() -> NavShell {
    notes_shell().content_kind(ContentKind::Document)
}

/// At any width the shell picks the layout in which everything shown is
/// usable: docked while the sidebar fits beside the content's real minimum,
/// one column at a time below that, and a drawer only for a content-primary
/// app. No automatic width ever darkens the content.
#[test]
fn the_sidebar_docks_while_the_contents_real_minimum_fits() {
    let theme = Theme::dark();
    let m = Metrics::resolve(&theme);
    use SidebarPlacement::{Docked, Drawer, Screen};
    for (name, make, min, narrow) in [
        ("files", files_shell as fn() -> NavShell, m.list_min, Screen),
        ("settings", settings_shell, m.page_min, Screen),
        ("notes", document_shell, m.reading_min, Drawer),
    ] {
        for width in [320_u32, 360, 480, 520, 560, 600, 720, 840, 900, 1400] {
            let mut h = harness(make(), width, 700, theme.clone());
            settle(&mut h, width, 700);
            let g = geometry(&h);
            let w = f64::from(width);
            assert!(
                (g.breakpoint - (m.sidebar_min + min)).abs() < 1e-6,
                "{name} {width}: the sidebar at its minimum plus the content's"
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
                // A new window opens on the first list of items: Files'
                // folder, Settings' categories.
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
                    assert!(g.back.width() > 0.0, "{name} {width}: Back to the places");
                }
            } else {
                assert_eq!(
                    content.map(|r| r.width()),
                    Some(w),
                    "{name} {width}: the document fills the window"
                );
            }
            // No automatic width darkens anything: nothing is modal.
            assert_eq!(g.open_drawer(), None, "{name} {width}");
        }
    }
    // A docked Files sidebar works down to 600 instead of 840.
    assert!(m.sidebar_min + m.list_min <= 600.0);
}

/// The narrowest window is the compact layout's: one column at a time at
/// phone width, the content's minimum where that is wider.
#[test]
fn the_minimum_window_width_is_the_compact_layouts() {
    for theme in [Theme::dark(), Theme::dark().with_text_scale(2.0)] {
        let m = Metrics::resolve(&theme);
        for (make, min) in [
            (files_shell as fn() -> NavShell, m.list_min),
            (settings_shell, m.page_min),
            (document_shell, m.reading_min),
            (mail_shell, m.page_min),
        ] {
            let mut h = harness(make(), 1280, 700, theme.clone());
            settle(&mut h, 1280, 700);
            let shell = h.root::<NavShell>();
            let expected = min.max(m.compact_min);
            assert!((shell.minimum_width() - expected).abs() < 1e-6);
            assert_eq!(
                crate::widget::minimum_window_width(shell),
                Some(shell.minimum_width()),
                "the host reads it from the tree"
            );
            assert!(
                shell.minimum_width() <= 360.0 * f64::from(theme.text_growth()),
                "phone width"
            );
            let width = shell.minimum_width().ceil();
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small positive width"
            )]
            let px = width as u32;
            settle(&mut h, px, 700);
            let g = geometry(&h);
            assert_ne!(g.sidebar, SidebarPlacement::Docked);
            assert_eq!(g.open_drawer(), None);
        }
    }
}

/// A window resized near a threshold does not flicker: back to docked only
/// `layout_hysteresis` past it.
#[test]
fn layout_changes_have_hysteresis() {
    let theme = Theme::dark();
    let m = Metrics::resolve(&theme);
    let threshold = m.sidebar_min + m.list_min;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "small positive widths"
    )]
    let px = |w: f64| w.round() as u32;
    let mut h = harness(files_shell(), 1280, 700, theme);
    settle(&mut h, 1280, 700);
    let at = |h: &mut FrameHarness, w: f64| {
        settle(h, px(w), 700);
        geometry(h).sidebar
    };
    assert_eq!(at(&mut h, threshold), SidebarPlacement::Docked);
    assert_eq!(at(&mut h, threshold - 1.0), SidebarPlacement::Screen);
    assert_eq!(
        at(&mut h, threshold + m.hysteresis - 1.0),
        SidebarPlacement::Screen,
        "not back until past the band"
    );
    assert_eq!(
        at(&mut h, threshold + m.hysteresis),
        SidebarPlacement::Docked
    );
    assert_eq!(
        at(&mut h, threshold),
        SidebarPlacement::Docked,
        "docked holds down to the threshold"
    );
}

/// A window opened narrow has no place to keep yet, so it opens on a list,
/// whole screen: the content's own list where it has one, else the sidebar.
/// Never the content, which nothing has chosen.
#[test]
fn a_fresh_narrow_window_opens_on_its_list() {
    // A mail window: mailboxes, the inbox, a message.
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
    // A settings window: a list of pages and a page.
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .content_header("General", Vec::new());
    let mut h = harness(shell, 360, 700, Theme::dark());
    settle(&mut h, 360, 700);
    let g = geometry(&h);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    assert_eq!(g.sidebar_column.width(), 360.0, "the pages fill the screen");
}

/// Narrowing a docked window keeps the person's place: they land on the
/// page they were on, Back goes to the list, and widening docks again with
/// the same page open.
#[test]
fn the_open_page_is_kept_across_docked_push_and_docked() {
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
        .content(Probe::boxed("content").0)
        .content_header("Appearance", Vec::new());
    let mut h = harness(shell, 1280, 700, Theme::dark());
    settle(&mut h, 1280, 700);
    // The person works in the page.
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

/// A live resize across the docking threshold moves the columns from where
/// they stood instead of jumping: narrowing, the page slides from beside the
/// sidebar to fill the window; widening, the sidebar slides back in.
#[test]
fn a_resize_across_the_threshold_moves_the_columns_from_where_they_stood() {
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .content_header("Appearance", Vec::new());
    let mut h = harness(shell, 1280, 700, Theme::dark());
    settle(&mut h, 1280, 700);
    let docked = geometry(&h);
    assert_eq!(docked.sidebar, SidebarPlacement::Docked);
    let point = docked.content_column.center();
    h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
    h.dispatch_to_root(&crate::synthetic_input::mouse_press());
    let docked_x = geometry(&h).content_column.x0;
    assert!(docked_x > 0.0);

    // Narrower than the sidebar beside the page's minimum.
    h.relayout_at((400, 700)).expect("narrow");
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Screen);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
    assert!(
        g.content_column.x0 > docked_x / 2.0 && g.content_column.x0 <= docked_x + 0.5,
        "the page sets out from where it stood beside the sidebar: {} vs {docked_x}",
        g.content_column.x0
    );
    assert!(h.root::<NavShell>().needs_animation_frame(), "and slides");
    settle(&mut h, 400, 700);
    assert_eq!(geometry(&h).content_column.x0, 0.0, "to fill the window");

    // Wider again: the sidebar the person was not looking at slides in.
    h.relayout_at((1280, 700)).expect("wide");
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Docked);
    assert!(
        g.sidebar_column.width() < docked.sidebar_column.width(),
        "the sidebar slides in rather than appearing"
    );
    assert!(h.root::<NavShell>().needs_animation_frame());
    settle(&mut h, 1280, 700);
    assert_eq!(geometry(&h).sidebar_column, docked.sidebar_column);
}

/// Under reduced motion nothing slides across a layout switch: the page
/// stands in its new place at once and fades in there.
#[test]
fn under_reduced_motion_a_layout_switch_fades_instead_of_sliding() {
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0);
    let mut theme = Theme::dark();
    theme.reduce_motion = true;
    let mut h = harness(shell, 1280, 700, theme);
    settle(&mut h, 1280, 700);
    let point = geometry(&h).content_column.center();
    h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
    h.dispatch_to_root(&crate::synthetic_input::mouse_press());
    h.relayout_at((400, 700)).expect("narrow");
    let shell = h.root::<NavShell>();
    assert_eq!(shell.geometry().content_column.x0, 0.0, "no slide");
    assert!(
        shell.switch_fade.progress(crate::time::now()) < 1.0,
        "the page fades in"
    );
    assert!(shell.switch_columns[Column::Content.index()]);
    assert!(shell.needs_animation_frame());
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
        .content(Probe::boxed("content").0);
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

#[test]
fn the_shell_follows_the_window_and_the_text_scale() {
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
            let pair = m.overview + m.page_min;
            let breakpoint = m.sidebar_min + pair;
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
                assert!((g.sidebar_column.width() - m.sidebar.min(w - pair)).abs() < 0.5);
                assert_eq!(g.sidebar_column.x0, 0.0);
            } else {
                // A fresh window has no place to keep: it opens on the
                // first list, whole screen (the inbox where the content has
                // its own list, else the sidebar).
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
            // The toggle is top-leading, one inset in; a pushed screen has
            // Back there instead, and the list screen neither.
            let front = h.root::<NavShell>().front_pane();
            if g.stack.is_empty() {
                assert!((g.toggle.x0 - m.inset).abs() < 0.5, "{scale}x {width}");
                assert!(g.toggle.y0 > 0.0 && g.toggle.y1 < g.header_height);
            } else if front == NavPane::Sidebar {
                assert_eq!(g.back, Rect::ZERO);
            } else {
                assert!(g.back.width() > 0.0);
            }
        }
    }
}

#[test]
fn a_docked_sidebar_is_never_kept_beside_squeezed_columns() {
    let mut h = harness(mail_shell(), 800, 600, Theme::dark());
    settle(&mut h, 800, 600);
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Screen);
    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, 800, 600);
    let g = geometry(&h);
    assert!(g.side_by_side);
    let content = child_rect(&h, "content").expect("content laid out");
    let m = Metrics::resolve(&Theme::dark());
    assert!(
        content.width() >= m.page_min - 0.5,
        "the detail keeps its minimum ({})",
        content.width()
    );
}

#[test]
fn the_sidebar_toggle_moves_and_the_choice_is_remembered_per_size_class() {
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

    // Narrow: one column at a time; the list is a screen of its own.
    settle(&mut h, 560, 800);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Screen);
    settle(&mut h, 1280, 800);
    assert!(
        !h.root::<NavShell>().is_sidebar_open(),
        "the wide window remembers the sidebar was hidden"
    );
    // Showing it again docks it, and the content reflows.
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

#[test]
fn a_toolbar_in_a_header_behind_the_drawer_settles() {
    // The headers under an open drawer are laid out hidden; a toolbar among
    // their actions takes that placement instead of holding its own height
    // against it, so the layout settles.
    let toolbar = || -> Element {
        Box::new(
            crate::widget::Toolbar::new(crate::widget::toolbar::Orientation::Horizontal)
                .push_icon(crate::icons::Icon::Search, "Search", || {})
                .push_icon(crate::icons::Icon::Refresh, "Refresh", || {}),
        )
    };
    let shell = NavShell::new()
        .content_kind(ContentKind::Document)
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Mailboxes", Vec::new())
        .overview_header("Inbox", vec![toolbar()])
        .content_header("", vec![toolbar()]);
    let mut h = harness(shell, 560, 860, Theme::dark());
    settle(&mut h, 560, 860);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Drawer);
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 560, 860);
    assert!(h.root::<NavShell>().is_sidebar_open());
}

/// A content-primary app's drawer floats over the document without a scrim:
/// the document stays visible and exposed, and a press on it puts the
/// drawer away without reaching it.
#[test]
fn an_open_drawer_floats_over_the_content_without_a_scrim() {
    let (content, content_events) = Probe::boxed("content");
    let shell = NavShell::new()
        .content_kind(ContentKind::Document)
        .sidebar(Probe::boxed("sidebar").0)
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

    // A press on the document beside it puts the drawer away, and only that.
    let seen = content_events.get();
    let point = Point::new(g.sidebar_column.x1 + 40.0, 300.0);
    h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
    let handled = h.dispatch_to_root(&crate::synthetic_input::mouse_press());
    assert!(handled);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert_eq!(
        content_events.get(),
        seen + 1,
        "the document heard the pointer move, not the dismissing press"
    );
}

#[test]
fn narrow_windows_push_the_content_over_the_overview() {
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
    assert!(
        g.back.width() > 0.0,
        "the overview goes back to the mailboxes"
    );
    assert!(h.root_mut::<NavShell>().back());
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    assert_eq!(
        geometry(&h).back,
        Rect::ZERO,
        "the first screen has no Back"
    );

    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, 560, 800);
    let g = geometry(&h);
    assert!(g.back.width() > 0.0, "a pushed screen has a Back button");
    assert_eq!(g.toggle, Rect::ZERO, "one affordance holds the corner");
    assert_eq!(g.content_column.x0, 0.0);
    assert_eq!(g.overview_column, Rect::ZERO, "the overview has slid away");

    // Escape goes back.
    let esc =
        crate::synthetic_input::key_input(&Key::Named(NamedKey::Escape), ElementState::Pressed);
    assert!(h.dispatch_to_root(&esc));
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
}

#[test]
fn push_mode_makes_the_sidebar_the_first_screen() {
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0);
    let mut h = harness(shell, 360, 800, Theme::dark());
    settle(&mut h, 360, 800);
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Screen);
    assert_eq!(g.stack, [NavPane::Sidebar, NavPane::Content]);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    assert_eq!(g.toggle, Rect::ZERO, "a sidebar screen has no toggle");
    assert_eq!(child_rect(&h, "sidebar").map(|r| r.width()), Some(360.0));

    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, 360, 800);
    assert!(geometry(&h).back.width() > 0.0);
    assert!(h.root_mut::<NavShell>().back());
    settle(&mut h, 360, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);

    // Wide, it docks like any sidebar.
    settle(&mut h, 1280, 800);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Docked);
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
        .content(Probe::boxed("content").0);
    let mut h = harness(shell, 360, 800, Theme::dark());
    settle(&mut h, 360, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    let g = geometry(&h);
    let row = crate::chrome::nav_row_height(&Theme::dark());
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

#[test]
fn the_system_shortcut_toggles_the_sidebar() {
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

/// Frames as a window's host draws them: the one an input asked for, then
/// more only while the shell asks.
fn pump(h: &mut FrameHarness, w: u32, height: u32) {
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

    // A press whose next frame comes after the slide's whole time.
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
fn the_toggle_names_what_it_does_for_the_palette_and_agents() {
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let name = |h: &FrameHarness| {
        h.root::<NavShell>()
            .toggle
            .accessibility()
            .and_then(|node| node.name)
    };
    assert_eq!(name(&h).as_deref(), Some("Hide sidebar"));
    h.root_mut::<NavShell>().toggle_sidebar();
    settle(&mut h, 1280, 800);
    assert_eq!(name(&h).as_deref(), Some("Show sidebar"));
    let doc = automerge::AutoCommit::new();
    let resources = crate::resources::resources(&h.root::<NavShell>().toggle, &doc);
    assert!(
        resources
            .iter()
            .any(|resource| resource.label == "Show sidebar"),
        "the toggle publishes its action"
    );
}

#[test]
fn dragging_the_trailing_edge_resizes_and_below_its_minimum_hides() {
    let mut h = harness(mail_shell(), 1400, 800, Theme::dark());
    settle(&mut h, 1400, 800);
    let g = geometry(&h);
    let edge = g.resize_handle.center();
    let device = winit::event::DeviceId::dummy();
    let move_to = |h: &mut FrameHarness, x: f64| {
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

#[test]
fn the_inspector_docks_beside_the_content_and_rises_as_a_sheet_when_narrow() {
    let shell = NavShell::new()
        .content(Probe::boxed("content").0)
        .inspector(Probe::boxed("inspector").0);
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

#[test]
fn a_column_without_a_shell_has_no_header_row() {
    let shell = NavShell::new().content(Probe::boxed("content").0);
    let mut h = harness(shell, 1024, 700, Theme::dark());
    settle(&mut h, 1024, 700);
    let g = geometry(&h);
    assert_eq!(g.header_height, 0.0);
    assert_eq!(
        child_rect(&h, "content"),
        Some(Rect::new(0.0, 0.0, 1024.0, 700.0))
    );
}

/// A header row stands over a column only while that column's header
/// holds something: beside a docked sidebar's titled header, an empty
/// content header leaves the content at the window's top, and the row
/// comes back over it once the header has a title.
#[test]
fn an_empty_content_header_holds_no_row_beside_the_sidebars() {
    for (title, holds) in [("", false), ("Draft", true)] {
        let mut shell = notes_shell().content_header(title, Vec::new());
        shell.set_sidebar_toggle_in_window_bar(true);
        let mut h = harness(shell, 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert_eq!(g.sidebar, SidebarPlacement::Docked);
        assert!(
            g.sidebar_header().height() > 0.0,
            "{title:?}: the sidebar keeps its header"
        );
        let content = child_rect(&h, "content").expect("the content is laid out");
        if holds {
            assert!((g.content_header().height() - g.header_height).abs() < 1e-9);
            assert!(
                (content.y0 - g.header_height).abs() < 1e-9,
                "{title:?}: the content starts under its header"
            );
        } else {
            assert_eq!(
                g.content_header(),
                Rect::ZERO,
                "{title:?}: no content header"
            );
            assert!(
                content.y0.abs() < 1e-9,
                "{title:?}: the content starts at the top, at {}",
                content.y0
            );
        }
    }
}

#[test]
fn history_is_one_glyph_in_the_window_top_trailing_corner() {
    // Case `only_where_the_host_gives_the_shell_history`.
    {
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        assert_eq!(geometry(&h).history, Rect::ZERO);
        assert!(child_rect(&h, "nav-shell-history").is_none());
    }

    // Case `closed_it_stands_at_the_trailing_edge_of_the_row`.
    {
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert!(g.history.width() > 0.0);
        assert!(g.history.x1 > 1280.0 - 24.0 && g.history.x1 <= 1280.0);
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

    // Case `open_it_reads_as_on`.
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

    // Case `the_inspector_toggle_stands_just_before_it`.
    {
        let shell = NavShell::new()
            .content(Probe::boxed("content").0)
            .inspector(Probe::boxed("inspector").0);
        let mut h = harness(shell, 1280, 800, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert!(g.inspector_toggle.width() > 0.0);
        assert!(g.inspector_toggle.x1 < g.history.x0, "{g:?}");
        assert!((g.inspector_toggle.y0 - g.history.y0).abs() < 0.5);
    }

    // Case `a_shell_without_headers_grows_a_row_for_it`.
    {
        let shell = NavShell::new().content(Probe::boxed("content").0);
        let mut h = harness(shell, 1024, 700, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        settle(&mut h, 1024, 700);
        let g = geometry(&h);
        assert!(g.header_height > 0.0);
        assert!(g.history.x1 > 1024.0 - 24.0);
    }

    // Case `a_narrow_push_stack_keeps_it_on_the_front_screen`.
    {
        let mut h = harness(mail_shell(), 560, 800, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        settle(&mut h, 560, 800);
        let g = geometry(&h);
        assert!(g.history.width() > 0.0 && g.history.x1 <= 560.0, "{g:?}");
    }
}

#[test]
fn a_status_the_person_can_act_on_has_its_button_beside_it_in_the_row() {
    const NOT_SAVED: &str = "Couldn't save this change to your history";
    let mut resource = tilo_actions::Resource::new("test.unsaved", "test.unsaved", NOT_SAVED);
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

    // Saying something else with no button takes the button away.
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

#[test]
fn sync_status_reads_as_quiet_text_before_the_rows_trailing_controls() {
    const OFFLINE: &str = "Working offline. Changes will sync when you're back.";
    let status = |h: &FrameHarness| {
        h.root::<NavShell>()
            .children()
            .into_iter()
            .find(|child| child.name() == "HeaderStatus")
            .map(|child| (child.get_rect(), child.accessibility()))
    };
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    let offline = crate::widget::WindowStatus::says(OFFLINE);
    assert!(h.root_mut::<NavShell>().set_sync_status(Some(&offline)));
    h.root_mut::<NavShell>()
        .set_history_control(Some(HistoryControl::Closed));
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    let (rect, node) = status(&h).unwrap();
    let node = node.unwrap();
    assert_eq!(node.name.as_deref(), Some(OFFLINE));
    assert_eq!(node.live, Some(crate::a11y::Politeness::Polite));
    assert!(rect.width() > 0.0, "painted in the row");
    assert!(
        rect.y1 <= g.header_height,
        "inside the header row, not a band"
    );
    assert!(
        rect.x1 <= g.history.x0,
        "{rect:?} ends before History {g:?}"
    );
    assert!(
        rect.x0 >= g.content_column.x0,
        "stays in the content column's row"
    );

    assert!(h.root_mut::<NavShell>().set_sync_status(None));
    settle(&mut h, 1280, 800);
    assert!(
        status(&h).and_then(|(_, node)| node).is_none(),
        "nothing to announce"
    );
}

#[test]
fn slides_take_the_motion_duration_and_reduced_motion_jumps() {
    let theme = Theme::dark();
    let mut h = harness(mail_shell(), 1280, 800, theme.clone());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().toggle_sidebar();
    h.relayout_at((1280, 800)).expect("layout");
    assert!(
        h.root::<NavShell>().needs_animation_frame(),
        "hiding the sidebar animates"
    );

    let mut calm = Theme::dark();
    calm.reduce_motion = true;
    let mut h = harness(mail_shell(), 1280, 800, calm);
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().toggle_sidebar();
    h.relayout_at((1280, 800)).expect("layout");
    h.relayout_at((1280, 800)).expect("layout");
    assert_eq!(
        geometry(&h).sidebar_column,
        Rect::ZERO,
        "reduced motion does not slide"
    );
}

#[test]
fn pressing_the_toggle_hides_and_shows_the_sidebar() {
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let toggle = geometry(&h).toggle.center();
    let press = |h: &mut FrameHarness| {
        h.click_at(toggle.x, toggle.y);
        h.relayout_at((1280, 800)).expect("layout");
    };
    press(&mut h);
    settle(&mut h, 1280, 800);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert_eq!(
        geometry(&h).toggle.center(),
        toggle,
        "the toggle did not move"
    );
    press(&mut h);
    settle(&mut h, 1280, 800);
    assert!(h.root::<NavShell>().is_sidebar_open());
}

#[test]
fn an_unbuilt_shell_reports_structure_changes_once() {
    let mut shell = notes_shell();
    shell.take_structure_dirty();
    for frame in 0..3 {
        assert!(
            !shell.take_structure_dirty(),
            "unbuilt frame {frame} must settle"
        );
    }
    shell.set_content(Probe::boxed("replacement").0);
    assert!(shell.take_structure_dirty());
    assert!(!shell.take_structure_dirty());
}

#[test]
fn an_open_drawer_keeps_the_contents_wake_deadline() {
    let deadline = crate::time::now() + crate::time::Duration::from_millis(900);
    let (mut content, _) = Probe::boxed("content");
    content
        .as_any_mut()
        .unwrap()
        .downcast_mut::<Probe>()
        .unwrap()
        .deadline = Some(deadline);
    let shell = NavShell::new()
        .content_kind(ContentKind::Document)
        .sidebar(Probe::boxed("sidebar").0)
        .content(content);
    let mut h = harness(shell, 480, 800, Theme::dark());
    settle(&mut h, 480, 800);
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 480, 800);
    assert_eq!(geometry(&h).open_drawer(), Some(Column::Sidebar));
    assert_eq!(h.root::<NavShell>().next_wake_deadline(), Some(deadline));
}

#[test]
fn a_drawer_leaves_the_contents_action_emitters_published() {
    let resource = tilo_actions::Resource::new("test-pane", "test", "Test").with_action(
        tilo_actions::ActionSpec::operation(
            "go",
            "Go",
            tilo_actions::OperationKind::View,
            "test.go",
            &(),
        )
        .unwrap(),
    );
    let button = Button::new("go", "Go")
        .on_action(tilo_actions::ActionBinding::unit_for(&resource, "go").unwrap());
    let shell = NavShell::new()
        .content_kind(ContentKind::Document)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Box::new(button));
    let mut h = harness(shell, 480, 800, Theme::dark());
    for open in [false, true, false] {
        if open {
            h.root_mut::<NavShell>().open_sidebar();
        } else {
            h.root_mut::<NavShell>().close_sidebar();
        }
        settle(&mut h, 480, 800);
        assert_eq!(h.root::<NavShell>().is_sidebar_open(), open);
        let bindings = crate::widget::controls::control_action_bindings(h.root::<NavShell>());
        let inventory = bindings
            .iter()
            .any(|binding| matches!(binding.kind, crate::widget::ControlBindingKind::Action(_)));
        let backdrop = bindings.iter().any(|binding| matches!(&binding.kind,
            crate::widget::ControlBindingKind::Action(address) if address.resource == "test-pane" && address.action == "go"));
        assert_eq!(
            crate::widget::controls::has_action_control_bindings(h.root::<NavShell>()),
            inventory
        );
        assert!(
            backdrop,
            "a drawer is not modal: the content's action stays published"
        );
    }
}

#[test]
fn narrowing_keeps_the_explicit_or_last_interacted_content_pane() {
    for clicked in [false, true] {
        let mut h = harness(mail_shell(), 1600, 800, Theme::dark());
        settle(&mut h, 1600, 800);
        assert!(geometry(&h).overview_column.width() > 0.0);
        if clicked {
            let point = geometry(&h).overview_column.center();
            h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
            h.dispatch_to_root(&crate::synthetic_input::mouse_press());
        } else {
            h.root_mut::<NavShell>().show(NavPane::Overview);
        }
        settle(&mut h, 560, 800);
        assert_eq!(
            h.root::<NavShell>().front_pane(),
            NavPane::Overview,
            "clicked: {clicked}"
        );
        assert!(geometry(&h).overview_column.width() > 0.0);
    }
}

#[test]
fn narrowing_keeps_the_list_an_app_showed_and_its_actions_answer() {
    // An app whose content is only a placeholder shows its list: narrowed,
    // the list stays in front rather than a blank screen pushed over it.
    let mut h = harness(mail_shell(), 1600, 800, Theme::dark());
    settle(&mut h, 1600, 800);
    h.root_mut::<NavShell>().show(NavPane::Overview);
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    let list = geometry(&h).overview_column;
    assert!(list.width() > 0.0 && list.x0 >= 0.0, "{list:?}");
    // A press on the list reaches the list, not a screen in front of it.
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
fn scrolling_reuses_measured_panes_without_reloading_fonts() {
    let content = crate::widget::Scrollable::new_vertical().with_child(Box::new(Text::new(
        &"file row with a measured label\n".repeat(100),
    )));
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Box::new(content));
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().mark_drawn();
    h.relayout_at((1280, 800)).expect("draw state republished");
    let text_rect = |h: &FrameHarness| {
        h.root::<NavShell>()
            .children()
            .into_iter()
            .find(|child| {
                child.concrete_type_name() == std::any::type_name::<crate::widget::Scrollable>()
            })
            .expect("retained scroll pane")
            .children()[0]
            .get_rect()
    };
    let before = text_rect(&h);
    let center = h.root::<NavShell>().geometry().content_column.center();
    h.dispatch_to_root(&crate::synthetic_input::cursor_moved(center.x, center.y));
    crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(|count| count.set(0));
    for _ in 0..4 {
        h.dispatch_to_root(&WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                0.0, -20.0,
            )),
            phase: winit::event::TouchPhase::Moved,
        });
        assert!(h.root::<NavShell>().is_layout_dirty());
        assert!(
            !h.root::<NavShell>().layout_needs_measure(),
            "scroll preserves intrinsic sizes: shell_pending={} dirty_parts={:?} font_constructions={}",
            h.root::<NavShell>().invalidation.is_pending(),
            h.root::<NavShell>()
                .host_parts
                .iter()
                .filter_map(|part| {
                    let widget = h.root::<NavShell>().part(*part)?;
                    widget.layout_needs_measure().then(|| {
                        (
                            *part,
                            widget.name(),
                            widget.get_rect(),
                            widget.is_layout_dirty(),
                        )
                    })
                })
                .collect::<Vec<_>>(),
            crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(Cell::get),
        );
        h.relayout_at((1280, 800)).expect("scroll repositioned");
    }
    let after = text_rect(&h);
    assert_eq!(after.x0, before.x0);
    assert_eq!(after.y0, before.y0 - 80.0);
    assert_eq!(
        crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(Cell::get),
        0,
        "translation-only layout must not reload system fonts"
    );
}

#[test]
fn retained_panes_remeasure_theme_resize_replacement_and_edits_beside_a_drawer() {
    use automerge::transaction::Transactable;
    fn make(theme: Theme, viewport: (u32, u32), label: &str) -> FrameHarness {
        let mut state = State::new();
        state.doc.put(automerge::ROOT, "label", label).unwrap();
        let content = crate::widget::Scrollable::new_vertical()
            .with_child(Box::new(Text::from_document_string("label")));
        FrameHarness::for_root(
            state,
            NavShell::new()
                .content_kind(ContentKind::Document)
                .sidebar(Probe::boxed("sidebar").0)
                .content(Box::new(content)),
            viewport,
            theme,
        )
    }
    fn text_rect(h: &mut FrameHarness) -> Rect {
        h.root_mut::<NavShell>()
            .content_mut::<crate::widget::Scrollable>()
            .unwrap()
            .children()[0]
            .get_rect()
    }
    let mut h = make(Theme::dark(), (1280, 800), "first line");
    settle(&mut h, 1280, 800);
    let original = text_rect(&mut h);
    let scaled = Theme::light().with_text_scale(1.5);
    crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(|count| count.set(0));
    h.relayout_themed((1400, 900), scaled.clone())
        .expect("theme and resize measured");
    assert!(text_rect(&mut h).height() > original.height());
    assert_eq!(
        crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(Cell::get),
        0,
        "panes reuse the host renderer rather than constructing font systems"
    );
    let mut fresh = make(scaled.clone(), (1400, 900), "first line");
    settle(&mut fresh, 1400, 900);
    assert_eq!(text_rect(&mut h), text_rect(&mut fresh));

    h.relayout_at((480, 800)).expect("narrow");
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 480, 800);
    assert_eq!(geometry(&h).open_drawer(), Some(Column::Sidebar));
    let before = text_rect(&mut h);
    h.state_mut()
        .doc
        .put(
            automerge::ROOT,
            "label",
            "first line\nsecond line\nthird line",
        )
        .unwrap();
    h.relayout_at((480, 800))
        .expect("edit beside the drawer measured");
    let changed = text_rect(&mut h);
    assert!(
        changed.height() > before.height(),
        "the document beside the drawer gets changed text geometry"
    );
    h.recompose().expect("authoritative full measurement");
    assert_eq!(text_rect(&mut h), changed);

    // A replacement already clean at the same scale still invalidates the
    // retained composition, even though its own intrinsic sizes are known.
    let mut replacement = make(
        scaled,
        (480, 800),
        "replacement\nsecond line\nthird line\nfourth line",
    );
    settle(&mut replacement, 480, 800);
    replacement.root_mut::<NavShell>().set_content(Box::new(
        crate::widget::Scrollable::new_vertical().with_child(Box::new(Text::new(
            "replacement\nsecond line\nthird line\nfourth line",
        ))),
    ));
    settle(&mut replacement, 480, 800);
    replacement.root_mut::<NavShell>().mark_drawn();
    let content = std::mem::replace(
        &mut replacement.root_mut::<NavShell>().content,
        Probe::boxed("placeholder").0,
    );
    assert!(
        !content.layout_needs_measure(),
        "fixture: replacement is already measured and clean"
    );
    h.root_mut::<NavShell>().set_content(content);
    h.relayout_at((480, 800))
        .expect("clean replacement measured");
    assert!(text_rect(&mut h).height() > changed.height());
}

#[test]
fn the_navigation_list_keeps_its_typed_identity_and_selected_activation_opens_content() {
    let changes = SharedRc::new(Cell::new(0));
    let seen = SharedRc::clone(&changes);
    let list = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
        "places",
        vec![crate::widget::NavItem::new("inbox", "Inbox")],
    )])
    .selected("inbox")
    .on_select(move |_| seen.set(seen.get() + 1));
    assert!(
        Widget::as_any(&list)
            .unwrap()
            .is::<crate::widget::NavList>()
    );
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Box::new(list))
        .content(Probe::boxed("content").0);
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
    // Mutable typed access retires the host's raw layout maps, even when
    // used only to inspect a value. Publish a fresh admitted frame for AT.
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
        changes.get(),
        0,
        "activation does not invent a selection change"
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
        .content(Probe::boxed("content").0);
    let mut h = harness(shell, 360, 800, Theme::dark());
    settle(&mut h, 360, 800);
    let outgoing = h.a11y_node_named("Inbox").expect("initial navigation row");
    {
        let shell = h.root_mut::<NavShell>();
        shell.show(NavPane::Content);
        // The first animation sample is still wholly on the outgoing page.
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

#[test]
fn a_hidden_inspector_animation_waits_until_the_drawer_is_painted() {
    let (mut inspector, _) = Probe::boxed("inspector");
    inspector
        .as_any_mut()
        .unwrap()
        .downcast_mut::<Probe>()
        .unwrap()
        .animating = true;
    let shell = NavShell::new()
        .content(Probe::boxed("content").0)
        .inspector(inspector);
    let mut h = harness(shell, 600, 534, Theme::dark());
    assert!(h.settle_for_capture(tilo_test_env::hang_bound()));
    let shell = h.root::<NavShell>();
    assert!(!shell.is_inspector_open());
    assert!(!shell.needs_animation_frame());
    assert!(!shell.needs_redraw());
    assert!(
        shell.inspector.as_ref().unwrap().needs_animation_frame(),
        "the hidden animation is retained"
    );
    h.root_mut::<NavShell>().open_inspector();
    assert!(h.settle_for_capture(tilo_test_env::hang_bound()));
    let shell = h.root::<NavShell>();
    assert!(shell.is_inspector_open());
    assert!(
        !shell.inspector.as_ref().unwrap().needs_animation_frame(),
        "opening the drawer paints the retained animation"
    );
}

/// A notice is the window bar's: it shows there in the location's place,
/// leaves on its own, and paints nothing over the window.
#[test]
fn a_toast_is_the_window_bars_notice_and_paints_nothing_over_the_window() {
    use crate::window_toolbar::{ToolbarNotice, notice, notify};
    notify(None);
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let theme = Theme::dark();
    let mut text = crate::font_catalog::fixture_renderer();
    let shell = h.root_mut::<NavShell>();
    let _ = present_shell(shell, &mut text, &theme, 1.0);
    let _ = present_shell(shell, &mut text, &theme, 1.0);
    shell.toast("Saved", ToastKind::Success);
    assert_eq!(
        notice(),
        Some(ToolbarNotice::new("Saved", ToastKind::Success)),
        "the window's bar shows the notice"
    );
    assert_eq!(shell.toast_text(), Some("Saved"));
    let (_, damage) = present_shell(shell, &mut text, &theme, 1.0);
    assert!(
        damage.is_empty(),
        "nothing is painted over the window: {damage:?}"
    );
    shell.dismiss_toast();
    assert_eq!(notice(), None, "taken down, it leaves the bar");
    assert_eq!(shell.toast_text(), None);
}

/// A notice's button stands in the window's bar; pressing it there reports
/// the notice once and takes it down.
#[test]
fn a_notices_button_is_the_window_bars_and_a_press_reports_it() {
    use crate::window_toolbar::{ToolbarEvent, notice, notify};
    notify(None);
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let shell = h.root_mut::<NavShell>();
    shell.toast_with_action("Deleted Groceries", ToastKind::Info, "undo-2", "Undo");
    let shown = notice().expect("the bar shows the notice");
    let button = shown.actions.into_iter().next().expect("with its button");
    assert_eq!(button.name, "Undo");
    crate::window_toolbar::deliver(ToolbarEvent::Action { id: button.id });
    assert!(
        crate::window_toolbar::take_events().is_empty(),
        "the press is the notice's, not among the app's slot events"
    );
    settle(&mut h, 1280, 800);
    let shell = h.root_mut::<NavShell>();
    assert_eq!(
        shell.take_toast_action().as_deref(),
        Some("Deleted Groceries"),
        "the press names the notice"
    );
    assert_eq!(shell.take_toast_action(), None, "once");
    assert_eq!(shell.toast_text(), None, "and it goes");
    assert_eq!(notice(), None, "from the bar too");
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
    let presence = PresenceCtx::empty();
    let transform = Affine::scale(scale);
    shell.draw(&mut scene, transform, text, theme, &presence);
    shell.draw_overlay(&mut scene, transform, text, theme, &presence);
    let mut damage = Vec::new();
    shell.collect_damage(&mut damage);
    shell.mark_drawn();
    let damage = damage
        .into_iter()
        .map(|rect| transform.transform_rect_bbox(rect).inflate(1.0, 1.0))
        .collect();
    (scene, damage)
}

/// Maximized, the window's controls join the header row at its trailing end:
/// one bar, the app's header and the window's controls side by side. The
/// shell's own trailing buttons and the app's actions stand before them.
#[test]
fn the_window_bar_merges_into_the_header_row() {
    let theme = Theme::dark();
    let width = crate::widget::merged_controls_width(&theme);
    let mut h = harness(mail_shell(), 1280, 800, theme);
    h.root_mut::<NavShell>()
        .set_history_control(Some(HistoryControl::Closed));
    assert!(
        h.root_mut::<NavShell>()
            .set_window_bar(Some(crate::widget::WindowBar {
                controls_width: width,
                revealed: false,
                top_inset: 0.0,
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
    let slot = h
        .root::<NavShell>()
        .window_bar_slot()
        .expect("the row reports its slot");
    assert_eq!(slot.controls, g.window_controls);
    assert!(!slot.hides_with_bar);
    assert_eq!(
        child_rect(&h, "content").map(|r| r.y0),
        Some(g.header_height),
        "the content stands below a permanent header"
    );

    // Restored: the row gives the room back.
    h.root_mut::<NavShell>().set_window_bar(None);
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    assert_eq!(g.window_controls, Rect::ZERO);
    assert!(g.history.x1 > 1280.0 - 24.0);
    assert!(h.root::<NavShell>().window_bar_slot().is_none());
}

/// An app that puts its header in the window's bar gives the whole window to
/// its content while maximized; the header comes back, over the content and
/// with the window's controls in its row, when the person reaches for the
/// bar, and the row is then the header's, never the content's under it.
#[test]
fn a_header_in_the_window_bar_hides_until_reached_for() {
    let theme = Theme::dark();
    let width = crate::widget::merged_controls_width(&theme);
    let (content, content_events) = Probe::boxed("content");
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(content)
        .sidebar_header("Notes", Vec::new())
        .content_header("", Vec::new())
        .with_header_in_window_bar();
    let mut h = harness(shell, 1280, 800, theme);
    let bar = |revealed| {
        Some(crate::widget::WindowBar {
            controls_width: width,
            revealed,
            top_inset: 0.0,
        })
    };

    // Tiled: an ordinary header above the content.
    settle(&mut h, 1280, 800);
    let tiled = geometry(&h);
    assert!(tiled.header_height > 0.0);
    assert_eq!(
        child_rect(&h, "content").map(|r| r.y0),
        Some(tiled.header_height)
    );

    // Maximized, at rest: no header, the content from the top.
    h.root_mut::<NavShell>().set_window_bar(bar(false));
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    assert_eq!(g.header_height, 0.0, "the header hides with the bar");
    assert_eq!(child_rect(&h, "content").map(|r| r.y0), Some(0.0));
    assert_eq!(g.toggle, Rect::ZERO, "no header buttons at rest");
    let slot = h
        .root::<NavShell>()
        .window_bar_slot()
        .expect("where the bar will stand");
    assert!(slot.hides_with_bar);
    assert_eq!(slot.row.height(), tiled.header_height);
    let before = content_events.get();
    h.click_at(700.0, 10.0);
    let hidden_delta = content_events.get() - before;
    assert!(
        hidden_delta >= 3,
        "at rest the top of the window is the content's"
    );

    // Reached for: the header over the content, the controls in its row.
    h.root_mut::<NavShell>().set_window_bar(bar(true));
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    assert_eq!(g.header_height, tiled.header_height);
    assert_eq!(
        child_rect(&h, "content").map(|r| r.y0),
        Some(0.0),
        "the content does not move: the header stands over it"
    );
    assert!(g.toggle.width() > 0.0, "the header's buttons are back");
    assert!(g.window_controls.width() > 0.0);
    assert!(g.window_controls.y1 <= g.header_height);
    let before = content_events.get();
    h.click_at(700.0, 10.0);
    let shown_delta = content_events.get() - before;
    assert!(
        shown_delta < hidden_delta,
        "a press on the shown bar never reaches the content under it \
         ({shown_delta} vs {hidden_delta} events)"
    );
    // The desktop's top bar shown over the window's top edge: the bar
    // stacks below it instead of hiding under it.
    h.root_mut::<NavShell>()
        .set_window_bar(Some(crate::widget::WindowBar {
            controls_width: width,
            revealed: true,
            top_inset: 36.0,
        }));
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    assert_eq!(g.window_row.y0, 36.0);
    assert_eq!(g.window_controls.y0, 36.0);
    assert!(
        g.toggle.y0 >= 36.0,
        "the header's buttons move down with it"
    );
    assert_eq!(child_rect(&h, "content").map(|r| r.y0), Some(0.0));
}

fn has_child(h: &FrameHarness, id: &str) -> bool {
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
        Box::new(crate::search::search_button(
            crate::search::SearchRequest::this_app(),
        )),
    ];
    NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Notes", Vec::new())
        .content_header("", actions)
}

#[test]
fn a_window_bar_that_presents_search_takes_the_headers_own_search_button_away() {
    // Case `told_before_the_first_build`.
    {
        let mut shell = notes_shell_with_search();
        assert!(WindowHeader::set_window_presents_search(&mut shell, true));
        let mut h = harness(shell, 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        assert!(!has_child(&h, crate::search::APP_SEARCH_BUTTON_ID));
        assert!(has_child(&h, "note-menu"), "the app's other actions stay");
    }

    // Case `told_after_it_was_shown`.
    {
        let mut h = harness(notes_shell_with_search(), 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        assert!(
            has_child(&h, crate::search::APP_SEARCH_BUTTON_ID),
            "outside a window bar the header keeps its search button"
        );
        assert!(WindowHeader::set_window_presents_search(
            h.root_mut::<NavShell>(),
            true
        ));
        settle(&mut h, 1280, 800);
        assert!(!has_child(&h, crate::search::APP_SEARCH_BUTTON_ID));
        assert!(has_child(&h, "note-menu"));
    }

    // Case `a_header_set_later_has_none_either`.
    {
        let mut h = harness(notes_shell_with_search(), 1280, 800, Theme::dark());
        WindowHeader::set_window_presents_search(h.root_mut::<NavShell>(), true);
        settle(&mut h, 1280, 800);
        h.root_mut::<NavShell>().set_content_header(
            "Groceries",
            vec![Box::new(crate::search::search_button(
                crate::search::SearchRequest::this_app(),
            ))],
        );
        settle(&mut h, 1280, 800);
        assert!(!has_child(&h, crate::search::APP_SEARCH_BUTTON_ID));
    }
}

/// Before any layout the shell builds its tree for the screens a regular
/// window shows. A first layout whose window shows those same screens needs
/// no second tree: Settings opened at a regular size built its tree twice
/// in one layout for nothing. A first layout that lands elsewhere (a narrow
/// window, one screen at a time) still rebuilds, and opens on its list.
#[test]
fn a_first_layout_on_the_screens_the_shell_planned_keeps_its_tree() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut shell = settings_shell();
    let regular = Rect::new(0.0, 0.0, 1280.0, 800.0);
    crate::widget::layout::lay_out_detached(&mut shell, regular, &theme, &mut text)
        .expect("layout");
    assert!(
        !shell.take_structure_dirty(),
        "a regular window lands on the screens the tree was built for"
    );

    let mut shell = settings_shell();
    let narrow = Rect::new(0.0, 0.0, 360.0, 700.0);
    crate::widget::layout::lay_out_detached(&mut shell, narrow, &theme, &mut text).expect("layout");
    assert!(
        shell.take_structure_dirty(),
        "a narrow window shows one screen at a time: its tree is rebuilt"
    );
    assert_eq!(shell.front_pane(), NavPane::Sidebar, "it opens on its list");
}

fn sidebar_resize_control(h: &FrameHarness) -> Option<crate::headless::Control> {
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

/// A shell whose toolbar is its window bar's slots draws no header row, at
/// any width, whatever titles and actions it was given; its own buttons
/// become slots and run from the bar.
#[test]
fn a_toolbar_in_the_window_bar_leaves_no_header_row_and_its_buttons_are_slots() {
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
        "the sidebar's toggle is a slot"
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

    // Narrow: the panes are pushed over one another, and Back is a slot.
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

/// An app that declares no slots of its own gets them from its shell: while the
/// desktop's bar hosts the window, the column in front names the window's
/// location, the headers' buttons are the bar's actions and run from it, and
/// the shell draws no header row.
#[test]
fn a_hosted_shell_hands_its_header_to_the_window_bar() {
    use crate::window_toolbar::{ToolbarEvent, declared, deliver};
    let pressed = SharedRc::new(Cell::new(0));
    let count = SharedRc::clone(&pressed);
    let add = Button::icon_only("mail.new-mailbox", Icon::Plus, "New mailbox")
        .on_click(move || count.set(count.get() + 1));
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
    let action = toolbar
        .actions
        .iter()
        .find(|action| action.name == "New mailbox")
        .expect("the header's button is the bar's action");
    assert_eq!(action.icon, "plus");
    assert!(!action.menu, "a button with an icon stands in the row");
    assert!(
        toolbar
            .actions
            .iter()
            .any(|action| action.id == "mail.reply"),
        "a row of buttons gives its buttons"
    );
    deliver(ToolbarEvent::Action {
        id: action.id.clone(),
    });
    settle(&mut h, 1280, 800);
    assert_eq!(pressed.get(), 1, "the bar's press runs the button");
}

/// A header the bar cannot show (a fill of the app's own beside the title)
/// keeps its row under the bar, and the shell declares nothing; a window
/// where another part declares the slots keeps its row too.
#[test]
fn a_header_the_bar_cannot_show_keeps_its_row() {
    use crate::window_toolbar::{WindowToolbar, declare, declared};
    let shell = mail_shell().content_header_leading(Box::new(Text::new("Path")));
    let mut h = harness(shell, 1280, 800, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, 1280, 800);
    assert!(geometry(&h).header_height > 0.0);
    assert_eq!(declared(), None);
    drop(h);

    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, 1280, 800);
    assert_eq!(geometry(&h).header_height, 0.0);
    let app = WindowToolbar {
        sidebar: Some(false),
        ..WindowToolbar::default()
    };
    declare(Some(app.clone()));
    settle(&mut h, 1280, 800);
    assert!(
        geometry(&h).header_height > 0.0,
        "the shell steps aside for the app's own slots"
    );
    assert_eq!(
        declared(),
        Some(app.bounded()),
        "and leaves them as declared"
    );
}

/// A header button bound to an action, as framework apps build theirs, runs
/// when pressed in the window's bar, though the shell lays no header row out
/// under it. A column the shell does not have offers none of its buttons, as
/// its header row would show none.
#[test]
fn a_header_action_runs_from_the_window_bar() {
    use crate::window_toolbar::{ToolbarEvent, declared};
    let pressed = SharedRc::new(Cell::new(0));
    let count = SharedRc::clone(&pressed);
    let add = Button::icon_only("mail.new-mailbox", Icon::Plus, "New mailbox")
        .on_own_device_effect(move || count.set(count.get() + 1));
    let compose = Button::icon_only("mail.compose", Icon::Plus, "Compose");
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Mailboxes", vec![Box::new(add)])
        .overview_header("Inbox", vec![Box::new(compose)])
        .content_header("Design review", Vec::new());
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

    h.press_window_slot(ToolbarEvent::Action {
        id: "mail.new-mailbox".into(),
    });
    h.idle();
    assert!(
        h.control_failures().is_empty(),
        "{:?}",
        h.control_failures()
    );
    assert_eq!(h.refusal_shown(), None);
    assert_eq!(pressed.get(), 1, "the bar's press runs the button's action");
}

/// A host that runs several apps on one thread (the browser desktop) shows
/// each in a window of its own: every window's shell declares that window's
/// slots, and one declaring never makes another step aside, keep a header
/// row under its bar and leave its old slots there.
#[test]
fn each_window_on_one_thread_keeps_its_own_shells_slots() {
    use crate::hosted_window::HostedWindow;
    use crate::window_toolbar::enter;
    let windows = [HostedWindow::new(), HostedWindow::new()];
    let mut harnesses: Vec<FrameHarness> = windows
        .iter()
        .map(|window| {
            let _scope = enter(Some(window));
            let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
            h.present_in_window_bar();
            settle(&mut h, 1280, 800);
            h
        })
        .collect();
    for _ in 0..2 {
        for (window, h) in windows.iter().zip(&mut harnesses) {
            let _scope = enter(Some(window));
            settle(h, 1280, 800);
        }
    }
    for (i, (window, h)) in windows.iter().zip(&harnesses).enumerate() {
        assert!(
            window.declared().is_some(),
            "window {i}'s shell declares its slots"
        );
        assert_eq!(
            geometry(h).header_height,
            0.0,
            "window {i} draws no header row under its bar"
        );
    }
}

/// A window's app built anew (its code arrived, its tile was rebuilt) after
/// the one before was let go outside that window: the new shell declares
/// the window's slots, and the bar shows none of the old shell's.
#[test]
fn a_shell_built_anew_in_its_window_takes_the_bar_over() {
    use crate::hosted_window::HostedWindow;
    use crate::window_toolbar::enter;
    let window = HostedWindow::new();
    let first = {
        let _scope = enter(Some(&window));
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        h.present_in_window_bar();
        settle(&mut h, 1280, 800);
        h
    };
    assert!(window.declared().is_some(), "the first shell declares");
    drop(first);
    let _scope = enter(Some(&window));
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, 1280, 800);
    settle(&mut h, 1280, 800);
    assert!(window.declared().is_some(), "the new shell declares");
    assert_eq!(
        geometry(&h).header_height,
        0.0,
        "the new shell draws no header row under its bar"
    );
}

/// A content that continues on another device and answers a diagnostic
/// input with its own state.
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
    ) -> Option<crate::collaboration::ContinuationMetadata> {
        Some(crate::collaboration::ContinuationMetadata {
            schema_version: 1,
            bytes: b"line 3".to_vec(),
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        _metadata: &crate::collaboration::ContinuationMetadata,
        _state: &mut crate::widget::Edits<'_>,
        _path: &automerge::ObjId,
    ) -> bool {
        true
    }
}

#[test]
fn the_shell_continues_and_reports_input_as_its_content() {
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
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
            h.root_mut::<NavShell>()
        )
        .is_some()
    );
    assert_eq!(
        h.native_input_snapshot(),
        Some(serde_json::json!({ "caret_line": 3 }))
    );
}
