//! Every capture renders in a language that is not English.
//!
//! A scenario that looks a control up by the name it has in English finds
//! nothing when the window is drawn in another language, and the capture
//! fails — which is how the `discard` shot stopped rendering at all under
//! `--pseudo-locale`. The expanded pseudo-locale is the hardest language the
//! app ever draws (every message longer, every label wider), so a capture
//! that survives it survives every translation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use git_client::i18n::t;
use git_client::resource::GitClientShell;
use ui::framework::ResourceApp as _;
use ui::framework::testing::TestApp;
use ui::i18n::{I18nManager, PseudoLocale};

/// Every declared scenario captures with an expanded language in place.
#[test]
fn every_scenario_renders_in_an_expanded_language() {
    I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
    let mut rendered = Vec::new();
    for scenario in GitClientShell::scenarios() {
        let driven = TestApp::<GitClientShell>::drive(scenario);
        match driven {
            Ok(_) => rendered.push(*scenario),
            Err(error) => {
                I18nManager::set_pseudo_locale(PseudoLocale::Off);
                panic!("{scenario}: {error}");
            }
        }
    }
    // The label the `discard` shot presses is the one the window draws, so
    // the shot keeps rendering whatever language it is drawn in.
    assert_ne!(
        t("git-discard-file-name"),
        "Discard file changes",
        "the pseudo-locale really is in place"
    );
    I18nManager::set_pseudo_locale(PseudoLocale::Off);
    assert_eq!(
        rendered.len(),
        GitClientShell::scenarios().len(),
        "every scenario rendered"
    );
}
