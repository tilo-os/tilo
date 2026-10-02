//! Feeds — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is dictated by ui::launch::ShotFactory, which other apps' fallible shot roots need"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(feeds::app_root_for_shot(
        args.scenario.as_deref().unwrap_or_default(),
    ))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn install_locales() -> Result<(), ui::i18n::I18nError> {
    ui::i18n::I18nManager::install_system(
        "en-US",
        &[
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ],
    )
}

fn wants_standalone(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            flag if flag.starts_with('-') => {}
            _ => {}
        }
    }
    false
}

pub fn main() {
    if let Err(error) = install_locales() {
        eprintln!("Feeds locale initialization failed: {error}");
        std::process::exit(1);
    }
    if !wants_standalone(std::env::args().skip(1)) {
        ui::framework::run::<feeds::FeedsShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.feeds", feeds::app_root)
            .shot_root(shot_root)
            .scenarios(&[
                "sidebar-open",
                "detail",
                "empty",
                "reader-settings",
                "refresh",
                "long-titles",
                "context-menu",
                "post-import",
                "add-subscription",
                "tags",
            ])
            .configure_app(|app| {
                app.set_window_title(Some("Feeds".into()));
                feeds::actions::register(app.action_registry_mut());
            }),
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_startup_installs_link_status_catalog() {
        super::install_locales().unwrap();
        for (key, value) in [
            (
                "feeds-open-link-invalid",
                ui::i18n!("feeds-open-link-invalid"),
            ),
            (
                "feeds-open-link-started",
                ui::i18n!("feeds-open-link-started"),
            ),
            (
                "feeds-open-link-failed",
                ui::i18n!("feeds-open-link-failed"),
            ),
        ] {
            assert_ne!(key, value);
            assert!(!value.is_empty());
        }
    }
}
