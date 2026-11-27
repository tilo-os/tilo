use super::*;
use taffy::{Size, Style};
use tilo_ui_app::widget::{FilterCount, Filterable};
use tilo_ui_base::component::Described;

fn labelled(id: &'static str, label: &str, icon: Icon) -> Element {
    Box::new(Button::new(id, label).with_icon(icon).on_open(|| {}))
}

fn icon(id: &'static str, icon: Icon, name: &str) -> Element {
    Box::new(Button::icon_only(id, icon, name).on_open(|| {}))
}

fn part_rect(h: &Driver, part: Part) -> Rect {
    h.root::<NavShell>()
        .test_part(part)
        .map_or(Rect::ZERO, Widget::get_rect)
}

fn exposed_ids(h: &Driver) -> Vec<String> {
    h.root::<NavShell>()
        .children()
        .iter()
        .filter_map(|child| child.test_id().map(str::to_owned))
        .collect()
}

fn titled_content(actions: Vec<Element>) -> NavShell {
    NavShell::new()
        .content(TestLeaf::fill("content").boxed().0)
        .content_header("Quarterly planning: first draft", actions)
}

tilo_test_env::scenario! {
    fn a_header_row_gives_its_title_and_actions_the_area_they_need;


    // The bar follows the actions it is given in place: a window that shows something else shows
    // its actions from the first layout that sees them, each placed, never the ones before.
    phase a_header_takes_new_actions_in_place;
    {
        let shell = NavShell::new()
            .overview(TestLeaf::fill("overview").boxed().0)
            .overview_header(
                "Inbox",
                vec![
                    labelled("one", "One", Icon::Plus),
                    icon("two", Icon::Send, "Two"),
                ],
            );
        let mut h = harness(shell, 1024, 700, Theme::dark());
        settle(&mut h, 1024, 700);
        assert!(
            (0..2).all(|i| part_rect(&h, Part::Action(Column::Overview, i)).width() > 0.0),
            "the window's own actions stand in its bar"
        );
        let next = vec![
            icon("three", Icon::Send, "Three"),
            labelled("four", "Four", Icon::Plus),
            Box::new(Button::new("five", "Five").in_menu().on_open(|| {})) as Element,
            Box::new(Button::new("six", "Six").in_menu().on_open(|| {})) as Element,
        ];
        {
            let mut shell = h.root_mut::<NavShell>();
            let views = shell
                .part_views("overview_actions")
                .expect("the shell changes its header's actions in place");
            for index in (0..views.len()).rev() {
                views.remove(index);
            }
            for (index, view) in next.into_iter().enumerate() {
                views.insert(index, view);
            }
        }
        settle(&mut h, 1024, 700);
        for i in 0..2 {
            assert!(
                part_rect(&h, Part::Action(Column::Overview, i)).width() > 0.0,
                "action {i} of the new set stands in the bar"
            );
        }
        assert_eq!(
            h.root::<NavShell>()
                .test_part(Part::Action(Column::Overview, 0))
                .and_then(Widget::accessibility)
                .and_then(|node| node.name)
                .as_deref(),
            Some("Three"),
            "the bar shows what arrived, not what it showed"
        );
        // What the bar keeps in its closed More menu stands nowhere: a reader, an agent and a
        // press by name find no control for it until the menu is open.
        let names: Vec<_> = h
            .controls()
            .into_iter()
            .filter_map(|control| control.name)
            .collect();
        assert!(
            names.iter().any(|name| name == "Three")
                && !names.iter().any(|name| name == "Five" || name == "Six"),
            "the closed menu's actions are no controls: {names:?}"
        );
    }

    phase every_column_title_changes_in_place_and_a_bound_title_follows_its_state;
    {
        let title = LocalState::new("Inbox".to_owned());
        let shell = NavShell::new()
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .content(TestLeaf::fill("content").boxed().0)
            .inspector(TestLeaf::fill("inspector").boxed().0)
            .content_header_bound(&title, Vec::new())
            .overview_count(3);
        let mut h = harness(shell, 1400, 800, Theme::dark());
        settle(&mut h, 1400, 800);
        let heading = |h: &Driver, column: Column| {
            h.root::<NavShell>()
                .test_part(Part::Title(column))
                .and_then(Widget::accessibility)
                .filter(|_| {
                    h.root::<NavShell>()
                        .test_laid_out_parts()
                        .contains(&Part::Title(column))
                })
                .and_then(|node| node.name)
        };
        assert_eq!(heading(&h, Column::Content).as_deref(), Some("Inbox"));
        h.root_mut::<NavShell>().set_sidebar_title("Mail");
        h.root_mut::<NavShell>().set_inspector_title("Details");
        h.root_mut::<NavShell>().set_content_count(Some(12));
        title.set("Archive".to_owned());
        let doc = automerge::AutoCommit::new();
        h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
        settle(&mut h, 1400, 800);
        assert_eq!(heading(&h, Column::Sidebar).as_deref(), Some("Mail"));
        assert_eq!(heading(&h, Column::Inspector).as_deref(), Some("Details"));
        assert_eq!(heading(&h, Column::Content).as_deref(), Some("Archive"));
        let count = h
            .root::<NavShell>()
            .test_part(Part::Title(Column::Content))
            .and_then(Widget::accessibility)
            .and_then(|node| node.value);
        assert_eq!(
            count.as_deref(),
            Some("12"),
            "the count reads after the title"
        );
    }



    // The row's order of giving way: an action's label gives way for its icon, then what else stands
    // in the row for the menu's API, and only then is the title cut — the primary action never
    // leaves the row, whatever the row's width and whatever language the title speaks.
    phase a_title_is_cut_only_once_its_actions_have_given_what_they_can;
    {
        let theme = Theme::dark();
        let title = "Launch plan for the autumn release";
        let needs = |title: &str| {
            let (w, _) =
                test_measure_title(&mut TextRenderer::new(), &theme, title, None);
            w
        };
        let actions = || {
            vec![
                icon("sort", Icon::ArrowUpDown, "Sort"),
                Box::new(
                    Button::new("new", "New note")
                        .with_icon(Icon::Plus)
                        .role(tilo_ui_app::widget::ButtonRole::Primary)
                        .on_open(|| {}),
                ) as Element,
                labelled("pin", "Pin", Icon::Pin),
                Box::new(
                    Button::new("delete", "Delete note")
                        .with_icon(Icon::Trash2)
                        .in_menu()
                        .on_open(|| {}),
                ) as Element,
            ]
        };
        // Every row width, in the words of two languages: the title is never cut while a label
        // stands in the row that could be an icon, and the primary action is never missing.
        for width in [1024, 700, 560, 460, 380, 300, 240, 200] {
            for title in [
                title,
                "Sammelplan für die Herbstausgabe und alles, was er trägt",
            ] {
                let mut h = harness(
                    NavShell::new()
                        .content(TestLeaf::fill("content").boxed().0)
                        .content_header(title, actions()),
                    width,
                    700,
                    theme.clone(),
                );
                settle(&mut h, width, 700);
                let shown = part_rect(&h, Part::Title(Column::Content));
                let primary = part_rect(&h, Part::Action(Column::Content, 1));
                assert!(
                    primary.width() > 0.0,
                    "at {width} the primary action left the row ({primary:?})"
                );
                let edge = h.root::<NavShell>().get_rect().x1;
                for part in (0..4)
                    .map(|i| Part::Action(Column::Content, i))
                    .chain([Part::More(Column::Content)])
                {
                    let placed = part_rect(&h, part);
                    assert!(
                        placed.x1 <= edge + 0.5,
                        "at {width} {part:?} runs past the row: {placed:?} beside {edge:.0}"
                    );
                }
                let cut = shown.width() + 1.0 < needs(title);
                if cut {
                    let shell = h.root::<NavShell>();
                    let labelled = (0..shell.test_shown_actions(Column::Content)).any(|i| {
                        shell
                            .test_part(Part::Action(Column::Content, i))
                            .and_then(|action| button_of(action))
                            .is_some_and(|button| {
                                button.shrinks_to_icon() && !button.is_shown_compact()
                            })
                    });
                    assert!(
                        !labelled,
                        "at {width} {title:?} is cut to {:.0} of {:.0} while a label stands in the row",
                        shown.width(),
                        needs(title)
                    );
                }
            }
        }
    }





    // A list offers its actions again as what it shows changes: the row takes the ones it is
    // handed now, so an action that arrives with the icon it draws gives its label up for the
    // icon, and the title keeps its words instead of being cut beside it.
    phase a_row_gives_its_labels_up_when_its_actions_are_offered_again;
    {
        let theme = Theme::dark();
        let title = "Quarterly planning for the autumn release, and the review after it";
        let primary = |icon: Option<Icon>| {
            let button = Button::new("new", "New note")
                .role(tilo_ui_app::widget::ButtonRole::Primary)
                .on_open(|| {});
            Box::new(match icon {
                Some(icon) => button.with_icon(icon),
                None => button,
            }) as Element
        };
        let shell = NavShell::new()
            .content(TestLeaf::fill("content").boxed().0)
            .content_header(
                title,
                vec![labelled("pin", "Pin to the top", Icon::Pin), primary(None)],
            );
        let mut h = harness(shell, 560, 700, theme);
        settle(&mut h, 560, 700);
        let before = part_rect(&h, Part::Title(Column::Content));
        let stood = part_rect(&h, Part::Action(Column::Content, 1));
        assert!(
            stood.width() > stood.height() * 1.5,
            "an action with no icon to give up stands labelled ({stood:?})"
        );

        h.root_mut::<NavShell>().set_content_header(
            title,
            vec![
                labelled("pin", "Pin to the top", Icon::Pin),
                primary(Some(Icon::Plus)),
            ],
        );
        settle(&mut h, 560, 700);
        let action = part_rect(&h, Part::Action(Column::Content, 1));
        assert!(
            action.width() < action.height() * 1.5,
            "the action gave its label up ({action:?})"
        );
        let after = part_rect(&h, Part::Title(Column::Content));
        assert!(
            after.width() > before.width() + (stood.width() - action.width()) / 2.0,
            "the title took the area the label gave up ({before:?} then {after:?})"
        );
    }

    // Labelled buttons collapse to their icons when the row is short; then
    // the rest move into the More menu, and choosing one there runs it.
    phase actions_collapse_to_icons_then_overflow_into_more;
    {
        let deleted = SharedRc::new(Cell::new(false));
        let actions = || {
            let deleted = SharedRc::clone(&deleted);
            vec![
                labelled("reply", "Reply to everyone", Icon::Reply),
                labelled("forward", "Forward", Icon::Send),
                labelled("archive", "Archive", Icon::Archive),
                Box::new(
                    Button::new("delete", "Delete message")
                        .with_icon(Icon::Trash2)
                        .on_open(move || deleted.set(true)),
                ) as Element,
            ]
        };
        let mut h = harness(titled_content(actions()), 1400, 700, Theme::dark());
        settle(&mut h, 1400, 700);
        let wide = part_rect(&h, Part::Action(Column::Content, 0));
        assert!(wide.width() > wide.height() * 1.5, "a labelled button");
        assert!(part_rect(&h, Part::More(Column::Content)).width() <= 0.0);

        let mut h = harness(titled_content(actions()), 640, 700, Theme::dark());
        settle(&mut h, 640, 700);
        let compact = part_rect(&h, Part::Action(Column::Content, 0));
        assert!(
            (compact.width() - compact.height()).abs() < 1.0,
            "an icon-only square ({compact:?})"
        );
        let reply = h
            .root::<NavShell>()
            .test_part(Part::Action(Column::Content, 0))
            .and_then(Widget::accessibility)
            .and_then(|node| node.name);
        assert_eq!(
            reply.as_deref(),
            Some("Reply to everyone"),
            "the label stays its name"
        );

        let mut h = harness(titled_content(actions()), 220, 700, Theme::dark());
        settle(&mut h, 220, 700);
        let shown = h.root::<NavShell>().test_shown_actions(Column::Content);
        assert!(shown < 4, "{shown}");
        let more = part_rect(&h, Part::More(Column::Content));
        assert!(more.width() > 0.0 && more.x1 <= 220.0);
        assert!(
            h.root::<NavShell>()
                .test_part_hidden(Part::Action(Column::Content, 3)),
            "an action in the menu is not in the row"
        );
        assert!(
            h.root::<NavShell>()
                .test_exposed_parts()
                .contains(&Part::More(Column::Content))
        );
        h.click_at(more.center().x, more.center().y);
        settle(&mut h, 220, 700);
        let doc = automerge::AutoCommit::new();
        h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
        let labels = at(h.root::<NavShell>().test_headers(), Column::Content)
            .more
            .menu_labels();
        assert_eq!(labels.last().map(String::as_str), Some("Delete message"));
        let row = labels.len() - 1;
        at_mut(h.root_mut::<NavShell>().test_headers_mut(), Column::Content)
            .more
            .choose(row);
        h.send(&tilo_ui_base::synthetic_input::cursor_moved(10.0, 300.0));
        assert!(deleted.get(), "the chosen action ran through its api");
    }

    // A control the app hosts in its row — a view switcher — has no row of its own in the menu,
    // so the row keeps it and the buttons before it give way instead. The row shrinks its
    // buttons to their icons and keeps the switcher, where its shares stand.
    phase a_control_in_the_row_keeps_its_place_as_the_buttons_give_way;
    {
        let actions = || {
            vec![
                labelled("add", "Add a book", Icon::FilePlus),
                labelled("sort", "Sort", Icon::ArrowDownWideNarrow),
                labelled("hide", "Hide finished", Icon::EyeOff),
                Box::new(
                    ToggleGroup::new(
                        vec![
                            Segment::new("Shelf"),
                            Segment::new("Settings"),
                            Segment::new("Found online"),
                        ],
                        usize::MAX,
                    )
                    .on_choose("showing", |_| {}),
                ) as Element,
            ]
        };
        let mut h = harness(titled_content(actions()), 1280, 700, Theme::dark());
        settle(&mut h, 1280, 700);
        assert!(
            part_rect(&h, Part::Action(Column::Content, 3)).width() > 0.0,
            "the switcher stands in the row"
        );
        assert!(
            part_rect(&h, Part::More(Column::Content)).width() <= 0.0,
            "nothing is behind the row's menu"
        );

        let mut h = harness(titled_content(actions()), 660, 700, Theme::dark());
        settle(&mut h, 660, 700);
        let switcher = part_rect(&h, Part::Action(Column::Content, 3));
        assert!(
            switcher.width() > switcher.height(),
            "the switcher keeps its place in a narrow row ({switcher:?})"
        );
        let shown = h.root::<NavShell>().test_shown_actions(Column::Content);
        assert_eq!(shown, 4, "the row gave its buttons' labels up before it gave up the switcher");
        let add = part_rect(&h, Part::Action(Column::Content, 0));
        assert!(
            (add.width() - add.height()).abs() < 1.0,
            "a button gave its label up instead ({add:?})"
        );
    }
}

tilo_test_env::scenario! {
    fn drawers_bindings_and_typed_access_serve_the_app;

    // A bound Back and a bound title ask for an idle sync only once what they
    // follow moved, and the sync that shows it settles them again: a shell
    // with nothing to change keeps its window's event loop asleep.
    // The content's Back follows the app's page stack; the shell draws the
    // chip, and a press pops the stack.
    phase back_follows_the_apps_page_stack;
    {
        let pages = LocalState::new(tilo_ui_base::NavStack::new("dashboard"));
        let shell = NavShell::new()
            .content(TestLeaf::fill("content").boxed().0)
            .content_header("Dashboard", Vec::new())
            .with_back(&pages);
        let mut h = harness(shell, 1024, 700, Theme::dark());
        settle(&mut h, 1024, 700);
        assert_eq!(geometry(&h).back, Rect::ZERO, "the root has no Back");

        pages.update(|stack| stack.push("queue"));
        let doc = automerge::AutoCommit::new();
        h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
        settle(&mut h, 1024, 700);
        let g = geometry(&h);
        assert!(g.back.width() > 0.0, "a page pushed: Back shows");
        assert_eq!(g.test_back_column(), Some(Column::Content));
        assert!(part_rect(&h, Part::Title(Column::Content)).x0 >= g.back.x1);

        assert!(h.root_mut::<NavShell>().back());
        assert_eq!(pages.with(|stack| *stack.current()), "dashboard");
        settle(&mut h, 1024, 700);
        assert_eq!(geometry(&h).back, Rect::ZERO);

        let pages = LocalState::new(tilo_ui_base::NavStack::new("watch"));
        pages.update(|stack| stack.push("creator"));
        let shell = NavShell::new()
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .content(TestLeaf::fill("content").boxed().0)
            .with_back(&pages)
            .with_sidebar_hidden();
        let mut h = harness(shell, 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert!(g.back.width() > 0.0, "Back leads: {g:?}");
        assert_eq!(g.toggle, Rect::ZERO, "the toggle stands aside for it");
        assert!(h.root_mut::<NavShell>().back());
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert!(
            g.toggle.width() > 0.0 && g.back == Rect::ZERO,
            "the first page brings the toggle back"
        );
    }

    // Beside an open drawer the other columns stay laid out, painted and live:
    // a drawer is not modal. It opens under the header row, which stays live.
    phase an_open_drawer_leaves_the_window_live_beside_it;
    {
        let mut h = harness(
            mail_shell().content_kind(ContentKind::Document),
            800,
            600,
            Theme::dark(),
        );
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        settle(&mut h, 800, 600);
        let before = child_rect(&h, "content").expect("content laid out");
        h.root_mut::<NavShell>().open_sidebar();
        settle(&mut h, 800, 600);
        let g = geometry(&h);
        assert_eq!(g.sidebar, SidebarPlacement::Drawer);
        assert!(
            g.sidebar_column.y0 >= g.header_height,
            "under the header row"
        );
        let shell = h.root::<NavShell>();
        assert!(shell.test_laid_out_parts().contains(&Part::Body(Column::Content)));
        assert_eq!(
            shell
                .test_part(Part::Body(Column::Content))
                .map(Widget::get_rect),
            Some(before),
            "the content keeps its place beside the drawer"
        );
        let ids = exposed_ids(&h);
        assert!(ids.contains(&"content".to_owned()), "not inert: {ids:?}");
        assert!(
            ids.contains(&"nav-shell-history".to_owned()),
            "History stays reachable"
        );
        assert!(ids.contains(&"sidebar".to_owned()));
    }

    phase a_bound_state_opens_a_drawer_and_hears_the_persons_choice;
    {
        let open = LocalState::new(true);
        let shell = mail_shell()
            .content_kind(ContentKind::Document)
            .with_sidebar_state(&open);
        let mut h = harness(shell, 800, 600, Theme::dark());
        settle(&mut h, 800, 600);
        assert!(
            !open.with(|v| *v),
            "the shell wrote where it stands: a closed drawer"
        );
        open.set(true);
        let doc = automerge::AutoCommit::new();
        h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
        settle(&mut h, 800, 600);
        assert!(
            h.root::<NavShell>().is_sidebar_open(),
            "the state opened the drawer"
        );
        h.root_mut::<NavShell>().close_sidebar();
        assert!(!open.with(|v| *v), "the shell wrote the choice back");

        let inspector = LocalState::new(false);
        let shell = NavShell::new()
            .content(TestLeaf::fill("content").boxed().0)
            .inspector(TestLeaf::fill("inspector").boxed().0)
            .with_inspector_state(&inspector);
        let mut h = harness(shell, 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        assert!(inspector.with(|v| *v), "docked: open");
        inspector.set(false);
        h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
        settle(&mut h, 1280, 800);
        assert!(
            !h.root::<NavShell>().is_inspector_open(),
            "docked, but closed by state"
        );
    }

    // The inspector keeps the choice made for its size class while the app
    // takes it away and gives it back.
    phase the_inspector_remembers_its_choice_across_removal;
    {
        let shell = NavShell::new()
            .content(TestLeaf::fill("content").boxed().0)
            .inspector(TestLeaf::fill("inspector").boxed().0);
        let mut h = harness(shell, 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        h.root_mut::<NavShell>().close_inspector();
        settle(&mut h, 1280, 800);
        h.root_mut::<NavShell>().set_inspector(None);
        settle(&mut h, 1280, 800);
        h.root_mut::<NavShell>()
            .set_inspector(Some(TestLeaf::fill("inspector").boxed().0));
        settle(&mut h, 1280, 800);
        assert!(!h.root::<NavShell>().is_inspector_open());
    }

    phase typed_access_controls_and_the_filter_reach_the_apps_widgets;
    {
        let query = SharedRc::new(RefCell::new(String::new()));
        let list = Filtered {
            query: SharedRc::clone(&query),
            inner: TestLeaf::fill("overview"),
        };
        let shell = NavShell::new()
            .overview(Box::new(list))
            .content(TestLeaf::fill("content").boxed().0)
            .overview_header("Contacts", Vec::new())
            .overview_controls(Box::new(Button::new("sort", "Sort by name").on_open(|| {})))
            .with_filter(SearchField::filter());
        let mut h = harness(shell, 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert!(g.filter.width() > 0.0 && g.controls.width() > 0.0);
        assert!(
            g.controls.y0 >= g.filter.y1,
            "the controls stand under the filter"
        );
        let sort = part_rect(&h, Part::Controls);
        assert!(
            (sort.x0 - (g.overview_column.x0 + Metrics::resolve(&Theme::dark()).inset)).abs() < 0.5
        );
        let overview = child_rect(&h, "overview").expect("overview laid out");
        assert!(overview.y0 >= g.controls.y1);

        let order = LocalState::new("Name".to_owned());
        let shell = NavShell::new()
            .overview(TestLeaf::fill("list").boxed().0)
            .content(TestLeaf::fill("detail").boxed().0)
            .overview_header("Contacts", Vec::new())
            .overview_controls(Box::new(
                tilo_ui_app::widget::Dropdown::from_state(vec!["Name".into(), "Organization".into()], &order)
                    .with_accessible_name("Sort contacts"),
            ));
        let mut d = harness(shell, 1280, 800, Theme::dark());
        settle(&mut d, 1280, 800);
        let sort = part_rect(&d, Part::Controls);
        assert!(sort.width() > 0.0 && sort.height() > 0.0, "{sort:?}");

        assert!(
            h.root_mut::<NavShell>()
                .overview_mut::<Filtered>()
                .is_some()
        );
        assert!(h.root_mut::<NavShell>().content_mut::<TestLeaf>().is_some());
        assert!(h.root_mut::<NavShell>().sidebar_mut::<TestLeaf>().is_none());

        h.root_mut::<NavShell>()
            .filter_mut()
            .expect("the filter")
            .set_query("kai");
        let doc = automerge::AutoCommit::new();
        h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
        assert_eq!(query.borrow().as_str(), "kai");
        assert_eq!(
            h.root::<NavShell>().filter().and_then(SearchField::count),
            Some(FilterCount::new(2, 9))
        );
    }


    phase a_shell_with_overflowing_actions_passes_the_native_audits;
    {
        let shell = titled_content(vec![
            labelled("reply", "Reply to everyone", Icon::Reply),
            labelled("forward", "Forward", Icon::Send),
            labelled("archive", "Archive", Icon::Archive),
            labelled("delete", "Delete message", Icon::Trash2),
        ])
        .with_id("audit.shell");
        let mut h = harness(shell, 220, 700, Theme::dark());
        settle(&mut h, 220, 700);
        let audit = h.accessibility_audit();
        assert!(audit.issues.is_empty(), "{:?}", audit.issues);
        let native = h.native_app_audit();
        assert!(native.issues.is_empty(), "{:?}", native.issues);
    }

    // A shell that searches its sidebar hands the window's search the
    // sidebar's list, also while a narrow window shows the content screen and
    // the list is off screen; another shell leaves its sidebar alone.
    phase a_shell_that_searches_its_sidebar_hands_the_search_its_list;
    {
        let filtered = |query: &SharedRc<RefCell<String>>| Filtered {
            query: SharedRc::clone(query),
            inner: TestLeaf::fill("sidebar"),
        };
        let query = SharedRc::new(RefCell::new(String::new()));
        let shell = NavShell::new()
            .mode(NavMode::Push)
            .sidebar(Box::new(filtered(&query)))
            .content(TestLeaf::fill("content").boxed().0)
            .content_header("Found", Vec::new())
            .search_sidebar();
        let mut h = harness(shell, 400, 800, Theme::dark());
        settle(&mut h, 400, 800);
        h.root_mut::<NavShell>().show(NavPane::Content);
        settle(&mut h, 400, 800);
        assert!(
            !h.root::<NavShell>().is_sidebar_open(),
            "the narrow window shows its content"
        );
        let found = tilo_ui_app::widget::search_field::apply_filter(&mut *h.root_mut::<NavShell>(), "kai");
        assert_eq!(
            query.borrow().as_str(),
            "kai",
            "the search narrows the list"
        );
        assert_eq!(found, Some(FilterCount::new(2, 9)));

        let other = SharedRc::new(RefCell::new(String::new()));
        let mut plain = NavShell::new()
            .sidebar(Box::new(filtered(&other)))
            .content(TestLeaf::fill("content").boxed().0);
        assert!(
            tilo_ui_base::widget::capability::capability_mut::<dyn Filterable>(&mut plain).is_none(),
            "a shell answers for its sidebar's list only when it searches it"
        );
    }

}

#[test]
fn an_enlarged_inspector_sheet_leaves_the_content_above_it_in_reach() {
    let _settled = tilo_ui_base::motion::testing::settled();
    for theme in [Theme::light(), Theme::dark().with_text_scale(2.0)] {
        let band = f64::from(theme.tokens.metrics.min_pointer_target + theme.tokens.spacing.s4);
        let (content, events) = TestLeaf::fill("content").boxed();
        let shell = NavShell::new()
            .content_kind(ContentKind::Document)
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .content(content)
            .inspector(TestLeaf::fill("inspector").boxed().0);
        let mut h = harness(shell, 480, 800, theme.clone());
        h.root_mut::<NavShell>().open_inspector();
        settle(&mut h, 480, 800);
        let g = geometry(&h);
        assert_eq!(g.test_open_drawer(), Some(Column::Inspector));
        let content = child_rect(&h, "content").expect("the content stands");
        assert!(
            g.inspector_column.y0 - content.y0 >= band - 0.5,
            "a pointer target of content above the sheet: {content:?} {:?}",
            g.inspector_column
        );
        let before = events.events();
        h.click_at(
            content.center().x,
            (content.y0 + g.inspector_column.y0) / 2.0,
        );
        settle(&mut h, 480, 800);
        assert!(events.events() > before, "the press reaches the content");
        assert_eq!(
            geometry(&h).test_open_drawer(),
            Some(Column::Inspector),
            "the sheet stays up while the content above is used"
        );
    }
}

struct Capturing {
    rect: Rect,
    capture: Rect,
    events: SharedRc<Cell<usize>>,
}

impl Widget for Capturing {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(32.0, 32.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if matches!(event, WindowEvent::MouseInput { .. }) {
            self.events.set(self.events.get().saturating_add(1));
            *ctx.handled = true;
        }
    }

    fn modal_capture(&self) -> Option<Rect> {
        Some(self.capture)
    }

    fn name(&self) -> &'static str {
        "Capturing"
    }
}

#[test]
fn a_popover_owned_by_a_header_button_hears_the_pointer_first() {
    let events = SharedRc::new(Cell::new(0));
    let (content, content_events) = TestLeaf::fill("content").boxed();
    let shell = NavShell::new().content(content).content_header(
        "Forecast",
        vec![Box::new(Capturing {
            rect: Rect::ZERO,
            capture: Rect::new(600.0, 60.0, 900.0, 400.0),
            events: SharedRc::clone(&events),
        })],
    );
    let mut h = harness(shell, 1024, 700, Theme::dark());
    settle(&mut h, 1024, 700);
    let device = winit::event::DeviceId::dummy();
    h.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(700.0, 200.0),
    });
    let before = content_events.events();
    h.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: device,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    });
    assert_eq!(events.get(), 1, "the popover's owner heard the press");
    assert_eq!(
        content_events.events(),
        before,
        "the column under it did not"
    );
}

struct Filtered {
    query: SharedRc<RefCell<String>>,
    inner: TestLeaf,
}

impl Filterable for Filtered {
    fn set_filter(&mut self, query: &str) {
        self.query.replace(query.to_owned());
    }

    fn filter_count(&self) -> Option<FilterCount> {
        Some(FilterCount::new(2, 9))
    }
}

impl Widget for Filtered {
    tilo_ui_base::provides!(dyn Filterable);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.inner.apply_layout(taffy, node, origin);
    }

    fn name(&self) -> &'static str {
        "Filtered"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.inner.id)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.inner.rect
    }
}

/// Where a header's parts land against each other: a leading fill (a path) stands in the
/// content's row before its actions; a list's action stands at its own column's trailing edge
/// and keeps its place while an item is selected beside it; and a short column title is never
/// cut by the action after it.
#[test]
fn a_headers_parts_keep_their_places_beside_each_other() {
    let shell = NavShell::new()
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .content_header("", vec![icon("new", Icon::Plus, "New folder")])
        .content_header_leading(Box::new(Button::new("path", "Home › Documents › Taxes")));
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    let leading = part_rect(&h, Part::Leading(Column::Content));
    let action = part_rect(&h, Part::Action(Column::Content, 0));
    assert!(leading.width() > 0.0 && leading.x0 >= g.content_column.x0);
    assert!(leading.x1 <= action.x0, "{leading:?} before {action:?}");
    assert!(leading.y1 <= g.header_height);

    let theme = Theme::dark();
    let gutter = tilo_ui_base::chrome::column_gutter(&theme);
    let shell = NavShell::new()
        .overview(TestLeaf::fill("overview").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .overview_header("Inbox", vec![icon("search", Icon::Send, "Search")]);
    let mut h = harness(shell, 1024, 700, theme.clone());
    settle(&mut h, 1024, 700);
    let edge = geometry(&h).overview_column.x1;
    let ends_at =
        |rect: Rect, edge: f64| rect.width() > 0.0 && (rect.x1 - (edge - gutter)).abs() < 0.5;
    let quiet = part_rect(&h, Part::Action(Column::Overview, 0));
    assert!(
        ends_at(quiet, edge),
        "the list's action ends at its edge {edge}: {quiet:?}"
    );
    h.root_mut::<NavShell>()
        .set_content_header("Move flat", vec![icon("board", Icon::Send, "Board")]);
    settle(&mut h, 1024, 700);
    let selected = part_rect(&h, Part::Action(Column::Overview, 0));
    assert!(
        (selected.x0 - quiet.x0).abs() < 0.5 && (selected.x1 - quiet.x1).abs() < 0.5,
        "it keeps its place when an item is selected beside it: {quiet:?} then {selected:?}"
    );
    let own = part_rect(&h, Part::Action(Column::Content, 0));
    assert!(
        ends_at(own, geometry(&h).content_column.x1),
        "the detail's at the detail's edge"
    );

    let shell = NavShell::new()
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .sidebar_header("Notes", vec![icon("new", Icon::Plus, "New note")]);
    let mut h = harness(shell, 280, 700, theme.clone());
    settle(&mut h, 280, 700);
    let title = part_rect(&h, Part::Title(Column::Sidebar));
    let action = part_rect(&h, Part::Action(Column::Sidebar, 0));
    assert!(title.x1 <= action.x0, "{title:?} before {action:?}");
    let style = test_title_style(&theme);
    assert_eq!(
        TextRenderer::new().elide_to_width_style(
            "Notes",
            tilo_ui_base::num::f32_of_f64(title.width()),
            &style
        ),
        "Notes",
        "the whole name fits the title's box: {title:?}"
    );
}
