//! The checks of [`super`]: each reads one kind of problem off a window the toolkit
//! inspected, or one way a row's text is cut, and says which windows it finds it in.

use super::*;
use ui::a11y::{AccessNode, Role};

fn node_of(widget: &str, id: &str, rect: [f64; 4]) -> InspectionNode {
    node(widget, id, rect, None)
}

fn node(widget: &str, id: &str, rect: [f64; 4], role: Option<Role>) -> InspectionNode {
    InspectionNode {
        path: id.to_owned(),
        widget: widget.to_owned(),
        test_id: Some(id.to_owned()),
        focusable: role.is_some(),
        rect,
        child_clip: None,
        page_title: None,
        layout: None,
        paint_key: None,
        semantics: role.map(AccessNode::new),
        children: Vec::new(),
    }
}

/// A title cut off with room beside it is the defect; one that fills its row, or has
/// nothing to spare, is not.
#[test]
fn a_title_is_only_cut_off_unfairly_when_room_stands_beside_it() {
    assert!(title_cut_with_room(180.0, 99.0, 77.0));
    assert!(!title_cut_with_room(180.0, 180.0, 77.0));
    assert!(!title_cut_with_room(180.0, 99.0, 8.0));
    assert!(!title_cut_with_room(180.0, 99.0, 24.0));
}

fn header_row(
    needs: f32,
    has: f32,
    free: f32,
    labelled: bool,
    primary: bool,
) -> ui::widget::HeaderRowLayout {
    ui::widget::HeaderRowLayout {
        column: "content",
        title_needs: needs,
        title_has: has,
        free_beside_title: free,
        labelled_action: labelled,
        primary_shown: primary,
    }
}

/// A header row holds its title against its actions by one rule: a label the row could show
/// as its icon gives way before the title is cut, room standing empty beside the title is
/// the title's, and the app's primary action never leaves the row.
#[test]
fn a_header_row_gives_up_its_labels_before_its_title() {
    assert!(
        header_row_problem(&header_row(180.0, 180.0, 0.0, true, true)).is_none(),
        "a title that fits is cut off by nothing"
    );
    assert!(
        header_row_problem(&header_row(180.0, 180.0, 0.0, false, true)).is_none(),
        "labels stand while the row has room"
    );
    let labels = header_row_problem(&header_row(180.0, 99.0, 8.0, true, true))
        .expect("a title cut beside labels the row could show as icons");
    assert!(labels.contains("icons"), "{labels}");
    assert!(
        header_row_problem(&header_row(180.0, 99.0, 8.0, false, true)).is_none(),
        "a title cut beside actions with nothing left to give is the row's last resort"
    );
    let room = header_row_problem(&header_row(180.0, 99.0, 77.0, false, true))
        .expect("a title cut with room standing empty beside it");
    assert!(room.contains("free beside it"), "{room}");
    let dropped = header_row_problem(&header_row(180.0, 180.0, 0.0, false, false))
        .expect("a row that keeps its menu and not its primary action");
    assert!(dropped.contains("primary"), "{dropped}");
}

fn shell_layout(
    sidebar: ui::widget::SidebarStanding,
    beside: ui::widget::arrange::WidthClass,
    width: f32,
) -> ui::widget::ShellLayout {
    ui::widget::ShellLayout {
        sidebar,
        beside,
        needs: ui::widget::arrange::WidthClass::Medium,
        beside_width: width,
        rows: ui::widget::HeaderRows { rows: [None; 4] },
    }
}

/// The shell reports how it held each header row, and the check holds it to the rule there.
#[test]
fn a_header_row_the_shell_reports_is_checked_where_it_stands() {
    let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
    shell.layout = Some(ui::widget::LayoutInfo::Shell(ui::widget::ShellLayout {
        sidebar: ui::widget::SidebarStanding::None,
        beside: ui::widget::arrange::WidthClass::Compact,
        needs: ui::widget::arrange::WidthClass::Compact,
        beside_width: 400.0,
        rows: ui::widget::HeaderRows {
            rows: [
                None,
                Some(header_row(120.0, 120.0, 0.0, false, false)),
                None,
                None,
            ],
        },
    }));
    let found: Vec<Problem> = inspect(&shell, (400, 300), None, 1.0)
        .into_iter()
        .filter(|problem| problem.kind == Kind::TitleClippedWithRoom)
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].keys.first().map(String::as_str), Some("shell"));
    assert!(found[0].detail.contains("primary"), "{}", found[0].detail);
}

/// A sidebar stands beside content of the window's own: a pane narrower than a column is a
/// phone's screen, so the sidebar folds behind the bar's toggle there instead. The shell reports
/// where its sidebar stood and how wide the pane beside it is, and the check holds it to it.
#[test]
fn a_sidebar_beside_a_phone_screen_is_caught() {
    use ui::widget::SidebarStanding;
    use ui::widget::arrange::WidthClass;
    let with = |sidebar, beside, width| {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 616.0, 700.0], None);
        shell.layout = Some(ui::widget::LayoutInfo::Shell(shell_layout(
            sidebar, beside, width,
        )));
        inspect(&shell, (616, 700), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::ContentBesideSidebar)
            .collect::<Vec<Problem>>()
    };
    let found = with(SidebarStanding::Docked, WidthClass::Compact, 336.0);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].detail.contains("336"), "{}", found[0].detail);
    assert!(
        with(SidebarStanding::Docked, WidthClass::Medium, 420.0).is_empty(),
        "content of a column's width stands beside the sidebar"
    );
    // A grid of tiles or a surface drawn on works narrower than a column: the same width is what
    // its own kind asks for there.
    let mut phone_pane = node("NavShell", "shell", [0.0, 0.0, 616.0, 700.0], None);
    let mut layout = shell_layout(SidebarStanding::Docked, WidthClass::Compact, 336.0);
    layout.needs = WidthClass::Compact;
    phone_pane.layout = Some(ui::widget::LayoutInfo::Shell(layout));
    assert!(
        inspect(&phone_pane, (616, 700), None, 1.0)
            .iter()
            .all(|problem| problem.kind != Kind::ContentBesideSidebar),
        "a surface works below a column where its kind says so"
    );
    assert!(
        with(SidebarStanding::Alone, WidthClass::Compact, 336.0).is_empty(),
        "a sidebar that folded beside a phone-width pane is what the rule asks for"
    );
    assert!(
        with(SidebarStanding::Over, WidthClass::Compact, 336.0).is_empty(),
        "a drawer stands over the content, not beside it"
    );
}

fn overlaps(children: Vec<InspectionNode>) -> Vec<Problem> {
    let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
    shell.children = children;
    inspect(&shell, (400, 300), None, 1.0)
        .into_iter()
        .filter(|p| p.kind == Kind::Overlap)
        .collect()
}

/// A window shell places its header row's buttons itself, over its columns: two of them
/// covering each other (an action over the search button) is caught, though the shell
/// layers its columns on purpose.
#[test]
fn controls_a_shell_places_over_each_other_are_caught() {
    let found = overlaps(vec![
        node(
            "Button",
            "search",
            [300.0, 12.0, 32.0, 32.0],
            Some(Role::Button),
        ),
        node(
            "Button",
            "new",
            [316.0, 12.0, 32.0, 32.0],
            Some(Role::Button),
        ),
    ]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].keys.first().map(String::as_str), Some("new"));
}

/// A column of a board as a height it did not get leaves it: the view scrolling its cards
/// has no room, so the cards under its heading cannot be brought into view. Given the room
/// of a pane, the same view holds them and nothing is lost.
#[test]
fn a_scroll_view_with_no_room_loses_the_cards_it_holds() {
    let lane = |height: f64| {
        let mut scrolling = stack_node(
            "lane",
            [0.0, 40.0, 240.0, height],
            (0..7)
                .map(|index| {
                    stack_node(
                        "card",
                        [0.0, 40.0 + 76.0 * f64::from(index), 240.0, 68.0],
                        vec![],
                    )
                })
                .collect(),
        );
        scrolling.layout = Some(ui::widget::LayoutInfo::Scroll(ui::widget::ScrollLayout {
            direction: ui::widget::ScrollDirection::Vertical,
        }));
        stack_node("window", [0.0, 0.0, 400.0, 300.0], vec![scrolling])
    };
    let lost = |root: &InspectionNode| {
        inspect(root, (400, 300), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::LostContent)
            .map(|problem| problem.keys)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lost(&lane(0.0)),
        vec![vec!["lane".to_owned(), "window".to_owned()]],
        "a scroll view with no height holds seven cards in none of it"
    );
    assert!(
        lost(&lane(600.0)).is_empty(),
        "given the room of a pane, the same cards are there to scroll through"
    );
}

/// A list keeps no scroller of its own inside the page that scrolls it: the page carries the
/// rows past the window's edge, and the list stands as tall as them.
#[test]
fn a_list_scrolling_inside_the_page_that_scrolls_it_is_caught() {
    fn list_filling(height: f64) -> InspectionNode {
        let mut list = node(
            "DescribedList",
            "list",
            [0.0, 40.0, 400.0, height],
            Some(Role::List),
        );
        list.children = (0..10)
            .map(|index| {
                stack_node(
                    "row",
                    [0.0, 40.0 + 40.0 * f64::from(index), 400.0, 36.0],
                    vec![],
                )
            })
            .collect();
        list
    }
    fn under(widget: &str, list: InspectionNode) -> InspectionNode {
        let mut above = node(widget, "above", [0.0, 0.0, 400.0, 300.0], None);
        above.children = vec![list];
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
        shell.children = vec![above];
        shell
    }
    let nested = |root: &InspectionNode| {
        inspect(root, (400, 300), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::NestedScroller)
            .map(|problem| problem.keys)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        nested(&under("Page", list_filling(200.0))),
        vec![vec![
            "list".to_owned(),
            "above".to_owned(),
            "shell".to_owned()
        ]],
        "a list scrolling rows the page scrolls in a box of its own"
    );
    assert!(
        nested(&under("Page", list_filling(460.0))).is_empty(),
        "the list as tall as its rows leaves the page to scroll them"
    );
    assert!(
        nested(&under("Docked", list_filling(200.0))).is_empty(),
        "a list scrolling in a column of its own is the column's scroller"
    );
}

/// A row taking half the window is the page itself, and caught — unless its content is as
/// tall as it is: a conversation's message fills its row whatever the row's height, and a
/// row that tall is no page stretched around one line.
#[test]
fn a_tall_row_is_caught_only_when_its_line_floats_in_it() {
    let bare_rows = |content: InspectionNode| {
        let mut row = node("SwipeRow", "row", [0.0, 0.0, 400.0, 200.0], None);
        row.children.push(content);
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
        shell.children.push(row);
        inspect(&shell, (400, 300), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::BareRow)
            .count()
    };
    assert_eq!(
        bare_rows(node("Row", "line", [0.0, 88.0, 400.0, 24.0], None)),
        1,
        "a line floating in the row's middle is the page drawn as one row"
    );
    assert_eq!(
        bare_rows(node("Row", "body", [0.0, 0.0, 400.0, 200.0], None)),
        0,
        "a row as tall as what it holds is as tall as its content"
    );
}

/// A page is the readable column, its gutter counted once: a page given a column of its own
/// inside one counts it twice, so its content stands inset from the rows beside it and narrower
/// than them. A page holding rows, and no page, is the column itself.
#[test]
fn a_page_inside_a_page_is_caught() {
    let page_rows = |inner: Vec<InspectionNode>| {
        let mut list = node("DescribedList", "list", [0.0, 0.0, 400.0, 300.0], None);
        list.children = inner;
        stack_node("window", [0.0, 0.0, 400.0, 300.0], vec![list])
    };
    let found = |root: &InspectionNode| {
        inspect(root, (400, 300), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::PageInPage)
            .map(|problem| problem.keys)
            .collect::<Vec<_>>()
    };
    let mut row = node("Page", "row", [0.0, 0.0, 400.0, 40.0], None);
    row.children
        .push(node("Row", "message", [24.0, 16.0, 352.0, 24.0], None));
    assert!(
        found(&page_rows(vec![row])).is_empty(),
        "a row of the column is the column, not a page of its own"
    );
    let mut nested = node("Page", "row", [0.0, 0.0, 400.0, 40.0], None);
    let mut inner = node("Page", "inner", [24.0, 0.0, 352.0, 40.0], None);
    inner
        .children
        .push(node("Row", "message", [48.0, 16.0, 304.0, 24.0], None));
    nested.children.push(inner);
    assert_eq!(
        found(&page_rows(vec![nested])),
        vec![vec![
            "row".to_owned(),
            "list".to_owned(),
            "window".to_owned()
        ]],
        "a page inside the column's row is the gutter counted twice"
    );
}

/// A bar docked at the foot of a column stands in the column the content over it is read at:
/// an entry that adds to a list is as wide as the list, its leading edge where the list's is.
/// A bar across the window beside the list it adds to is the defect, and one standing on the
/// column is not, the room the content's scroller keeps for its bar aside.
#[test]
fn a_docked_bar_wider_than_the_column_over_it_is_caught() {
    // The Docked's content as a scroller `scroll` wide holding a page `page` wide, its list
    // `list` wide, and the bar of `bar` at its foot.
    let docked = |scroll: [f64; 4], page: [f64; 4], list: [f64; 4], bar: [f64; 4]| {
        let mut page = node_of("Page", "page", page);
        page.children.push(node_of("DescribedList", "list", list));
        let mut scroll = node_of("Scrollable", "scroll", scroll);
        scroll.children.push(page);
        let mut foot = node_of("Page", "foot", [280.0, 660.0, 1000.0, 96.0]);
        foot.children.push(node_of("Stack", "bar", bar));
        let mut column = node_of("Docked", "docked", [280.0, 0.0, 1000.0, 756.0]);
        column.children.push(scroll);
        column.children.push(foot);
        stack_node("window", [0.0, 0.0, 1280.0, 756.0], vec![column])
    };
    let found = |root: &InspectionNode| {
        inspect(root, (1280, 756), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::DockedColumn)
            .map(|problem| problem.keys)
            .collect::<Vec<_>>()
    };
    let read_at_the_measure = |bar: [f64; 4]| {
        docked(
            [280.0, 0.0, 1000.0, 648.0],
            [280.0, 0.0, 984.0, 835.0],
            [492.0, 48.0, 560.0, 739.0],
            bar,
        )
    };
    assert_eq!(
        found(&read_at_the_measure([328.0, 676.0, 904.0, 32.0])),
        vec![vec!["docked".to_owned(), "window".to_owned()]],
        "a bar across the window beside the column is the defect"
    );
    assert!(
        found(&read_at_the_measure([500.0, 676.0, 560.0, 32.0])).is_empty(),
        "a bar standing on the column adds to it in its own column"
    );
    assert!(
        found(&read_at_the_measure([492.0, 676.0, 560.0, 32.0])).is_empty(),
        "and one the scroller's bar puts a few pixels in still stands on it"
    );
    assert!(
        found(&docked(
            [0.0, 0.0, 272.0, 648.0],
            [0.0, 0.0, 256.0, 835.0],
            [0.0, 48.0, 256.0, 739.0],
            [8.0, 676.0, 272.0, 32.0],
        ))
        .is_empty(),
        "the room a scroller keeps for its bar is no misalignment: the column keeps it at its trailing edge"
    );
}

/// A scroll view's content stands in a box the view holds it in, which is the view's own
/// content and no container of its own: what the view scrolls through may go on past the
/// view. A box inside that content still holds what it holds, and is caught.
#[test]
fn a_scroll_views_content_box_passes_its_scrolling_on() {
    let scrolled = |content: InspectionNode| {
        let mut page = stack_node("page", [0.0, 0.0, 400.0, 300.0], vec![content]);
        page.layout = Some(ui::widget::LayoutInfo::Scroll(ui::widget::ScrollLayout {
            direction: ui::widget::ScrollDirection::Vertical,
        }));
        stack_node("window", [0.0, 0.0, 400.0, 300.0], vec![page])
    };
    let overflowing = |root: &InspectionNode| {
        inspect(root, (400, 300), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::Overflow)
            .map(|problem| problem.keys)
            .collect::<Vec<_>>()
    };
    let page = scrolled(stack_node(
        "content",
        [0.0, 0.0, 380.0, 300.0],
        vec![stack_node(
            "long",
            [0.0, 0.0, 380.0, 900.0],
            vec![stack_node(
                "card",
                [0.0, 0.0, 380.0, 100.0],
                vec![stack_node("over", [0.0, 0.0, 380.0, 240.0], vec![])],
            )],
        )],
    ));
    assert_eq!(
        overflowing(&page),
        vec![vec![
            "over".to_owned(),
            "card".to_owned(),
            "long".to_owned(),
            "content".to_owned(),
            "page".to_owned(),
            "window".to_owned()
        ]],
        "the content the view scrolls through may be taller than the view; the card in it may not"
    );
}

/// A column header that reads 0, and a place whose quiet count reads 0: a count says
/// something wants attention, so zero is no badge and no room kept for one.
#[test]
fn a_count_of_zero_is_caught_where_it_is_drawn() {
    let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
    let mut list = node(
        "NavList",
        "places",
        [0.0, 40.0, 280.0, 240.0],
        Some(Role::Tree),
    );
    list.semantics
        .as_mut()
        .expect("the list's semantics")
        .children = vec![
        place("Assigned to me", [0.0, 40.0, 280.0, 72.0], 0, false),
        place("Inbox", [0.0, 72.0, 280.0, 104.0], 3, false),
    ];
    shell.children = vec![
        list,
        counted(
            "NavShellTitle",
            "title",
            [300.0, 8.0, 380.0, 32.0],
            Role::Heading { level: 2 },
            "Done",
            "0",
        ),
    ];
    let zeros: Vec<Problem> = inspect(&shell, (400, 300), None, 1.0)
        .into_iter()
        .filter(|problem| problem.kind == Kind::ZeroCount)
        .collect();
    assert_eq!(zeros.len(), 2, "{zeros:?}");
}

/// A column header and the place beside it in the sidebar drawing the same number: the
/// same count twice on one screen. A different number, or a place past the header, is not
/// the same count again.
#[test]
fn a_header_count_the_place_beside_it_shows_is_caught_once() {
    let repeated = |header: u32, shown: u32, list_x0: f64| {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
        let mut list = node(
            "NavList",
            "places",
            [list_x0, 40.0, list_x0 + 280.0, 240.0],
            Some(Role::Tree),
        );
        list.semantics
            .as_mut()
            .expect("the list's semantics")
            .children = vec![place(
            "Inbox",
            [list_x0, 40.0, list_x0 + 280.0, 72.0],
            shown,
            true,
        )];
        shell.children = vec![
            list,
            counted(
                "NavShellTitle",
                "title",
                [600.0, 8.0, 680.0, 32.0],
                Role::Heading { level: 2 },
                "Inbox",
                &header.to_string(),
            ),
        ];
        let root = stack_node("window", [0.0, 0.0, 900.0, 300.0], vec![shell]);
        inspect(&root, (900, 300), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::RepeatedCount)
            .count()
    };
    assert_eq!(repeated(6, 6, 0.0), 1, "the sidebar beside it says 6");
    assert_eq!(repeated(6, 5, 0.0), 0, "the place says another number");
    assert_eq!(
        repeated(6, 6, 600.0),
        0,
        "a place past the header is no sidebar"
    );
}

fn counted(
    widget: &str,
    id: &str,
    rect: [f64; 4],
    role: Role,
    name: &str,
    value: &str,
) -> InspectionNode {
    let mut header = node(widget, id, rect, Some(role));
    let access = header.semantics.as_mut().expect("the header's semantics");
    access.name = Some(name.to_owned());
    access.value = Some(value.to_owned());
    header
}

/// A bar naming the page while the page shows those words as its own title: the same title
/// twice on one screen. A bar whose title is already covered, or a page whose heading says
/// other words, is no repeat.
#[test]
fn a_bar_naming_a_page_beside_the_pages_own_heading_is_caught_once() {
    let repeated = |bar_title: Option<&str>, page: Option<&str>| {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 390.0, 844.0], None);
        let mut bar = node(
            "NavShellTitle",
            "bar",
            [68.0, 18.0, 314.0, 39.0],
            Some(Role::Heading { level: 2 }),
        );
        if let Some(title) = bar_title {
            bar.semantics.as_mut().expect("the bar's semantics").name = Some(title.to_owned());
        } else {
            bar.semantics = None;
        }
        let mut heading = node("TextInput", "title", [80.0, 80.0, 294.0, 161.0], None);
        heading.page_title = page.map(str::to_owned);
        shell.children = vec![bar, heading];
        inspect(&shell, (390, 844), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::RepeatedPageTitle)
            .count()
    };
    assert_eq!(
        repeated(
            Some("Call the landlord about the boiler"),
            Some("Call the landlord about the boiler")
        ),
        1,
        "the page's heading says what the bar says"
    );
    assert_eq!(
        repeated(Some("Call the landlord about the boiler"), None),
        0,
        "the page shows no title of its own, so the bar names it"
    );
    assert_eq!(
        repeated(None, Some("Call the landlord about the boiler")),
        0,
        "the bar left the title to the page"
    );
    assert_eq!(
        repeated(
            Some("Call the landlord about the boiler"),
            Some("Call the plumber")
        ),
        0,
        "the page shows other words"
    );
}

/// A bar with one way back has that way at its leading edge. A back control standing beside
/// the sidebar toggle is two ways from one corner: caught.
#[test]
fn a_back_control_beside_the_sidebar_toggle_is_caught() {
    let beside = |toggle: Option<[f64; 4]>, back: Option<[f64; 4]>| {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 390.0, 844.0], None);
        let control = |id: &str, rect: [f64; 4]| node("Button", id, rect, Some(Role::Button));
        shell.children = [
            toggle.map(|rect| control(SIDEBAR_TOGGLE, rect)),
            back.map(|rect| control(BACK, rect)),
        ]
        .into_iter()
        .flatten()
        .collect();
        inspect(&shell, (390, 844), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::DoubleNavigation)
            .count()
    };
    // A toggle named for the window's corner, and back after it.
    let toggle = Some([8.0, 18.0, 44.0, 44.0]);
    let beside_toggle = Some([60.0, 18.0, 44.0, 44.0]);
    assert_eq!(
        beside(toggle, beside_toggle),
        1,
        "the phone's pushed screen shows the toggle and back side by side"
    );
    assert_eq!(
        beside(toggle, Some([8.0, 18.0, 44.0, 44.0])),
        0,
        "back at the leading edge is the one way back"
    );
    assert_eq!(
        beside(None, beside_toggle),
        0,
        "a screen without a sidebar toggle has only its way back"
    );
    assert_eq!(
        beside(toggle, Some([300.0, 18.0, 44.0, 44.0])),
        0,
        "a docked sidebar's toggle stands in the window's corner, back in its column"
    );
}

fn place(name: &str, rect: [f64; 4], count: u32, selected: bool) -> AccessNode {
    let mut place = AccessNode::new(Role::TreeItem);
    place.name = Some(name.to_owned());
    place.value = Some(count.to_string());
    place.state.selected = selected;
    place.bounds = Some(Rect::new(rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]).into());
    place
}

fn inspected(root: &InspectionNode) -> Vec<Problem> {
    inspect(root, (800, 600), None, 1.0)
        .into_iter()
        .filter(|p| p.kind == Kind::Overlap)
        .collect()
}

fn lone_doors(root: &InspectionNode) -> Vec<Problem> {
    inspect(root, (1280, 800), None, 1.0)
        .into_iter()
        .filter(|p| p.kind == Kind::LoneDoor)
        .collect()
}

/// A page's "…" is the item's actions at the trailing edge of the title's line. One
/// standing in a column takes a line of the document for its glyph alone, under the title:
/// caught. Beside the title, and in the window bar, a menu is where it belongs.
#[test]
fn a_pages_dots_are_caught_only_off_the_titles_line() {
    let page = |content: InspectionNode| {
        stack_node(
            "window",
            [0.0, 0.0, 1280.0, 800.0],
            vec![stack_node(
                "page",
                [0.0, 56.0, 1280.0, 200.0],
                vec![content],
            )],
        )
    };
    let door = || {
        node(
            "MenuButton",
            "detail~more",
            [888.0, 104.0, 32.0, 32.0],
            Some(Role::Button),
        )
    };
    let alone = page(stack_node(
        "content",
        [360.0, 104.0, 560.0, 85.0],
        vec![door()],
    ));
    let found = lone_doors(&alone);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0].keys.first().map(String::as_str),
        Some("detail~more"),
        "at the page's door"
    );
    let line = page(holding(
        node("Row", "title-line", [360.0, 104.0, 560.0, 49.0], None),
        vec![
            node("TextInput", "detail", [360.0, 104.0, 520.0, 49.0], None),
            door(),
        ],
    ));
    assert!(
        lone_doors(&line).is_empty(),
        "beside its title, the door is where it belongs"
    );
    let bar = page(node(
        "MenuButton",
        "nav-shell-more-3",
        [1220.0, 12.0, 32.0, 32.0],
        Some(Role::Button),
    ));
    assert!(
        lone_doors(&bar).is_empty(),
        "the window bar's own menu is no page's door"
    );
}

fn empty_headings(root: &InspectionNode) -> Vec<Problem> {
    inspect(root, (1280, 800), None, 1.0)
        .into_iter()
        .filter(|p| p.kind == Kind::EmptyHeading)
        .collect()
}

/// A heading nobody named takes its band's height over a lane's cards all the same: the
/// strip says nothing, and the cards start below it. Named, it is the lane's words; a lane
/// no heading heads starts its cards where a list starts its first row.
#[test]
fn a_heading_with_nothing_in_it_is_caught() {
    let heading = |name: &str| {
        let words = !name.is_empty();
        let mut header = node(
            HEADING,
            "heading",
            [304.0, 80.0, 320.0, 29.0],
            Some(Role::Heading { level: 2 }),
        );
        header.semantics = Some(AccessNode {
            name: words.then(|| name.to_owned()),
            ..AccessNode::new(Role::Heading { level: 2 })
        });
        let text = if words {
            [304.0, 84.0, 69.0, 21.0]
        } else {
            [304.0, 94.0, 0.0, 0.0]
        };
        header.children = vec![node(
            "Text",
            "title",
            text,
            Some(Role::Heading { level: 2 }),
        )];
        header
    };
    let lane = |header: Option<InspectionNode>| {
        let top = if header.is_some() { 117.0 } else { 80.0 };
        let cards = stack_node(
            "cards",
            [304.0, top, 320.0, 200.0],
            vec![node("ListRow", "card", [304.0, top, 320.0, 49.0], None)],
        );
        let column = holding(
            node("Column", "column", [304.0, 80.0, 320.0, 300.0], None),
            header.into_iter().chain([cards]).collect(),
        );
        holding(
            node("Container", "lane", [304.0, 80.0, 320.0, 300.0], None),
            vec![column],
        )
    };
    let board = |lane: InspectionNode| {
        holding(
            node("Row", "board", [280.0, 80.0, 1000.0, 600.0], None),
            vec![lane],
        )
    };
    let found = empty_headings(&board(lane(Some(heading("")))));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].keys.first().map(String::as_str), Some("heading"));
    assert!(
        empty_headings(&board(lane(Some(heading("Doing"))))).is_empty(),
        "a named heading is the lane's words"
    );
    assert!(
        empty_headings(&board(lane(None))).is_empty(),
        "a lane no heading heads starts its cards at its top"
    );
}

fn crowded_headings(root: &InspectionNode) -> Vec<Problem> {
    inspect(root, (1280, 800), None, 1.0)
        .into_iter()
        .filter(|p| p.kind == Kind::CrowdedHeading)
        .collect()
}

/// A section's heading belongs to what follows it: it keeps clearly more room under the block
/// over it than over its own content, or it reads as that block's last line.
#[test]
fn a_heading_keeps_more_room_over_it_than_over_its_content() {
    let page = |above: f64, below: f64| {
        let top = 200.0 + above;
        let header = node(
            HEADING,
            "heading",
            [304.0, top, 320.0, 29.0],
            Some(Role::Heading { level: 2 }),
        );
        let item = node(
            "ListRow",
            "row",
            [304.0, top + 29.0 + below, 320.0, 36.0],
            None,
        );
        let section = holding(
            node(
                "Section",
                "section",
                [304.0, top, 320.0, 29.0 + below + 36.0],
                None,
            ),
            vec![header, item],
        );
        holding(
            node("Page", "page", [304.0, 100.0, 320.0, 600.0], None),
            vec![
                node("Text", "body", [304.0, 150.0, 320.0, 50.0], None),
                section,
            ],
        )
    };
    assert!(
        crowded_headings(&page(24.0, 8.0)).is_empty(),
        "a group's gutter over the heading, its content a step under it"
    );
    assert!(
        crowded_headings(&page(16.0, 8.0)).is_empty(),
        "a section gap over the heading reads as more than its content's step"
    );
    let crowded = crowded_headings(&page(8.0, 16.0));
    assert_eq!(crowded.len(), 1, "{crowded:?}");
    assert!(
        crowded[0].detail.contains("8 px") && crowded[0].detail.contains("16 px"),
        "{crowded:?}"
    );
    // A heading first in its box has nothing over it to stand apart from.
    let first = holding(
        node("Page", "page", [304.0, 100.0, 320.0, 600.0], None),
        vec![holding(
            node("Section", "section", [304.0, 100.0, 320.0, 100.0], None),
            vec![
                node(
                    HEADING,
                    "heading",
                    [304.0, 100.0, 320.0, 29.0],
                    Some(Role::Heading { level: 2 }),
                ),
                node("ListRow", "row", [304.0, 140.0, 320.0, 36.0], None),
            ],
        )],
    );
    assert!(crowded_headings(&first).is_empty());
}

fn empty_surfaces(root: &InspectionNode) -> Vec<Problem> {
    inspect(root, (800, 600), None, 1.0)
        .into_iter()
        .filter(|p| p.kind == Kind::EmptySurface)
        .collect()
}

fn unruled_bodies(root: &InspectionNode) -> Vec<Problem> {
    inspect(root, (1280, 800), None, 1.0)
        .into_iter()
        .filter(|p| p.kind == Kind::UnruledBody)
        .collect()
}

/// A page's own sections stand under a rule, apart from the writing the page holds: a heading
/// set like the writing's own and standing no further from it reads as another line of the text.
#[test]
fn a_page_section_after_the_writing_stands_under_a_rule() {
    let page = |between: Vec<InspectionNode>, wrapped: bool| {
        let top = 400.0;
        let section = holding(
            node("Section", "section", [304.0, top, 320.0, 120.0], None),
            vec![
                node(
                    HEADING,
                    "heading",
                    [304.0, top, 320.0, 29.0],
                    Some(Role::Heading { level: 2 }),
                ),
                node("ListRow", "row", [304.0, top + 37.0, 320.0, 36.0], None),
            ],
        );
        let mut blocks = vec![node(
            "BlockEditor",
            "writing",
            [304.0, 120.0, 320.0, 250.0],
            None,
        )];
        if wrapped {
            blocks.push(holding(
                node("Stack", "tail", [304.0, top - 24.0, 320.0, 144.0], None),
                {
                    let mut tail = between;
                    tail.push(section);
                    tail
                },
            ));
        } else {
            blocks.extend(between);
            blocks.push(section);
        }
        holding(
            node("Page", "page", [304.0, 100.0, 320.0, 600.0], None),
            blocks,
        )
    };
    let rule = || node("Divider", "rule", [304.0, 378.0, 320.0, 1.0], None);
    let unruled = unruled_bodies(&page(Vec::new(), false));
    assert_eq!(unruled.len(), 1, "{unruled:?}");
    assert!(unruled[0].detail.contains("writing"), "{unruled:?}");
    assert!(
        unruled_bodies(&page(vec![rule()], false)).is_empty(),
        "a rule between the writing and the page's own sections says where the writing ends"
    );
    assert!(
        unruled_bodies(&page(vec![rule()], true)).is_empty(),
        "the rule stands in the box the page's sections are gathered in"
    );
    assert!(
        !unruled_bodies(&page(Vec::new(), true)).is_empty(),
        "a box of the page's own sections with no rule over them changes nothing"
    );
}

/// A card standing empty in a column paints a filled block with nothing in it, and what it
/// promised is not there: a surface is drawn for what it holds, so one holding nothing — or
/// only a placeholder that never filled — is no surface at all. What holds words, a control
/// or anything laid out of its own is a surface doing its work.
#[test]
fn a_surface_with_nothing_in_it_is_caught() {
    let column = |surface: Option<InspectionNode>, under: Option<InspectionNode>| {
        let mut children: Vec<InspectionNode> = surface.into_iter().collect();
        children.extend(under);
        holding(
            node("Column", "column", [24.0, 56.0, 342.0, 600.0], None),
            children,
        )
    };
    let card = |children: Vec<InspectionNode>| {
        holding(
            node("Card", "card", [24.0, 56.0, 342.0, 157.0], None),
            children,
        )
    };
    let found = empty_surfaces(&column(Some(card(Vec::new())), None));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].keys.first().map(String::as_str), Some("card"));
    assert!(
        found[0].detail.contains("342×157"),
        "it names the block it paints: {}",
        found[0].detail
    );
    let placeholder = card(vec![node("Card", "body", [24.0, 56.0, 0.0, 0.0], None)]);
    assert_eq!(
        empty_surfaces(&column(Some(placeholder), None)).len(),
        1,
        "a placeholder that never filled leaves the block empty"
    );
    let written = card(vec![node(
        "Text",
        "body",
        [44.0, 76.0, 302.0, 21.0],
        Some(Role::StaticText),
    )]);
    assert!(
        empty_surfaces(&column(Some(written), None)).is_empty(),
        "a card saying something is what the block is for"
    );
    let pictured = card(vec![node(
        "Picture",
        "cover",
        [44.0, 76.0, 302.0, 120.0],
        None,
    )]);
    assert!(
        empty_surfaces(&column(Some(pictured), None)).is_empty(),
        "a card showing a picture holds something"
    );
    let flat = holding(
        node("Card", "flat", [24.0, 56.0, 342.0, 0.0], None),
        Vec::new(),
    );
    assert!(
        empty_surfaces(&column(Some(flat), None)).is_empty(),
        "a block with no height paints nothing"
    );
    assert!(
        empty_surfaces(&column(
            None,
            Some(node("Stack", "rows", [24.0, 56.0, 342.0, 200.0], None))
        ))
        .is_empty(),
        "a column is no surface of its own"
    );
}

fn named(widget: &str, id: &str, rect: [f64; 4], said: &str) -> InspectionNode {
    let mut boxed = node(widget, id, rect, Some(Role::Group));
    if let Some(access) = boxed.semantics.as_mut() {
        access.name = Some(said.to_owned());
    }
    boxed
}

fn placeholder_panes(root: &InspectionNode) -> Vec<Problem> {
    inspect(root, (1280, 800), None, 1.0)
        .into_iter()
        .filter(|p| p.kind == Kind::PlaceholderPane)
        .collect()
}

/// A pane that only says nothing is chosen, standing beside the list the window chooses
/// from, takes the room the rows would list in: with nothing chosen the list lists for the
/// whole window, and the pane appears when a row is chosen. A window that lists no items
/// beside it keeps its pane, and so does a phone, where the pane stands over the list it
/// pushed aside rather than beside it.
#[test]
fn a_pane_saying_nothing_is_chosen_beside_the_list_is_caught() {
    let window = [0.0, 0.0, 1280.0, 800.0];
    let list = |rect: [f64; 4]| {
        holding(
            node("DescribedList", "notes", rect, None),
            vec![node(
                "SwipeRow",
                "note",
                [rect[0] + 24.0, rect[1], 296.0, 52.0],
                None,
            )],
        )
    };
    let chosen = |rect: [f64; 4]| named("EmptyState", "chosen", rect, "No note selected");
    let shell =
        |children: Vec<InspectionNode>| holding(node("NavShell", "shell", window, None), children);
    let found = placeholder_panes(&shell(vec![
        list([280.0, 56.0, 344.0, 744.0]),
        chosen([624.0, 56.0, 656.0, 744.0]),
    ]));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].keys.first().map(String::as_str), Some("chosen"));
    assert!(
        found[0].detail.contains("\"No note selected\""),
        "it names what the pane says: {}",
        found[0].detail
    );
    assert!(
        placeholder_panes(&shell(vec![chosen([624.0, 56.0, 656.0, 744.0])])).is_empty(),
        "a window listing nothing keeps its pane"
    );
    let mut places = node("NavList", "places", [280.0, 56.0, 344.0, 744.0], None);
    places.children = vec![node(
        "NavItem",
        "place",
        [280.0, 56.0, 344.0, 32.0],
        Some(Role::TreeItem),
    )];
    assert!(
        placeholder_panes(&shell(vec![places, chosen([624.0, 56.0, 656.0, 744.0])])).is_empty(),
        "the places of a sidebar are no list of items to choose from"
    );
    assert!(
        placeholder_panes(&shell(vec![
            list([-1000.0, 56.0, 344.0, 744.0]),
            chosen([0.0, 56.0, 1280.0, 744.0]),
        ]))
        .is_empty(),
        "a phone's pane stands over the list it pushed aside, not beside it"
    );
    assert!(
        placeholder_panes(&shell(vec![
            list([280.0, 56.0, 344.0, 744.0]),
            chosen([624.0, 300.0, 100.0, 60.0]),
        ]))
        .is_empty(),
        "a notice inside a column is no pane of its own"
    );
}

fn bare_rows(root: &InspectionNode) -> Vec<Problem> {
    inspect(root, (800, 600), None, 1.0)
        .into_iter()
        .filter(|p| p.kind == Kind::BareRow)
        .collect()
}

/// A page whose content is one row: the row is stretched over the column and its line
/// floats at the column's middle, where a page around it would hold it at the top. A list's
/// rows are their line's height, and a page that holds a row is no finding.
#[test]
fn a_page_drawn_as_one_row_is_caught() {
    let shell = |content: Vec<InspectionNode>| {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 800.0, 600.0], None);
        shell.children = content;
        shell
    };
    let bare = shell(vec![node(
        "ListRow",
        "detail-row",
        [416.0, 56.0, 384.0, 544.0],
        Some(Role::ListItem),
    )]);
    let found = bare_rows(&bare);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0].keys.first().map(String::as_str),
        Some("detail-row"),
        "at the row a view drew"
    );
    let listed = shell(vec![holding(
        node("DescribedList", "rows", [76.0, 56.0, 340.0, 540.0], None),
        vec![
            node("ListRow", "a", [76.0, 56.0, 340.0, 36.0], None),
            node("ListRow", "b", [76.0, 92.0, 340.0, 36.0], None),
        ],
    )]);
    assert!(bare_rows(&listed).is_empty(), "a list's rows are lines");
    let page = shell(vec![holding(
        node("Scroll", "scroll", [416.0, 56.0, 384.0, 544.0], None),
        vec![holding(
            node("page", "page", [416.0, 56.0, 384.0, 544.0], None),
            vec![holding(
                node("stack", "stack", [440.0, 80.0, 336.0, 120.0], None),
                vec![node(
                    "ListRow",
                    "summary",
                    [440.0, 80.0, 336.0, 60.0],
                    Some(Role::ListItem),
                )],
            )],
        )],
    )]);
    assert!(
        bare_rows(&page).is_empty(),
        "a page holding a row is no finding"
    );
}

fn holding(mut outer: InspectionNode, children: Vec<InspectionNode>) -> InspectionNode {
    outer.children = children;
    outer
}

/// Layers that each hold text somewhere cover nothing until the text itself meets: a bar
/// across the top and a dock on the foot of a desk drawn over the whole window are two
/// layers whose words stand apart; a caption drawn on another caption is caught.
#[test]
fn layers_cover_each_other_only_where_their_text_meets() {
    let layer = |id: &str, text: [f64; 4]| {
        holding(
            node("Stack", id, [0.0, 0.0, 800.0, 600.0], None),
            vec![node("Text", &format!("{id}-text"), text, None)],
        )
    };
    let apart = holding(
        node("Layers", "desk", [0.0, 0.0, 800.0, 600.0], None),
        vec![
            layer("bar", [8.0, 8.0, 120.0, 20.0]),
            layer("dock", [300.0, 560.0, 40.0, 20.0]),
        ],
    );
    assert!(inspected(&apart).is_empty());
    let meeting = holding(
        node("Layers", "desk", [0.0, 0.0, 800.0, 600.0], None),
        vec![
            layer("bar", [8.0, 8.0, 120.0, 20.0]),
            layer("over", [60.0, 12.0, 120.0, 20.0]),
        ],
    );
    assert_eq!(inspected(&meeting).len(), 1);
    // Text a scroller has moved out of its view is not read, so it covers nothing: a
    // conversation scrolled to its end, its first messages above its top edge.
    let mut scroller = holding(
        node("Scroll", "scroller", [0.0, 100.0, 800.0, 400.0], None),
        vec![node("Text", "scrolled-out", [8.0, 8.0, 120.0, 20.0], None)],
    );
    scroller.child_clip = Some([0.0, 100.0, 800.0, 400.0]);
    let desk_over_scroller = holding(
        node("Layers", "desk", [0.0, 0.0, 800.0, 600.0], None),
        vec![
            layer("bar", [8.0, 8.0, 120.0, 20.0]),
            holding(
                node("Stack", "window", [0.0, 0.0, 800.0, 600.0], None),
                vec![scroller],
            ),
        ],
    );
    assert!(inspected(&desk_over_scroller).is_empty());
}

/// Windows on a desktop stand over each other by design: the one drawn later is opaque
/// and covers the one under it, and that is no finding; two plain boxes side by side in a
/// free placement covering each other still are.
#[test]
fn a_window_over_another_window_is_no_finding() {
    let window = |id: &str, at: [f64; 4]| {
        holding(
            node("TilolangPane", id, at, None),
            vec![holding(
                node("WindowFrame", &format!("{id}-frame"), at, None),
                vec![node(
                    "Text",
                    &format!("{id}-title"),
                    [at[0] + 16.0, at[1] + 12.0, 100.0, 20.0],
                    None,
                )],
            )],
        )
    };
    let desk = holding(
        node("Manual", "desk", [0.0, 0.0, 800.0, 600.0], None),
        vec![
            window("behind", [40.0, 40.0, 400.0, 300.0]),
            window("front", [200.0, 100.0, 400.0, 300.0]),
        ],
    );
    assert!(inspected(&desk).is_empty());
    let boxes = holding(
        node("Manual", "desk", [0.0, 0.0, 800.0, 600.0], None),
        vec![
            node("Stack", "a", [40.0, 40.0, 400.0, 300.0], None),
            node("Stack", "b", [200.0, 100.0, 400.0, 300.0], None),
        ],
    );
    assert_eq!(inspected(&boxes).len(), 1);
}

/// On touch a control smaller than a finger's target is caught, at its own box grown by
/// what it reports; with a mouse, whose hit areas grow about the face, nothing is.
#[test]
fn a_control_smaller_than_a_finger_is_caught_on_touch() {
    let mut root = node("Column", "root", [0.0, 0.0, 400.0, 300.0], None);
    root.children = vec![
        node(
            "Button",
            "small",
            [16.0, 16.0, 32.0, 32.0],
            Some(Role::Button),
        ),
        node(
            "Button",
            "large",
            [16.0, 64.0, 48.0, 48.0],
            Some(Role::Button),
        ),
    ];
    let small = |min| {
        inspect(&root, (400, 300), min, 1.0)
            .into_iter()
            .filter(|p| p.kind == Kind::SmallHitTarget)
            .map(|p| p.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(small(Some(48.0)), [Some("small".to_owned())]);
    assert!(small(None).is_empty());
}

/// A long word in a column narrower than it: text that wraps breaks it anywhere and shows
/// all of it; text that keeps its words whole (source code's soft wrap) is clipped, and
/// caught.
#[test]
fn text_that_cannot_break_a_word_wider_than_its_column_is_caught() {
    use ui::widget::arrange::{Length, Slot, Stack};
    let clipped = |mode: ui::text_render::TextWrapMode| {
        let text = ui::widget::Text::new("Donaudampfschifffahrtsgesellschaft").with_wrap_mode(mode);
        let mut column = Slot::new(Box::new(Stack::stack().push(Box::new(text))));
        column.fit.width.max = Some(Length::Rem(6.0));
        let mut h = ui::headless::FrameHarness::for_root(
            ui::State::new(),
            Stack::stack().push_slot(column),
            (320, 200),
            ui::Theme::light(),
        );
        let mut renderer = ui::TextRenderer::new();
        clipped_text(h.root_widget_mut(), &mut renderer, (320, 200)).len()
    };
    assert_eq!(clipped(ui::text_render::TextWrapMode::Wrap), 0);
    assert_eq!(clipped(ui::text_render::TextWrapMode::Indented), 1);
}

/// A line given less room than its text needs: its last rows are laid out and painted nowhere,
/// which reads as a line cut short over empty space. A line with room for its text is no
/// finding.
#[test]
fn a_line_with_less_room_than_its_text_needs_is_caught() {
    let cut = |height: f32| {
        let mut line = ui::widget::RichTextLine::new();
        line.set_wrap(true);
        line.set_text("The new onboarding build is on the test machines.");
        let mut h = ui::headless::FrameHarness::for_root(
            ui::State::new(),
            ui::widget::Container::new()
                .with_width(216.0)
                .with_height(height)
                .with_child(Box::new(line)),
            (320, 200),
            ui::Theme::light(),
        );
        let mut renderer = ui::TextRenderer::new();
        clipped_text(h.root_widget_mut(), &mut renderer, (320, 200)).len()
    };
    assert_eq!(cut(17.5), 1, "one row of a two-row sentence is cut");
    assert_eq!(cut(200.0), 0, "room for the whole sentence is no finding");
}

/// A place on a canvas past its viewport is reached by panning, so it is no finding; the
/// same place in a region of a plain column reaches past it, and is caught.
#[test]
fn a_place_past_a_canvas_viewport_pans_and_past_a_column_overflows() {
    use ui::widget::{LayoutInfo, ManualLayout, ScrollDirection, ScrollLayout};
    let found = |outer: Option<LayoutInfo>| {
        let mut region = node("Manual", "region", [0.0, 0.0, 320.0, 300.0], None);
        region.layout = Some(LayoutInfo::Manual(ManualLayout {
            allocation: [320.0, 300.0],
            requested_bounds: [0.0, 0.0, 320.0, 300.0],
            content_bounds: [0.0, 0.0, 560.0, 300.0],
            overflow: false,
            first_conflict: None,
        }));
        region.children = vec![holding(
            node("Card", "place", [200.0, 40.0, 240.0, 90.0], None),
            vec![node(
                "Text",
                "place-name",
                [204.0, 44.0, 236.0, 86.0],
                Some(Role::StaticText),
            )],
        )];
        let mut view = node("View", "view", [0.0, 0.0, 320.0, 300.0], None);
        view.child_clip = Some([0.0, 0.0, 320.0, 300.0]);
        view.layout = outer;
        view.children = vec![region];
        // The window is wider than the view, so the place stays within the window and only
        // the view's own box is passed.
        inspect(&view, (600, 300), None, 1.0)
            .into_iter()
            .map(|p| p.kind)
            .collect::<Vec<_>>()
    };
    let canvas = Some(LayoutInfo::Scroll(ScrollLayout {
        direction: ScrollDirection::Both,
    }));
    assert_eq!(found(canvas), []);
    assert!(found(None).contains(&Kind::Overflow));
}

/// A header action the shell holds in a frame of its own is the control it frames.
#[test]
fn a_framed_action_over_a_button_is_caught() {
    let mut frame = node("Row", "actions", [316.0, 12.0, 32.0, 32.0], None);
    frame.children = vec![node(
        "Button",
        "new",
        [316.0, 12.0, 32.0, 32.0],
        Some(Role::Button),
    )];
    let found = overlaps(vec![
        node(
            "Button",
            "search",
            [300.0, 12.0, 32.0, 32.0],
            Some(Role::Button),
        ),
        frame,
    ]);
    assert_eq!(found.len(), 1, "{found:?}");
}

/// What a shell layers on purpose is no finding: a column under its header's buttons, and
/// buttons side by side.
#[test]
fn a_shells_columns_and_buttons_side_by_side_are_not() {
    let mut column = node("Column", "content", [0.0, 0.0, 400.0, 300.0], None);
    column.children = vec![
        node("Text", "a", [16.0, 60.0, 100.0, 20.0], None),
        node("Text", "b", [16.0, 90.0, 100.0, 20.0], None),
    ];
    let found = overlaps(vec![
        column,
        node(
            "Button",
            "search",
            [300.0, 12.0, 32.0, 32.0],
            Some(Role::Button),
        ),
        node(
            "Button",
            "new",
            [340.0, 12.0, 32.0, 32.0],
            Some(Role::Button),
        ),
    ]);
    assert!(found.is_empty(), "{found:?}");
}

fn stack_node(id: &str, rect: [f64; 4], children: Vec<InspectionNode>) -> InspectionNode {
    InspectionNode {
        path: id.to_owned(),
        widget: "Stack".to_owned(),
        test_id: Some(id.to_owned()),
        focusable: false,
        rect,
        child_clip: None,
        page_title: None,
        layout: None,
        paint_key: None,
        semantics: None,
        children,
    }
}

fn off_pixel(root: &InspectionNode, scale: f64) -> Vec<Vec<String>> {
    inspect(root, (400, 300), None, scale)
        .into_iter()
        .filter(|problem| problem.kind == Kind::OffPixel)
        .map(|problem| problem.keys)
        .collect()
}

#[test]
fn an_edge_between_device_pixels_is_found_at_the_outermost_box_it_moves() {
    // 20.5 is a device pixel at 2x but not at 1x; 20 + 2/3 is one at 1.5x only.
    let third = 20.0 + 2.0 / 3.0;
    let window = |inner: [f64; 4]| {
        stack_node(
            "window",
            [0.0, 0.0, 400.0, 300.0],
            vec![stack_node(
                "column",
                inner,
                vec![stack_node(
                    "label",
                    [inner[0], inner[1], 50.0, 10.0],
                    vec![],
                )],
            )],
        )
    };
    let half = window([20.5, 10.0, 100.0, 40.0]);
    assert_eq!(
        off_pixel(&half, 1.0),
        vec![vec!["column".to_owned(), "window".to_owned()]],
        "found at the column, not again at the label it moves"
    );
    assert!(off_pixel(&half, 2.0).is_empty());
    assert_eq!(off_pixel(&half, 1.5).len(), 1);

    let thirds = window([third, 10.0, 100.0, 40.0]);
    assert!(off_pixel(&thirds, 1.5).is_empty());
    assert_eq!(off_pixel(&thirds, 2.0).len(), 1);
    assert_eq!(off_pixel(&thirds, 1.0).len(), 1);

    assert_eq!(
        off_pixel(&window([21.0, 10.0, 100.0, 40.0]), 1.5).len(),
        1,
        "a whole logical pixel is not a device pixel at 1.5x"
    );
    assert!(off_pixel(&window([20.0, 10.0, 100.0, 40.0]), 1.5).is_empty());
}

/// A bar's control: `id` in `slot`, named `name`, showing `icon`, asking for `words` wide of
/// them where it is a menu button (`menu`).
fn control(
    slot: &'static str,
    id: &str,
    name: &str,
    icon: Option<&str>,
    words: f64,
    menu: bool,
) -> ui::window_toolbar::BarControl {
    let control = ui::window_toolbar::BarControl::new(slot, id, Rect::new(0.0, 0.0, 32.0, 28.0))
        .named(name)
        .icon(icon.map(str::to_owned))
        .words(words);
    if menu { control.menu() } else { control }
}

fn bar(controls: Vec<ui::window_toolbar::BarControl>, spare: f64) -> ui::window_toolbar::BarLayout {
    ui::window_toolbar::BarLayout {
        spare,
        controls,
        labels: Vec::new(),
    }
}

fn kinds(layout: &ui::window_toolbar::BarLayout) -> Vec<Kind> {
    super::bar(layout)
        .into_iter()
        .map(|problem| problem.kind)
        .collect()
}

/// A window shows one bar: a header row of the app's under the bar is a second row, and the
/// bar is where the app's title, navigation and actions stand.
#[test]
fn a_header_row_under_the_bar_is_a_second_row() {
    let found = super::stacked_header_rows(44.0, 56.0);
    assert_eq!(found.len(), 1, "the bar and the row under it are two rows");
    assert_eq!(found[0].kind, Kind::StackedHeaderRows);
    assert!(
        found[0].detail.contains("44px") && found[0].detail.contains("56px"),
        "the finding says both heights: {}",
        found[0].detail
    );
    assert!(
        super::stacked_header_rows(0.0, 56.0).is_empty(),
        "a row holding the bar leaves no row under it"
    );
    assert!(
        super::stacked_header_rows(44.0, 0.0).is_empty(),
        "an app drawing no row of its own stacks nothing under the bar"
    );
}

/// Every column says what it shows, and offers what acts on it, in a row of its own at the
/// window's top at every time: a window that declares that row and draws none at rest loses it
/// until the person reaches for the window's bar.
#[test]
fn a_column_declares_its_header_and_draws_it_at_rest() {
    let frame = |key: &'static str, x0: f64, header: Rect| ui::widget::ColumnFrame {
        key,
        column: Rect::new(x0, 0.0, x0 + 400.0, 800.0),
        header,
        rail: false,
    };
    let row = |x0: f64| Rect::new(x0, 0.0, x0 + 400.0, 44.0);
    let at_rest = |declared: &[&str], frames: &[ui::widget::ColumnFrame]| {
        super::column_headers_at_rest(declared, frames, 1280.0)
    };
    assert!(
        at_rest(&[], &[frame("overview", 220.0, Rect::ZERO)]).is_empty(),
        "a column the app gives no header is the app's own, drawn or not"
    );
    assert!(
        at_rest(&["overview"], &[frame("overview", 220.0, row(220.0))]).is_empty(),
        "the row that names the column stands at rest"
    );
    assert!(
        at_rest(&["content"], &[frame("content", 1280.0, Rect::ZERO)]).is_empty(),
        "a pushed screen behind the one in front is not on the window"
    );
    let rail = ui::widget::ColumnFrame {
        rail: true,
        ..frame("sidebar", 0.0, Rect::ZERO)
    };
    assert!(
        at_rest(&["sidebar"], &[rail]).is_empty(),
        "a rail says what it holds by its icons"
    );
    let found = at_rest(
        &["sidebar", "overview"],
        &[
            frame("sidebar", 0.0, row(0.0)),
            frame("overview", 400.0, Rect::ZERO),
        ],
    );
    assert_eq!(found.len(), 1, "a declared header that only the bar shows");
    assert_eq!(found[0].kind, Kind::HeaderOnReveal);
    assert!(
        found[0].detail.contains("overview") && found[0].detail.contains("400px"),
        "the finding names the column and how wide it stands: {}",
        found[0].detail
    );
}

/// An action stands once: in its column's header row, never again in the window's bar.
#[test]
fn an_action_the_window_offers_is_never_offered_again_by_its_bar() {
    let control = |name: &str, furniture: bool| ui::window_toolbar::BarControl {
        name: name.to_owned(),
        furniture,
        ..ui::window_toolbar::BarControl::new("action", format!("bar.{name}"), Rect::ZERO)
    };
    let layout = ui::window_toolbar::BarLayout {
        spare: 0.0,
        controls: vec![
            control("Compose", false),
            control("Close", true),
            ui::window_toolbar::BarControl {
                name: "Close".to_owned(),
                ..ui::window_toolbar::BarControl::new("close", "window-control-close", Rect::ZERO)
            },
        ],
        labels: Vec::new(),
    };
    let buttons = vec!["Compose".to_owned(), "Close".to_owned()];
    let found = super::offered_twice(&layout, &buttons);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, Kind::OfferedTwice);
    assert!(found[0].detail.contains("Compose"));
    assert!(
        super::offered_twice(&layout, &["Reply".to_owned()]).is_empty(),
        "an action the window offers nowhere else stands in the bar alone"
    );
}

/// One bar shows each control once: the same action under two ids, or two controls one glyph
/// apart, is one control too many.
#[test]
fn a_bar_shows_each_control_once() {
    let twice = bar(
        vec![
            control(
                "leading",
                "tasks~check",
                "Complete",
                Some("circle-check"),
                0.0,
                false,
            ),
            control(
                "action",
                "page~check",
                "Complete",
                Some("circle-check"),
                0.0,
                false,
            ),
            control(
                "overflow",
                "window-toolbar-more",
                "More",
                Some("more-horizontal"),
                0.0,
                true,
            ),
            control("close", "window-close", "Close", Some("x"), 0.0, false),
        ],
        0.0,
    );
    assert_eq!(
        kinds(&twice),
        vec![Kind::DuplicateBarControl],
        "the two check circles are one control to a person"
    );
}

/// The app's own door beside the bar's own "…": one bar has one.
#[test]
fn an_apps_own_door_beside_the_bars_is_one_door_too() {
    let doors = bar(
        vec![
            control(
                "action",
                "page~more",
                "More actions",
                Some("more-horizontal"),
                0.0,
                true,
            ),
            control(
                "overflow",
                "window-toolbar-more",
                "More",
                Some("more-horizontal"),
                0.0,
                true,
            ),
        ],
        0.0,
    );
    assert_eq!(kinds(&doors), vec![Kind::DuplicateBarControl]);
    // Every control of a window's own bar stands once: its navigation, its "…", its
    // controls.
    let window = bar(
        vec![
            control(
                "sidebar",
                "nav-shell-sidebar",
                "Show sidebar",
                None,
                0.0,
                false,
            ),
            control("back", "nav-shell-back", "Back", None, 0.0, false),
            control(
                "action",
                "page~clear",
                "Clear done",
                Some("circle-check"),
                0.0,
                false,
            ),
            control(
                "overflow",
                "window-toolbar-more",
                "More",
                Some("more-horizontal"),
                0.0,
                true,
            ),
            control("history", "window-history", "History", None, 0.0, false),
            control(
                "minimize",
                "window-minimize",
                "Minimize",
                Some("minus"),
                0.0,
                false,
            ),
            control(
                "maximize",
                "window-maximize",
                "Maximize",
                Some("maximize-2"),
                0.0,
                false,
            ),
            control("close", "window-close", "Close", Some("x"), 0.0, false),
        ],
        400.0,
    );
    assert!(kinds(&window).is_empty(), "{:?}", kinds(&window));
}

/// An action that asks for its words stands with them where the bar has room, and as its
/// icon only where it has not.
#[test]
fn a_bar_says_what_an_action_does_where_it_has_room_for_the_words() {
    let icon = |spare: f64| {
        bar(
            vec![control(
                "action",
                "page~clear",
                "Clear done",
                Some("circle-check"),
                64.0,
                false,
            )],
            spare,
        )
    };
    assert_eq!(kinds(&icon(400.0)), vec![Kind::IconOnlyBarAction]);
    assert!(kinds(&icon(20.0)).is_empty(), "a narrow bar keeps the icon");
    // A menu button says what it opens like any other action where the row has room.
    let menu = bar(
        vec![control(
            "action",
            "page~sort",
            "Sort",
            Some("arrow-up-down"),
            48.0,
            true,
        )],
        400.0,
    );
    assert_eq!(kinds(&menu), vec![Kind::IconOnlyBarAction]);
    // A labelled action stands as wide as its words: the row gave it the room.
    let labelled = bar(
        vec![
            ui::window_toolbar::BarControl::new(
                "action",
                "page~clear",
                Rect::new(0.0, 0.0, 64.0, 28.0),
            )
            .named("Clear done")
            .icon(Some("circle-check".to_owned()))
            .labelled()
            .words(64.0),
        ],
        400.0,
    );
    assert!(kinds(&labelled).is_empty());
    // An action that asks for no words, like a bar's own plus, is its icon alone.
    let quiet = bar(
        vec![control(
            "action",
            "page~add",
            "New task",
            Some("plus"),
            0.0,
            false,
        )],
        400.0,
    );
    assert!(kinds(&quiet).is_empty());
    // The window's own furniture is no action of the app's: the strip's door wears the "+"
    // everyone reads, at its square chip, and says its name under the pointer alone.
    let door =
        ui::window_toolbar::BarControl::new("new-tab", "tab-new", Rect::new(0.0, 0.0, 32.0, 28.0))
            .named("New tab")
            .icon(Some("plus".to_owned()))
            .furniture();
    assert!(kinds(&bar(vec![door], 400.0)).is_empty());
}

/// A choice a list painted wide of what its row shows fills the gutter beside it: the lift
/// the list recorded is read against the row's own box, grown by the row's bleed and no
/// further, so the strip the person sees stands where the row does.
#[test]
fn a_lift_reaching_past_what_its_row_shows_is_a_selection_bleed() {
    let lift = |whole: [f64; 4], content: [f64; 4]| ui::widget::RowLift {
        rect: rect(whole),
        content: rect(content),
        bleed: 8.0,
    };
    let inset = lift([624.0, 100.0, 1280.0, 148.0], [672.0, 100.0, 1232.0, 148.0]);
    assert_eq!(reach_past(inset), Some(40.0), "the gutters either side");
    let around = lift([664.0, 100.0, 1240.0, 148.0], [672.0, 100.0, 1232.0, 148.0]);
    assert_eq!(reach_past(around), None, "a lift grown by the row's bleed");
    let whole = lift([0.0, 0.0, 300.0, 40.0], [0.0, 0.0, 300.0, 40.0]);
    assert_eq!(reach_past(whole), None, "a row showing everything it has");
    let empty = lift([0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(reach_past(empty), None, "no lift at all");
}

/// A chevron folds the words of its row, so it centres on the line they stand on: the step in
/// front of them is the chevron's column, not a second baseline beside it.
#[test]
fn a_chevron_centres_on_the_line_of_the_row_it_folds() {
    use super::text::chevron_off_line;
    let chevron = |rect: [f64; 4], line: [f64; 4]| ui::widget::virtual_list::RowChevron {
        rect: ui::kurbo::Rect::new(rect[0], rect[1], rect[2], rect[3]),
        line: ui::kurbo::Rect::new(line[0], line[1], line[2], line[3]),
    };
    // A row's line, and the chevron in the step in front of its words.
    let line = [46.0, 56.0, 366.0, 84.0];
    assert_eq!(
        chevron_off_line(chevron([24.0, 63.0, 46.0, 77.0], line)),
        None,
        "one line: the chevron and the words share a centre"
    );
    // A chevron riding the taller slot a finger's row takes: 10 px under the words.
    let off = chevron_off_line(chevron([24.0, 73.0, 46.0, 87.0], line))
        .expect("a chevron below its line");
    assert!((off - 10.0).abs() < 0.5, "{off}");
    // A row with no line of its own (nothing laid out) says nothing.
    assert_eq!(
        chevron_off_line(chevron([0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0])),
        None
    );
}

/// The one main action a window declares stands in its bar as the primary button: a bar
/// that shows it as every other action, behind its door, or not at all is what a person
/// cannot tell from the rest of the row.
#[test]
fn a_bar_keeps_its_main_action_as_the_primary_button() {
    let main = ui::window_toolbar::WindowToolbar {
        actions: vec![
            ui::window_toolbar::ToolbarAction::new("new", "plus", "New task")
                .labelled()
                .primary(),
            ui::window_toolbar::ToolbarAction::new("sort", "arrow-up-down", "Sort").labelled(),
        ],
        ..ui::window_toolbar::WindowToolbar::default()
    };
    let shown = bar(
        vec![
            ui::window_toolbar::BarControl::new(
                "action",
                "new",
                ui::kurbo::Rect::new(0.0, 0.0, 71.0, 28.0),
            )
            .named("New task")
            .labelled()
            .primary(),
            ui::window_toolbar::BarControl::new(
                "action",
                "sort",
                ui::kurbo::Rect::new(0.0, 0.0, 71.0, 28.0),
            )
            .named("Sort")
            .labelled(),
        ],
        400.0,
    );
    assert!(
        super::bar_primary(Some(&main), &shown).is_empty(),
        "the main action stands as the primary button"
    );
    // Drawn as every other action, the bar does not say what the window is for.
    let plain = bar(
        vec![
            ui::window_toolbar::BarControl::new(
                "action",
                "new",
                ui::kurbo::Rect::new(0.0, 0.0, 71.0, 28.0),
            )
            .named("New task")
            .labelled(),
        ],
        400.0,
    );
    let found = super::bar_primary(Some(&main), &plain);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, Kind::UnmarkedBarPrimary);
    // Behind the bar's door is no place for it either, and neither is out of the bar.
    let folded = bar(
        vec![
            ui::window_toolbar::BarControl::new(
                "overflow",
                "window-toolbar-more",
                ui::kurbo::Rect::new(0.0, 0.0, 28.0, 28.0),
            )
            .named("More actions")
            .menu(),
        ],
        0.0,
    );
    assert_eq!(
        super::bar_primary(Some(&main), &folded).len(),
        1,
        "an action only in the menu is no main action a person sees"
    );
    let absent = bar(Vec::new(), 0.0);
    assert_eq!(super::bar_primary(Some(&main), &absent).len(), 1);
    // A window that declares no main action has none drawn as one.
    assert!(super::bar_primary(None, &shown).is_empty());
}

/// The bar's words are the bar's, painted where no widget lays them out: two of them drawn in
/// one place are two words a person reads as one string of letters. A pill naming the window
/// where its location already does paints its words on the location's, and the check reads the
/// two labels as one place.
#[test]
fn a_bar_word_painted_where_another_stands_is_two_words_in_one_place() {
    let window = node("TilolangWindow", "window", [0.0, 0.0, 900.0, 600.0], None);
    let title =
        ui::window_toolbar::BarLabel::new("TabTitle", "Täoday", Rect::new(42.0, 14.0, 98.0, 30.0));
    let location = ui::window_toolbar::BarLabel::new(
        "BarLocation",
        "Täoday",
        Rect::new(42.0, 14.0, 132.0, 30.0),
    );
    let found = text_on_text(
        &window,
        (900, 600),
        &[title.clone(), location.clone()],
        44.0,
    );
    assert_eq!(found.len(), 1, "the two words are one finding: {found:?}");
    assert_eq!(found[0].kind, Kind::TextOnText);
    assert_eq!(
        found[0].widget, "BarLocation",
        "the row's location paints over the pill beside it"
    );
    assert!(
        found[0].detail.contains("Täoday"),
        "the finding says which words: {}",
        found[0].detail
    );
    // A window whose only tab shows its icon alone says the name once.
    assert!(
        text_on_text(&window, (900, 600), &[location], 44.0).is_empty(),
        "one word in one place is read"
    );
    // Two words beside each other, each in its own place, are no finding.
    let beside = ui::window_toolbar::BarLabel::new(
        "TabTitle",
        "Täoday",
        Rect::new(140.0, 14.0, 196.0, 30.0),
    );
    assert!(
        text_on_text(&window, (900, 600), &[beside, title], 44.0).is_empty(),
        "a word drawn where another word ends covers nothing"
    );
}

/// The frame's words are read where the window shows them, under the bar: a word at the top of
/// the frame stands below the bar's own words, never over them.
#[test]
fn a_frames_words_stand_under_the_bars() {
    let mut root = node("TilolangWindow", "window", [0.0, 0.0, 900.0, 600.0], None);
    root.children.push(node(
        "Text",
        "page ~title",
        [8.0, 0.0, 180.0, 22.0],
        Some(Role::StaticText),
    ));
    let location =
        ui::window_toolbar::BarLabel::new("BarLocation", "Today", Rect::new(8.0, 4.0, 100.0, 20.0));
    assert!(
        text_on_text(&root, (900, 600), std::slice::from_ref(&location), 44.0).is_empty(),
        "the frame's first line stands at y 44 in the window, below the bar"
    );
    assert!(
        !text_on_text(&root, (900, 600), &[location], 0.0).is_empty(),
        "read at the window's top edge, the two words meet"
    );
}

/// Two words on one card stand apart from the page under it: the card is a ground of its own,
/// and what it covers is behind it — so only words sharing one ground are found.
#[test]
fn words_on_a_ground_of_their_own_stand_apart() {
    let mut root = node("TilolangWindow", "window", [0.0, 0.0, 900.0, 600.0], None);
    root.children.push(node(
        "Text",
        "page ~heading",
        [8.0, 44.0, 200.0, 22.0],
        Some(Role::StaticText),
    ));
    let mut card = node("Card", "page ~card", [8.0, 40.0, 300.0, 120.0], None);
    // A card's own line, over the page's heading behind it.
    card.children.push(node(
        "Text",
        "page ~card ~title",
        [20.0, 44.0, 200.0, 22.0],
        Some(Role::StaticText),
    ));
    root.children.push(card);
    assert!(
        text_on_text(&root, (900, 600), &[], 0.0).is_empty(),
        "the card's ground holds its words away from the page's"
    );
}

/// A rounded surface stands inside its column with a gutter on every side: one glued to the
/// column's edges, cut by them, or one standing on the window's edge where no column holds it,
/// is found — and the same surface a gutter in is no finding.
#[test]
fn a_rounded_surface_keeps_a_gutter_from_its_column_and_the_window() {
    let shell_with = |banner: InspectionNode| {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 900.0, 600.0], None);
        let mut column = node("Stack", "column", [280.0, 0.0, 620.0, 600.0], None);
        column.children.push(banner);
        shell.children.push(column);
        shell
    };
    let found = |root: &InspectionNode| -> Vec<Problem> {
        inspect(root, (900, 600), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::RoundedOnEdge)
            .collect()
    };

    let glued = found(&shell_with(node_of(
        "Banner",
        "banner",
        [280.0, 0.0, 620.0, 65.0],
    )));
    assert_eq!(glued.len(), 1, "{glued:?}");
    assert!(
        glued[0].detail.contains("left") && glued[0].detail.contains("right"),
        "both sides stand on the column's: {}",
        glued[0].detail
    );
    assert!(
        glued[0].detail.contains("its column"),
        "the column is what it stands on: {}",
        glued[0].detail
    );

    let inset = found(&shell_with(node_of(
        "Banner",
        "banner",
        [304.0, 24.0, 572.0, 65.0],
    )));
    assert!(inset.is_empty(), "a gutter on every side: {inset:?}");

    // A card is rounded too, and a surface without rounded corners is no finding.
    assert_eq!(
        found(&shell_with(node_of(
            "Card",
            "card",
            [280.0, 0.0, 620.0, 65.0]
        )))
        .len(),
        1
    );
    assert!(
        found(&shell_with(node_of(
            "Stack",
            "box",
            [280.0, 0.0, 620.0, 65.0]
        )))
        .is_empty()
    );

    // With no column around it, the window's edge is the one it must keep clear of.
    let mut root = node("TilolangWindow", "window", [0.0, 0.0, 900.0, 600.0], None);
    root.children
        .push(node_of("Banner", "banner", [0.0, 0.0, 900.0, 65.0]));
    let on_window = found(&root);
    assert_eq!(on_window.len(), 1, "{on_window:?}");
    assert!(
        on_window[0].detail.contains("the window"),
        "{}",
        on_window[0].detail
    );
}

/// A card a page scrolls is no card on the frame: its top and bottom are where the scroll stands,
/// so a card whose last row ends within a pixel of the column's floor — or one the floor cuts on
/// its way past — is no finding, while its sides are still the layout's to keep clear of the
/// column.
#[test]
fn a_card_a_page_scrolls_is_no_card_on_the_edge() {
    let column_with = |card: InspectionNode| {
        let mut page = node("Page", "page", [0.0, 66.0, 752.0, 4107.5], None);
        page.children.push(card);
        let mut scroll = node_of("Scrollable", "scroll", [0.0, 66.0, 768.0, 958.0]);
        scroll.child_clip = Some([0.0, 66.0, 768.0, 958.0]);
        scroll.children.push(page);
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 768.0, 1024.0], None);
        shell.children.push(scroll);
        let mut root = node("TilolangWindow", "window", [0.0, 0.0, 768.0, 1024.0], None);
        root.children.push(shell);
        root
    };
    let found = |root: &InspectionNode| -> Vec<Problem> {
        inspect(root, (768, 1024), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::RoundedOnEdge)
            .collect()
    };

    assert!(
        found(&column_with(node_of(
            "Card",
            "card",
            [24.0, 777.5, 344.0, 245.5]
        )))
        .is_empty(),
        "a card ending a pixel from the floor is on its way past"
    );
    assert!(
        found(&column_with(node_of(
            "Card",
            "card",
            [24.0, 700.0, 344.0, 400.0]
        )))
        .is_empty(),
        "and one the floor cuts through is too"
    );
    // Its sides are the layout's to keep clear: a card across the page's whole width is one on
    // the frame, however it is scrolled.
    let across = found(&column_with(node_of(
        "Card",
        "card",
        [0.0, 777.5, 752.0, 245.5],
    )));
    assert_eq!(across.len(), 1, "{across:?}");
    assert!(
        across[0].detail.contains("left"),
        "its side stands on the column's: {}",
        across[0].detail
    );
}

/// A card keeps a sibling's gap from what stands over and under it: a notice glued to the list
/// under it, or a card pressed against the one above it, is found — and the same card with the
/// gap the toolkit's rhythm gives is no finding. A box beside it in a row is no neighbour.
#[test]
fn a_card_keeps_a_gap_from_its_neighbours() {
    let column_with = |children: Vec<InspectionNode>| {
        let mut column = node("Stack", "column", [280.0, 0.0, 620.0, 600.0], None);
        column.children = children;
        let mut root = node("TilolangWindow", "window", [0.0, 0.0, 900.0, 600.0], None);
        root.children.push(column);
        root
    };
    let found = |root: &InspectionNode| -> Vec<Problem> {
        inspect(root, (900, 600), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::CrampedCard)
            .collect()
    };

    // A notice standing on the list under it, with the page's gap above it.
    let glued = found(&column_with(vec![
        node_of("Banner", "notice", [304.0, 24.0, 572.0, 65.0]),
        node_of("DescribedList", "list", [304.0, 89.0, 572.0, 400.0]),
    ]));
    assert_eq!(glued.len(), 1, "{glued:?}");
    assert!(
        glued[0].detail.contains("below") && glued[0].detail.contains("0 px"),
        "it stands on the list under it: {}",
        glued[0].detail
    );
    assert_eq!(
        glued[0].id.as_deref(),
        Some("notice"),
        "the card is what the finding is about"
    );

    // The gap the toolkit gives cards and rows of a list: nothing to find.
    assert!(
        found(&column_with(vec![
            node_of("Banner", "notice", [304.0, 24.0, 572.0, 65.0]),
            node_of("DescribedList", "list", [304.0, 101.0, 572.0, 400.0]),
        ]))
        .is_empty(),
        "a sibling's gap under the notice"
    );

    // A card pressed against the card above it is the same defect from the other side.
    let stacked = found(&column_with(vec![
        node_of("Card", "first", [304.0, 24.0, 572.0, 80.0]),
        node_of("Card", "second", [304.0, 104.0, 572.0, 80.0]),
    ]));
    assert_eq!(stacked.len(), 2, "{stacked:?}");
    assert!(
        stacked
            .iter()
            .any(|problem| problem.detail.contains("above")),
        "the card under it stands on the one over it: {stacked:?}"
    );

    // A card beside a box, in a row, has no neighbour over or under it.
    let mut along = node("Row", "row", [280.0, 0.0, 620.0, 120.0], None);
    along.children = vec![
        node_of("Card", "card", [280.0, 0.0, 300.0, 120.0]),
        node_of("Notice", "text", [300.0, 0.0, 600.0, 120.0]),
    ];
    let mut root = node("TilolangWindow", "window", [0.0, 0.0, 900.0, 600.0], None);
    root.children.push(along);
    assert!(found(&root).is_empty(), "nothing stands over or under it");
}

/// A row of a list, named `title`, standing at `rect`: `open` a row grown into its document.
fn list_row(title: &str, rect: [f64; 4], open: bool) -> InspectionNode {
    let mut row = node("SwipeRow", title, rect, Some(Role::ListItem));
    let access = row.semantics.as_mut().expect("the row's semantics");
    access.name = Some(title.to_owned());
    access.state.expanded = open.then_some(true);
    row
}

/// A row of an outline inside a document, named `title`.
fn document_row(title: &str, rect: [f64; 4]) -> InspectionNode {
    let mut row = node("ListRow", title, rect, Some(Role::ListItem));
    row.semantics.as_mut().expect("the row's semantics").name = Some(title.to_owned());
    row
}

/// What a document holds: the outline of the items under the row standing open.
fn document(rows: Vec<InspectionNode>, rect: [f64; 4]) -> InspectionNode {
    let mut tree = node("TreeView", "subtasks", rect, Some(Role::Tree));
    tree.children = rows;
    tree
}

/// An item stands once: the document of a row standing open that lists an item the list
/// shows again is the same item twice, while a document listing items of its own is not.
#[test]
fn an_item_shown_in_an_open_rows_document_and_again_in_the_list_is_caught() {
    let held = |rows: Vec<InspectionNode>| {
        let mut list = node(
            "DescribedList",
            "list",
            [280.0, 0.0, 1200.0, 600.0],
            Some(Role::List),
        );
        list.children = rows;
        let mut root = node("TilolangWindow", "window", [0.0, 0.0, 1280.0, 756.0], None);
        root.children.push(list);
        inspect(&root, (1280, 756), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::ItemShownTwice)
            .collect::<Vec<Problem>>()
    };

    // The row standing open lists two steps, and the list shows one of them again.
    let mut open = list_row("Call the landlord", [304.0, 100.0, 952.0, 400.0], true);
    open.children = vec![document(
        vec![
            document_row("Find the service date", [344.0, 260.0, 912.0, 28.0]),
            document_row("Ask about Tuesday", [344.0, 288.0, 912.0, 28.0]),
        ],
        [344.0, 260.0, 912.0, 56.0],
    )];
    let twice = held(vec![
        list_row("Book a haircut", [304.0, 64.0, 952.0, 36.0], false),
        open,
        list_row("Find the service date", [304.0, 500.0, 952.0, 36.0], false),
    ]);
    assert_eq!(twice.len(), 1, "{twice:?}");
    assert!(
        twice[0].detail.contains("Find the service date")
            && twice[0].detail.contains("Call the landlord"),
        "the finding names the item and the row it stands in twice: {}",
        twice[0].detail
    );

    // A document listing steps of its own, none of them a row of the list: one place each.
    let mut open = list_row("Call the landlord", [304.0, 100.0, 952.0, 400.0], true);
    open.children = vec![document(
        vec![
            document_row("Find the service date", [344.0, 260.0, 912.0, 28.0]),
            document_row("Ask about Tuesday", [344.0, 288.0, 912.0, 28.0]),
        ],
        [344.0, 260.0, 912.0, 56.0],
    )];
    assert!(
        held(vec![
            list_row("Book a haircut", [304.0, 64.0, 952.0, 36.0], false),
            open,
            list_row("Buy oat milk", [304.0, 500.0, 952.0, 36.0], false),
        ])
        .is_empty(),
        "nothing of the document stands in the list again"
    );

    // A page's document naming the item it was made from, as one reference row: it is no row
    // of a list, and no finding.
    let mut open = list_row("Call the landlord", [304.0, 100.0, 952.0, 400.0], true);
    let mut section = node("Section", "linked", [344.0, 260.0, 912.0, 60.0], None);
    section.children = vec![node_of("ListRow", "reference", [344.0, 260.0, 912.0, 36.0])];
    section.children[0]
        .semantics
        .get_or_insert_with(|| AccessNode::new(Role::ListItem))
        .name = Some("Book a haircut".to_owned());
    open.children = vec![section];
    assert!(
        held(vec![
            open,
            list_row("Book a haircut", [304.0, 500.0, 952.0, 36.0], false),
        ])
        .is_empty(),
        "a reference on its own is no row of a list"
    );

    // A branch of a tree whose rows show is expanded and holds no document: no finding.
    let mut branch = node(
        "ListRow",
        "branch",
        [304.0, 64.0, 952.0, 36.0],
        Some(Role::ListItem),
    );
    let access = branch.semantics.as_mut().expect("the row's semantics");
    access.name = Some("Outline".to_owned());
    access.state.expanded = Some(true);
    assert!(
        held(vec![
            branch,
            list_row("First draft", [304.0, 100.0, 952.0, 36.0], false),
        ])
        .is_empty(),
        "an expanded branch is a row of its list, and its document holds no row"
    );
}

/// A group heading takes one band over its first row, and a finger's target where the window is
/// held in the hand — never a slot or padding of its own between the heading and the row.
#[test]
fn a_group_heading_takes_one_band_and_stands_on_the_row_it_heads() {
    let theme = ui::Theme::dark();
    let band = band_height(&theme);
    let with = |band: f64, row_top: f64| {
        let mut list = node_of("DescribedList", "list", [0.0, 0.0, 400.0, 600.0]);
        let mut access = AccessNode::new(Role::Tree);
        access.children = vec![
            AccessNode::new(Role::Button)
                .with_name("Monday")
                .with_action_target("group:Monday")
                .with_bounds(Rect::new(0.0, 0.0, 400.0, band)),
            AccessNode::new(Role::TreeItem)
                .with_name("a task")
                .with_bounds(Rect::new(0.0, row_top, 400.0, row_top + 48.0)),
        ];
        list.semantics = Some(access);
        list
    };
    assert!(
        heading_rooms(&with(band, band), &theme, None).is_empty(),
        "one band, the row at its end"
    );
    let loose = heading_rooms(&with(band + 12.0, band + 12.0), &theme, None);
    assert!(
        loose
            .iter()
            .any(|p| p.kind == Kind::HeadingRoom && p.detail.contains("takes")),
        "a band taller than a heading band: {loose:?}"
    );
    let slot = heading_rooms(&with(band, band + 12.0), &theme, None);
    assert!(
        slot.iter()
            .any(|p| p.kind == Kind::HeadingRoom && p.detail.contains("heads stands")),
        "a slot between the heading and its row: {slot:?}"
    );
    assert!(
        heading_rooms(&with(48.0, 48.0), &theme, Some(48.0)).is_empty(),
        "a finger's target is the one room a band grows by"
    );
}

/// The first band a list that heads its rows draws — the heading over its first run, or the first
/// row where the list leaves that heading out — stands on the column's content gutter, where a
/// page's first block stands: one closer to the box over the list hugs it. A list whose rows name
/// no group is one block and leads with its rows.
#[test]
fn a_list_that_heads_its_rows_starts_its_first_band_on_the_content_gutter() {
    let theme = ui::Theme::dark();
    let inset = ui::chrome::column_gutter(&theme);
    let list = |heads_rows: bool, lead: f64| {
        let mut list = node_of("VirtualList", "list", [100.0, 40.0, 400.0, 600.0]);
        list.layout = Some(ui::widget::LayoutInfo::List(ui::widget::ListLayout {
            heads_rows,
            lead: ui::num::f32_of_f64(lead),
        }));
        let mut access = AccessNode::new(Role::List);
        access.children = vec![
            AccessNode::new(Role::ListItem)
                .with_name("Water the plants")
                .with_bounds(Rect::new(100.0, 40.0 + lead, 500.0, 88.0 + lead)),
            AccessNode::new(Role::ListItem)
                .with_name("Call the plumber")
                .with_bounds(Rect::new(100.0, 88.0 + lead, 500.0, 136.0 + lead)),
        ];
        list.semantics = Some(access);
        list
    };
    assert!(
        heading_rooms(&list(true, inset), &theme, None).is_empty(),
        "a content gutter of room over the first row"
    );
    let hugging = heading_rooms(&list(true, 0.0), &theme, None);
    assert!(
        hugging.iter().any(|p| p.kind == Kind::HuggingRow),
        "a list that heads its rows with none of the content gutter over them: {hugging:?}"
    );
    assert!(
        heading_rooms(&list(true, band_height(&theme)), &theme, None).is_empty(),
        "a heading's own band is room enough over the row it heads"
    );
    assert!(
        heading_rooms(&list(false, 0.0), &theme, None).is_empty(),
        "a list whose rows name no group leads with them"
    );
    // The heading the list draws is held to the same inset, whatever room the list states.
    let heading_over = |top: f64| {
        let mut list = list(true, inset);
        let mut access = AccessNode::new(Role::List);
        access.children = vec![
            AccessNode::new(Role::Heading { level: 3 })
                .with_name("Overdue")
                .with_bounds(Rect::new(100.0, top, 400.0, top + band_height(&theme)))
                .with_action_target("group:Overdue"),
            AccessNode::new(Role::ListItem)
                .with_name("Water the plants")
                .with_bounds(Rect::new(
                    100.0,
                    top + band_height(&theme),
                    500.0,
                    top + band_height(&theme) + 48.0,
                )),
        ];
        list.semantics = Some(access);
        list
    };
    assert!(
        heading_rooms(&heading_over(40.0 + inset), &theme, None).is_empty(),
        "a heading standing on the content gutter"
    );
    let hugging_heading = heading_rooms(&heading_over(40.0), &theme, None);
    assert!(
        hugging_heading.iter().any(|p| p.kind == Kind::HuggingRow),
        "a heading hugging the box over the list: {hugging_heading:?}"
    );
}

/// A page's body stands under the text a control beside its heading leads to only where the
/// column has the room: on a phone's screen it starts on the gutter, and a section standing a
/// control's width in is what a person sees the page lose on every line.
#[test]
fn a_page_on_a_phone_screen_starts_its_body_on_the_gutter() {
    let theme = ui::Theme::dark();
    let page = |width: f64, section_x: f64, section_width: f64| {
        let mut page = node_of("Page", "page", [0.0, 56.0, width, 700.0]);
        let mut stack = node_of("Stack", "blocks", [24.0, 80.0, width - 48.0, 600.0]);
        stack.children.push(node_of(
            "Section",
            "section",
            [section_x, 200.0, section_width, 85.0],
        ));
        page.children.push(stack);
        page
    };
    // A control's cell and the gap after it, under a fine pointer.
    let lead = 32.0 + f64::from(theme.tokens.spacing.rhythm.get(ui::tokens::Rhythm::Related));
    let phone = indented_bodies(&page(390.0, 24.0 + lead, 302.0), &theme, None);
    assert_eq!(phone.len(), 1, "{phone:?}");
    assert_eq!(phone[0].kind, Kind::IndentedBody);
    assert!(
        phone[0].detail.contains("starts its body"),
        "{}",
        phone[0].detail
    );
    assert!(
        indented_bodies(&page(390.0, 24.0, 342.0), &theme, None).is_empty(),
        "a body on the gutter loses nothing"
    );
    assert!(
        indented_bodies(&page(900.0, 24.0 + lead, 800.0), &theme, None).is_empty(),
        "a column has the room the indent takes"
    );
    // A cell a grid places far across the page is no body indent, however narrow it is.
    let mut wide = page(1280.0, 24.0, 1224.0);
    wide.children[0]
        .children
        .push(node_of("Section", "cell", [869.0, 200.0, 371.0, 85.0]));
    assert!(
        indented_bodies(&wide, &theme, None).is_empty(),
        "a grid's cell stands where its column does"
    );
}

/// A presence name tag covers no word of the window, and one actor wears one tag.
#[test]
fn a_presence_tag_covers_no_word_and_one_actor_wears_one() {
    let title = || {
        let mut title = node(
            "Text",
            "title",
            [40.0, 10.0, 120.0, 20.0],
            Some(Role::StaticText),
        );
        title.semantics = title
            .semantics
            .map(|access| access.with_name("Buy oat milk"));
        title
    };
    let tag = |at: [f64; 4], index: usize| {
        let mut tag = node_of(ui::presence::TAG_WIDGET, "presence-tag-agent:helper", at);
        tag.path = format!("presence-tag-agent:helper-{index}");
        tag
    };
    let mut window = node_of("Window", "window", [0.0, 0.0, 400.0, 200.0]);
    window.children = vec![title(), tag([40.0, 40.0, 80.0, 18.0], 0)];
    let kinds = |root: &InspectionNode| {
        inspect(root, (400, 200), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::PresenceTag)
            .count()
    };
    assert_eq!(kinds(&window), 0, "a tag clear of the words is fine");
    window.children = vec![title(), tag([60.0, 14.0, 80.0, 18.0], 0)];
    assert_eq!(kinds(&window), 1, "a tag over the title is a fault");
    window.children = vec![
        title(),
        tag([40.0, 40.0, 80.0, 18.0], 0),
        tag([40.0, 80.0, 80.0, 18.0], 1),
    ];
    assert_eq!(kinds(&window), 1, "a second tag of one actor is a fault");
}

/// A list drawing one group heading over rows, under the title its window reads.
fn grouped_list(title: &str, count: u32, heading: &str, rows: u32) -> InspectionNode {
    grouped_list_of(title, count, heading, rows, 0)
}

/// A list drawing one group heading over `rows` rows, `done` of them checked off, under the
/// title its window reads.
fn grouped_list_of(title: &str, count: u32, heading: &str, rows: u32, done: u32) -> InspectionNode {
    let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 700.0], None);
    let mut list = node(
        "VirtualList",
        "list",
        [0.0, 40.0, 400.0, 690.0],
        Some(Role::List),
    );
    let mut head = AccessNode::new(Role::Button);
    head.name = Some(heading.to_owned());
    head.value = Some(count.to_string());
    head.action_target = Some(format!("group:{heading}"));
    head.bounds = Some(ui::a11y::AccessBounds {
        x0: 0.0,
        y0: 48.0,
        x1: 400.0,
        y1: 68.0,
    });
    let mut children = vec![head];
    for row in 0..rows {
        let mut item = AccessNode::new(Role::ListItem);
        if row < done {
            item.state.checked = Some(true);
        }
        let top = 68.0 + f64::from(row) * 40.0;
        item.bounds = Some(ui::a11y::AccessBounds {
            x0: 0.0,
            y0: top,
            x1: 400.0,
            y1: top + 40.0,
        });
        children.push(item);
    }
    list.semantics
        .as_mut()
        .expect("the list's semantics")
        .children = children;
    shell.children = vec![
        list,
        counted(
            "NavShellTitle",
            "title",
            [8.0, 8.0, 200.0, 32.0],
            Role::Heading { level: 2 },
            title,
            &count.to_string(),
        ),
    ];
    stack_node("window", [0.0, 0.0, 400.0, 700.0], vec![shell])
}

/// A card titled `heading` over a row whose trailing fact says `fact`.
fn day_fact_card(heading: Option<&str>, fact: &str) -> InspectionNode {
    let mut row = node(
        "ListRow",
        "row",
        [8.0, 48.0, 384.0, 40.0],
        Some(Role::ListItem),
    );
    {
        let access = row.semantics.as_mut().expect("the row's semantics");
        access.name = Some("Water the plants".to_owned());
        access.value = Some(fact.to_owned());
    }
    let mut children = Vec::new();
    if let Some(heading) = heading {
        let mut header = node(
            "SectionHeader",
            "header",
            [8.0, 8.0, 200.0, 32.0],
            Some(Role::Heading { level: 3 }),
        );
        header
            .semantics
            .as_mut()
            .expect("the header's semantics")
            .name = Some(heading.to_owned());
        children.push(header);
    }
    children.push(row);
    let mut card = node("Card", "card", [0.0, 0.0, 400.0, 200.0], Some(Role::Group));
    card.children = children;
    stack_node("window", [0.0, 0.0, 400.0, 700.0], vec![card])
}

/// A row whose trailing fact names the day the card over it names is caught; a fact naming
/// another day, or a row under no heading at all, is not.
#[test]
fn a_rows_day_fact_naming_the_day_the_heading_says_is_caught() {
    let repeats = |heading: Option<&str>, fact: &str| {
        inspect(&day_fact_card(heading, fact), (400, 700), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::RepeatedDayFact)
            .count()
    };
    assert_eq!(
        repeats(Some("Today"), "Today"),
        1,
        "the card says Today, and the row says it again"
    );
    assert_eq!(
        repeats(Some("Today"), "Today 09:00"),
        1,
        "the row keeps its time of day, and gives the day back to the heading"
    );
    assert_eq!(
        repeats(Some("Today"), "Tomorrow"),
        0,
        "a row due another day says something the card does not"
    );
    assert_eq!(
        repeats(Some("Fieldwork"), "Fieldwork"),
        0,
        "a card named for a thing says nothing of when anything is due"
    );
    assert_eq!(
        repeats(None, "Today"),
        0,
        "under no heading the row's day is the row's to say"
    );
}

/// A list whose one group heading says what the title over it already says is caught; a heading
/// saying something else, or counting other rows, is not.
#[test]
fn a_lone_group_heading_repeating_the_titles_words_is_caught() {
    let repeated = |root: &InspectionNode| {
        inspect(root, (400, 700), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::RepeatedGroupTitle)
            .count()
    };
    assert_eq!(
        repeated(&grouped_list("Today", 2, "Today", 2)),
        1,
        "the title says Today 2, and the heading over the rows says it again"
    );
    assert_eq!(
        repeated(&grouped_list("Upcoming", 2, "Tomorrow", 2)),
        0,
        "a heading naming another day says something the title does not"
    );
    assert_eq!(
        repeated(&grouped_list("Today", 3, "Today", 2)),
        0,
        "a title counting other rows than the heading does is no repeat"
    );
    assert_eq!(
        repeated(&grouped_list("Today", 2, "Today", 1)),
        0,
        "a heading counting rows the list does not show heads part of it"
    );
    // A heading counts what is left to do: three rows under it, one of them finished, read as
    // the two the title counts, and say what the title says.
    assert_eq!(
        repeated(&grouped_list_of("Today", 2, "Today", 3, 1)),
        1,
        "the heading counts the two rows left, as the title does"
    );
    assert_eq!(
        repeated(&grouped_list_of("Today", 3, "Today", 3, 1)),
        0,
        "a title counting every row says something else than the two left"
    );
}
