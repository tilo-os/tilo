//! The launcher as a person meets it: where the card, its field, its chips
//! and its rows sit in each scenario, as a capture stages it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use launcher_app::{LauncherShell, LauncherSurface};
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;
use ui::widget::Widget as _;

fn staged(scenario: &str) -> HeadlessApp<LauncherShell> {
    let viewport = <LauncherShell as ui::framework::ResourceApp>::shot_viewport(Some(scenario));
    HeadlessApp::<LauncherShell>::capture_at(Some(scenario), viewport).unwrap()
}

/// What the launcher's card shows, top to bottom, by name.
fn targets(app: &mut HeadlessApp<LauncherShell>) -> Vec<(String, Rect)> {
    let mut targets = app
        .surface(|surface: &mut LauncherSurface| surface.test_rects())
        .unwrap();
    targets.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
    targets
}

fn has(app: &mut HeadlessApp<LauncherShell>, name: &str) -> bool {
    targets(app).iter().any(|(shown, _)| shown == name)
}

fn target(app: &mut HeadlessApp<LauncherShell>, name: &str) -> Rect {
    targets(app)
        .into_iter()
        .find(|(shown, _)| shown == name)
        .map_or_else(|| panic!("no target named {name}"), |(_, rect)| rect)
}

fn rows(app: &mut HeadlessApp<LauncherShell>) -> Vec<String> {
    targets(app)
        .into_iter()
        .filter_map(|(name, _)| name.strip_prefix("launcher-row:").map(str::to_owned))
        .collect()
}

#[test]
fn the_launcher_lists_its_sections_under_the_field() {
    let mut app = staged("launcher");
    let field = target(&mut app, "launcher-field");
    let rows = rows(&mut app);
    assert_eq!(
        rows.first().map(String::as_str),
        Some("window:design-review")
    );
    assert_eq!(rows.last().map(String::as_str), Some("all-apps"));
    let first = target(&mut app, "launcher-row:window:design-review");
    assert!(first.y0 >= field.y1, "rows sit under the field");
    assert!(
        !has(&mut app, "launcher-filters"),
        "no chips before anything is typed"
    );
}

#[test]
fn results_wear_the_filter_band_between_field_and_rows() {
    let mut app = staged("results");
    let field = target(&mut app, "launcher-field");
    let band = target(&mut app, "launcher-filters");
    let first = rows(&mut app).first().cloned().unwrap();
    let row = target(&mut app, &format!("launcher-row:{first}"));
    assert!(
        band.y0 >= field.y1 && row.y0 >= band.y1,
        "{field:?} {band:?} {row:?}"
    );
}

#[test]
fn a_chosen_kind_shows_only_its_results() {
    let mut app = staged("filtered");
    let rows = rows(&mut app);
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row.starts_with("file:")), "{rows:?}");
}

#[test]
fn an_action_that_asks_shows_its_field_and_no_results() {
    let mut app = staged("step");
    assert!(rows(&mut app).is_empty());
    let card = target(&mut app, "launcher-card");
    let field = target(&mut app, "launcher-field");
    assert!(card.contains(field.center()));
}

#[test]
fn values_to_pick_and_other_actions_are_rows() {
    let mut app = staged("choices");
    assert_eq!(rows(&mut app), ["choice:0", "choice:1", "choice:2"]);
    let mut app = staged("more");
    assert_eq!(rows(&mut app).len(), 4);
}

#[test]
fn a_held_super_shows_the_field_alone() {
    let mut app = staged("peek");
    assert!(rows(&mut app).is_empty());
    let card = target(&mut app, "launcher-card");
    assert!(card.height() < 120.0, "{card:?}");
}

#[test]
fn a_narrow_window_keeps_the_card_inside_it() {
    let mut app = staged("narrow");
    let card = target(&mut app, "launcher-card");
    assert!(card.x1 <= 420.0 + 0.5, "{card:?}");
}
