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
        calculator::definition(),
        calendar::definition(),
        camera::definition(),
        canvas::definition(),
        chip_design::definition(),
        clocks::definition(),
        tilo_console::definition(),
        contacts::definition(),
        diffmerge::definition(),
        ebook::definition(),
        feeds::definition(),
        file_explorer::page_definition(),
        hex_editor::definition(),
        maps::definition(),
        messages::definition(),
        model_providers::definition(),
        music::definition(),
        passwords::definition(),
        pdf_viewer::definition(),
        poker::definition(),
        scratchpad::definition(),
        settings::definition(),
        sheets::definition(),
        shortcuts::definition(),
        slides::definition(),
        snippets::definition(),
        speed_test::definition(),
        tasks::definition(),
        terminal::definition(),
        themes::definition(),
        totp::definition(),
        typst_editor::definition(),
        video_editor::definition(),
        video_player::definition(),
        weather::definition(),
        word_processor::definition(),
    ]
}
