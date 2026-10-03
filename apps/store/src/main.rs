//! Binary shim for the Tilo Store.
//!
//! The real worker-backed application root lives in [`store::app_root`], so
//! the SDK, integration harnesses, and this binary share one lifecycle.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the library crate note"
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

#[cfg(feature = "thin")]
extern crate runtime_dylib;

use store::FlatpakRef;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

fn wants_standalone(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            flag if flag.starts_with('-') => {}
            // A `.flatpakref` or `tilo-store:app/<id>` argument is decoded by
            // `live_root`, which opens the same worker-backed Store on it.
            _ => return true,
        }
    }
    false
}

fn main() {
    if !wants_standalone(std::env::args().skip(1)) {
        ui::framework::run::<store::StoreShell>();
        return;
    }

    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("so.tilo.store", live_root)
            .max_positionals(1)
            .scenarios(&[
                "wide",
                "narrow",
                "detail",
                "remotes",
                "progress",
                "error",
                "moderation",
                "moderation-case",
                "moderation-loading",
                "moderation-offline",
                "moderation-error",
                "empty",
                "offline",
            ])
            .configure_app(|app| store::doc_actions::register(app.action_registry_mut()))
            .shot_handler(shot_handler),
    );
}

fn live_root(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    // Opening a reference only stages a regular Store card. Installation stays
    // gated on the card's explicit Install action.
    let opened_ref = args
        .positionals()
        .first()
        .map(std::path::Path::new)
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("flatpakref"))
        })
        .map(FlatpakRef::from_path)
        .transpose()
        .map_err(|error| LaunchError::Operation(error.to_string()))?
        .flatten();
    let store_target = args
        .positionals()
        .first()
        .filter(|value| value.starts_with("tilo-store:"))
        .map(|value| parse_store_uri(value))
        .transpose()?;
    Ok(store::live::app_root_with_launch(opened_ref, store_target))
}

/// Decode the sole Store deep-link shape. The launcher already validates the
/// scheme claim; this second check keeps malformed or argument-like values
/// from becoming a catalogue identity inside the app.
fn parse_store_uri(uri: &str) -> Result<String, LaunchError> {
    let Some(id) = uri.strip_prefix("tilo-store:app/") else {
        return Err(LaunchError::Operation("unsupported App Store link".into()));
    };
    if id.is_empty()
        || id.len() > 255
        || id
            .chars()
            .any(|ch| !ch.is_ascii_alphanumeric() && ch != '.' && ch != '-')
    {
        return Err(LaunchError::Operation(
            "invalid App Store app identity".into(),
        ));
    }
    Ok(id.to_string())
}

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args
        .shots
        .as_deref()
        .ok_or_else(|| LaunchError::Operation("missing screenshot output".into()))?;
    store::shots::render_scenario_to_path_with_theme(
        args.viewport
            .unwrap_or(store::shots::DEFAULT_SHOTS_VIEWPORT),
        out,
        args.scenario.as_deref(),
        &ui::launch::shot_theme(args),
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{parse_store_uri, wants_standalone};

    fn args(values: &[&str]) -> impl Iterator<Item = String> {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn opening_a_reference_or_store_link_keeps_its_argument() {
        assert!(!wants_standalone(args(&[])));
        assert!(!wants_standalone(args(&["--data-dir", "/profile"])));
        assert!(wants_standalone(args(&["/downloads/notes.flatpakref"])));
        assert!(wants_standalone(args(&[
            "--data-dir",
            "/profile",
            "tilo-store:app/so.tilo.notes"
        ])));
    }

    #[test]
    fn store_deep_link_accepts_only_a_stable_app_identity() {
        assert_eq!(
            parse_store_uri("tilo-store:app/so.tilo.notes").unwrap(),
            "so.tilo.notes"
        );
        for invalid in [
            "tilo-store:app/",
            "tilo-store:app/so.tilo.notes?install=true",
            "tilo-store:remote/so.tilo.notes",
        ] {
            assert!(parse_store_uri(invalid).is_err(), "{invalid}");
        }
    }
}
