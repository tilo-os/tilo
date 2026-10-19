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
    use settings::SettingsApp;
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

    /// The row labelled `label`, if the list shows one.
    pub fn nav_row(root: &dyn Widget, label: &str) -> Option<NavRow> {
        nav_rows(root).into_iter().find(|row| row.label == label)
    }

    pub struct ScratchConfig {
        dir: tempfile::TempDir,
    }

    impl ScratchConfig {
        pub fn new() -> Self {
            Self {
                dir: tempfile::Builder::new()
                    .prefix("tilo-settings-it-")
                    .tempdir()
                    .expect("create scratch config root"),
            }
        }

        pub fn app(&self) -> SettingsApp {
            SettingsApp::with_config_root(self.dir.path().join("tilo"))
        }
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
#[path = "search_focus.rs"]
mod search_focus;

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
