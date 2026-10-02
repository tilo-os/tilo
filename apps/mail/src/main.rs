//! Mail — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
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

fn scenario(args: &LaunchArgs) -> &str {
    args.scenario.as_deref().unwrap_or("wide")
}

// The signature is fixed by `ui::launch::ShotFactory`: other apps' shot roots
// genuinely can fail, so the alias is fallible even where this one isn't.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is dictated by ui::launch::ShotFactory, which other apps' fallible shot roots need"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(mail::app_root_scenario(scenario(args)))
}

fn shot_viewport(args: &LaunchArgs) -> (u32, u32) {
    mail::shot_viewport(scenario(args))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn wants_standalone(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            _ => {}
        }
    }
    false
}

pub fn main() {
    if !wants_standalone(std::env::args().skip(1)) {
        ui::framework::run::<mail::MailShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.mail", mail::app_root)
            .shot_root(shot_root)
            .scenarios(&[
                "wide",
                "narrow",
                "narrow_detail",
                "narrow_drawer",
                "mid",
                "compose",
                "compose_narrow",
                "setup",
                "setup_narrow",
                "setup_add",
                "setup_oauth",
                "firstrun",
                "html_mail",
                "search",
                "preview",
                "bulk",
                "bulk_purge_confirmation",
                "reply",
                "context_menu",
                "loading",
                "empty",
                "offline",
                "error",
            ])
            .shot_viewport(shot_viewport)
            .configure_app(|app| {
                app.set_window_title(Some("Mail".into()));
                mail::actions::register(app.action_registry_mut());
            }),
    );
}
