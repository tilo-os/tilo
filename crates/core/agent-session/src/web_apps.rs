//! The apps an agent can open in a browser session: one per app whose manifest
//! declares the web platform. Keep it in step with the app manifests.

use ui::application::ApplicationDefinition;

/// Every application an agent's browser session can open.
#[must_use]
pub fn web_apps() -> Vec<&'static ApplicationDefinition> {
    vec![
        activity::definition(),
        api_tester::definition(),
        assistant::definition(),
        camera::definition(),
        chip_design::definition(),
        clocks::definition(),
        tilo_console::definition(),
        diffmerge::definition(),
        feeds::definition(),
        hex_editor::definition(),
        maps::definition(),
        messages::definition(),
        model_providers::definition(),
        passwords::definition(),
        poker::definition(),
        scratchpad::definition(),
        shortcuts::definition(),
        slides::definition(),
        snippets::definition(),
        speed_test::definition(),
        terminal::definition(),
        themes::definition(),
        totp::definition(),
        typst_editor::definition(),
        weather::definition(),
    ]
}
