// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

/// Each test's app keeps its account stores under a private root, so a
/// suite run never reads the user's accounts, replays their interrupted
/// account journal or writes a migration marker into their config.
mod support {
    use settings::Inspected;
    use ui::widget::Widget;
    use vello::kurbo::Rect;

    /// One row of the sidebar's navigation list as the accessibility tree
    /// publishes it.
    #[derive(Clone, Debug)]
    pub struct NavRow {
        pub label: String,
        pub rect: Rect,
        pub selected: bool,
    }

    /// The category rows the live tree's navigation list shows, in order.
    /// Section headers are rows of the list too; they are told apart by
    /// not being a destination (no row of a category carries their label).
    pub fn nav_rows(root: &dyn Widget) -> Vec<NavRow> {
        fn walk(w: &dyn Widget, out: &mut Vec<NavRow>) {
            if w.name() == "NavList"
                && let Some(tree) = w.accessibility()
            {
                for item in tree.children {
                    if item.role != ui::a11y::Role::TreeItem {
                        continue;
                    }
                    let (Some(label), Some(bounds)) = (item.name.clone(), item.bounds) else {
                        continue;
                    };
                    out.push(NavRow {
                        label,
                        rect: bounds.to_rect(),
                        selected: item.state.selected,
                    });
                }
                return;
            }
            for child in w.children() {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(root, &mut out);
        out
    }

    /// Settings as a host opens it, on a fresh private profile, laid out at
    /// `viewport`: its sidebar rows run the published open action, so a
    /// click navigates only where a host dispatches it.
    pub type Window = ui::framework::testing::TestApp<settings::SettingsShell>;

    pub fn window(viewport: (u32, u32)) -> Window {
        let mut app = Window::new().expect("settings opens headlessly");
        app.harness()
            .resize(viewport)
            .expect("settings lays out at the fixture size");
        app.settle();
        app
    }

    /// Settings as a host opens it on `category`'s pane, laid out at
    /// `viewport`, once the pane shows what it first reads: a page that is
    /// view code over its sections is built only in the app's view, never
    /// in a window outside it.
    pub fn window_on(category: settings::categories::Category, viewport: (f32, f32)) -> Window {
        window_themed(category, viewport, ui::Theme::dark())
    }

    /// [`window_on`] in `theme`: large text is a theme's text scale, as the
    /// live host hands it to the window.
    pub fn window_themed(
        category: settings::categories::Category,
        (w, h): (f32, f32),
        theme: ui::Theme,
    ) -> Window {
        fn settling(widget: &dyn Widget) -> bool {
            if let Some(window) = widget
                .as_any()
                .and_then(|any| any.downcast_ref::<Inspected>())
            {
                return Widget::has_pending_visual_work(window);
            }
            widget.children().into_iter().any(settling)
        }
        let mut app = Window::new().expect("settings opens headlessly");
        app.effect(&settings::effects::open_pane(category))
            .expect("the pane opens");
        app.harness()
            .relayout_themed(ui::headless::viewport((w, h)), theme)
            .expect("settings lays out at the fixture size");
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| !settling(
                    harness.root_widget()
                )),
            "{category:?} shows what it reads"
        );
        app.settle();
        app
    }

    /// Where the window's shell is: whether it is narrow (one screen at a
    /// time), whether the list screen (or the docked list) shows, and the
    /// content screen's back arrow.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Shell {
        pub narrow: bool,
        pub sidebar_open: bool,
        pub back: Rect,
    }

    /// The window's shell, as laid out now.
    pub fn shell(app: &mut Window) -> Shell {
        app.with_widget::<ui::widget::NavShell, _>(|shell| {
            let geometry = shell.geometry();
            Shell {
                narrow: !geometry.stack.is_empty(),
                sidebar_open: shell.is_sidebar_open(),
                back: geometry.back,
            }
        })
        .expect("settings shows its shell")
    }

    /// The navigation rows the window's accessibility tree exports.
    pub fn window_rows(app: &mut Window) -> Vec<NavRow> {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.role() == ui::accesskit::Role::TreeItem)
            .filter_map(|(_, node)| {
                let label = node.label()?.to_owned();
                let bounds = node.bounds()?;
                Some(NavRow {
                    label,
                    rect: Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
                    selected: node.is_selected() == Some(true),
                })
            })
            .collect()
    }

    /// The window's row labelled `label`, if its list shows one.
    pub fn window_row(app: &mut Window, label: &str) -> Option<NavRow> {
        window_rows(app).into_iter().find(|row| row.label == label)
    }

    /// Click `category`'s sidebar row with the pointer, as a person does.
    pub fn click_category(app: &mut Window, category: settings::categories::Category) {
        let row = window_row(app, &category.label())
            .unwrap_or_else(|| panic!("no sidebar row for {category:?}"));
        let center = row.rect.center();
        app.harness().click_at(center.x, center.y);
        app.settle();
    }

    /// The pane the window shows: the one its sidebar's list selects.
    pub fn selected(app: &mut Window) -> settings::categories::Category {
        app.with_widget::<ui::widget::NavList, _>(|list| list.selected_id())
            .flatten()
            .as_deref()
            .and_then(settings::categories::Category::from_slug)
            .expect("the sidebar selects the pane shown")
    }

    /// Type `query` into the window bar's search, as the bar hands it to the
    /// app, and lay the window out again.
    pub fn bar_search(app: &mut Window, search: &mut ui::widget::SearchDriver, query: &str) {
        let root = app.harness().root_widget_mut();
        if search.in_place() == ui::widget::InPlace::Nothing {
            search.open(ui::window_toolbar::SearchOffer::of(&mut *root).into());
        }
        assert!(
            search.drive(root, query, ui::widget::FindOptions::default(), None, false),
            "the bar's search reached settings"
        );
        app.harness().relayout();
    }
}

#[path = "category_nav.rs"]
mod category_nav;
#[path = "category_sidebar.rs"]
mod category_sidebar;
#[path = "control_interaction.rs"]
mod control_interaction;
#[path = "narrow_navigation.rs"]
mod narrow_navigation;
#[path = "no_app_title_header.rs"]
mod no_app_title_header;
#[path = "scroll.rs"]
mod scroll;

#[path = "composition_balance.rs"]
mod composition_balance;

#[path = "alignment_review.rs"]
mod alignment_review;

#[path = "page_layout.rs"]
mod page_layout;
#[path = "pane_geometry.rs"]
mod pane_geometry;

#[path = "published_surface.rs"]
mod published_surface;

#[path = "accounts_calendar_loop.rs"]
mod accounts_calendar_loop;
#[path = "accounts_mail_loop.rs"]
mod accounts_mail_loop;
#[path = "perf_budget.rs"]
mod perf_budget;

#[path = "a11y_bus.rs"]
mod a11y_bus;

#[path = "perf_interaction.rs"]
mod perf_interaction;

#[path = "action_keywords_tests.rs"]
mod action_keywords_tests;
