//! Binary shim for the Tilo Store.
//!
//! The real worker-backed application root lives in [`store::app_root`], so
//! the SDK, integration harnesses, and this binary share one lifecycle.
//! A plain launch, `--shots` and every other standard launch mode run the
//! framework app; only a launch that carries a `.flatpakref` file or a
//! `tilo-store:` link needs the standalone root that opens it.

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

/// Whether an argument is a launch target only the standalone root can open.
fn is_launch_target(arg: &str) -> bool {
    arg.starts_with("tilo-store:")
        || std::path::Path::new(arg)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("flatpakref"))
}

fn main() {
    if !std::env::args().skip(1).any(|arg| is_launch_target(&arg)) {
        ui::framework::run::<store::StoreShell>();
        return;
    }

    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("so.tilo.store", live_root)
            .max_positionals(1)
            .configure_app(|app| store::doc_actions::register(app.action_registry_mut())),
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

#[cfg(test)]
mod tests {
    use super::{is_launch_target, parse_store_uri};

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

    #[test]
    fn only_links_and_references_need_the_standalone_root() {
        assert!(is_launch_target("tilo-store:app/so.tilo.notes"));
        assert!(is_launch_target(
            "/home/me/Downloads/org.gnome.Chess.flatpakref"
        ));
        assert!(!is_launch_target("--shots"));
        assert!(!is_launch_target("target/look/app-store/wide.png"));
        assert!(!is_launch_target("1280x800"));
    }
}
