//! Asserts the sidebar advertises one entry per category.
//!
//! Constructs `SettingsApp`, runs `build_layout` + `apply_layout` at a
//! wide size (so the two-pane layout with the full sidebar is built), then
//! reads the navigation list's rows. Each category is a row named by its
//! display label, so the count is exact.

use settings::categories::Category;

#[test]
fn sidebar_has_one_entry_per_category() {
    let config = crate::support::ScratchConfig::new();
    // Tall enough to mount every row: a navigation list builds only what is
    // in view.
    let app = ui::headless::Framed::new(
        ui::State::new(),
        config.app(),
        ui::headless::viewport((1200.0, 1800.0)),
        ui::Theme::dark(),
    );

    // Each category is one row of the navigation list, named by its full
    // display label. Collect the rows and assert exactly one per category,
    // with the *complete* (untruncated) label; the section headers are the
    // list's other rows.
    let labels: Vec<String> = crate::support::nav_rows(&*app)
        .into_iter()
        .map(|row| row.label)
        .collect();
    let destinations = labels
        .iter()
        .filter(|label| Category::ALL.iter().any(|c| c.label() == **label))
        .count();
    assert_eq!(
        destinations,
        Category::ALL.len(),
        "expected exactly one sidebar row per category, found {labels:?}"
    );

    for category in Category::ALL {
        let found = labels
            .iter()
            .filter(|l| l.as_str() == category.label())
            .count();
        assert_eq!(
            found,
            1,
            "expected one sidebar row carrying the full label {:?} for {:?}",
            category.label(),
            category
        );
    }
}
