// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

/// Each test's app keeps its account stores under a private root, so a
/// suite run never reads the user's accounts, replays their interrupted
/// account journal or writes a migration marker into their config.
mod support {
    use settings::SettingsApp;

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
#[path = "no_app_title_header.rs"]
mod no_app_title_header;
#[path = "scroll.rs"]
mod scroll;
#[path = "search_focus.rs"]
mod search_focus;
#[path = "shots_smoke.rs"]
mod shots_smoke;

#[path = "composition_balance.rs"]
mod composition_balance;

#[path = "alignment_review.rs"]
mod alignment_review;
