//! The gallery's charts pages, its states page and its radio buttons, reached as a person reaches
//! them (the sidebar's row, picked), at a laptop's width and a phone's: every chart is laid out with
//! room of its own, none over another, and is sent to the web page as its own figure; each state
//! sample is the control drawn still and inert; a radio group chooses through the action its radio
//! performs.

use tilo_app_test_host::TestApp;
use tilolang_process::window::app::TlApp;
use ui::a11y::{Action, Role};
use ui::composition::page::Page;
use ui::composition::{Composed, Composition};
use ui::headless::FrameHarness;

use super::gallery_html::{nodes, row, shows, until};

/// Each charts page and the charts it shows.
const CHARTS: [(&str, &[&str]); 3] = [
    (
        "Trends and series",
        &[
            "CartesianPlot",
            "CategoryChart",
            "CandlestickChart",
            "RangeChart",
        ],
    ),
    (
        "Shares and shapes",
        &[
            "DistributionChart",
            "PolarAreaChart",
            "RadarChart",
            "FunnelChart",
            "Heatmap",
            "PieChart",
        ],
    ),
    (
        "Flows and trees",
        &["SunburstChart", "Treemap", "SankeyChart", "ChordChart"],
    ),
];

/// A laptop's window and a phone's.
const SIZES: [(u32, u32); 2] = [(1280, 800), (390, 844)];

/// The control named `name` that `composition` offers assistive technology, as a widget path and
/// the target its press goes to.
fn offered(composition: &Composition, role: Role, name: &str) -> Option<(String, Option<String>)> {
    fn find(
        access: &ui::a11y::AccessNode,
        role: Role,
        name: &str,
    ) -> Option<Option<String>> {
        if access.role == role && access.name.as_deref() == Some(name) && !access.state.hidden {
            return Some(access.action_target.clone());
        }
        access
            .children
            .iter()
            .find_map(|child| find(child, role, name))
    }
    nodes(composition).into_iter().find_map(|node| {
        let target = find(node.semantics.as_ref()?, role, name)?;
        let widget = node
            .path
            .split_once('#')
            .map_or(node.path.as_str(), |(widget, _)| widget);
        Some((widget.to_owned(), target))
    })
}

/// Press what `composition` offers as `widget` and `target`, as a person's press does.
fn press(harness: &mut FrameHarness, (widget, target): (String, Option<String>), what: &str) {
    harness
        .perform_semantic_named(&widget, target.as_deref(), &Action::Activate)
        .unwrap_or_else(|refused| panic!("a press on {what} is taken: {refused}"));
}

/// Pick `place` in the sidebar, as a person does, and wait for it to show: on a phone, where a
/// page stands over the places, they go Back to them first.
fn pick(harness: &mut FrameHarness, place: &str) -> Composition {
    let composition = until(harness, "the sidebar lists its places, or Back to them", |composition| {
        row(composition, place).is_some() || offered(composition, Role::Button, "Back").is_some()
    });
    if row(&composition, place).is_none() {
        let back = offered(&composition, Role::Button, "Back").expect("Back to the places");
        press(harness, back, "Back");
    }
    let composition = until(harness, "the sidebar lists its places", |composition| {
        row(composition, place).is_some()
    });
    let picked = row(&composition, place).unwrap_or_else(|| panic!("the sidebar lists {place}"));
    let target = picked
        .semantics
        .as_ref()
        .and_then(|access| access.action_target.clone());
    let widget = picked
        .path
        .split_once('#')
        .map_or(picked.path.as_str(), |(widget, _)| widget)
        .to_owned();
    press(harness, (widget, target), &format!("{place}'s row"));
    until(
        harness,
        &format!("{place} shows once picked"),
        |composition| shows(composition, place),
    )
}

fn rect(node: &Composed) -> ui::kurbo::Rect {
    let [x, y, width, height] = node.rect;
    ui::kurbo::Rect::new(x, y, x + width, y + height)
}

/// The nodes of `composition` that are `widget`.
/// The radio group that asks how the report is sent.
fn report_group(composition: &Composition) -> Option<&Composed> {
    named(composition, "RadioGroup").into_iter().find(|group| {
        group
            .semantics
            .as_ref()
            .is_some_and(|access| access.name.as_deref() == Some("Send the report by"))
    })
}

/// The radios a radio group offers, as assistive technology reads them: one widget draws them
/// all, so they are its semantics' children, not widgets of their own.
fn radios_of(group: &Composed) -> Vec<&ui::a11y::AccessNode> {
    group
        .semantics
        .iter()
        .flat_map(|access| &access.children)
        .filter(|access| access.role == Role::RadioButton)
        .collect()
}

fn named<'a>(composition: &'a Composition, widget: &str) -> Vec<&'a Composed> {
    nodes(composition)
        .into_iter()
        .filter(|node| node.widget == widget)
        .collect()
}

#[test]
fn every_chart_has_room_of_its_own_at_any_width() {
    crate::support::installed("gallery");
    for size in SIZES {
        let mut app = TestApp::<TlApp>::drive_at("custom", size).expect("the scenario stages");
        let harness = app.harness();
        for (place, charts) in CHARTS {
            let _ = pick(harness, place);
            let composition = until(
                harness,
                &format!("{place} lays out its charts at {size:?}"),
                |composition| {
                    charts
                        .iter()
                        .all(|chart| !named(composition, chart).is_empty())
                },
            );
            let mut laid: Vec<(String, ui::kurbo::Rect)> = Vec::new();
            for chart in charts {
                for node in named(&composition, chart) {
                    let at = rect(node);
                    assert!(
                        at.width() >= 120.0 && at.height() >= 80.0,
                        "{place} at {size:?}: {} stands at {at:?}, too small to read",
                        node.path
                    );
                    assert!(
                        at.x1 <= f64::from(size.0) + 0.5,
                        "{place} at {size:?}: {} runs past the window's edge: {at:?}",
                        node.path
                    );
                    laid.push((node.path.clone(), at));
                }
            }
            for (at, (path, one)) in laid.iter().enumerate() {
                for (other_path, other) in laid.iter().skip(at + 1) {
                    let shared = one.intersect(*other);
                    assert!(
                        shared.width() <= 0.5 || shared.height() <= 0.5,
                        "{place} at {size:?}: {path} and {other_path} overlap"
                    );
                }
            }
            let html = Page::new().render(&composition);
            for chart in charts {
                assert!(
                    html.contains(&format!("data-w=\"{chart}\"")),
                    "{place} at {size:?}: the page is sent the {chart}"
                );
            }
        }
    }
}

#[test]
fn the_states_page_draws_each_control_still_in_its_state() {
    crate::support::installed("gallery");
    let mut app = TestApp::<TlApp>::drive_at("states", (1280, 800)).expect("the scenario stages");
    let harness = app.harness();
    let composition = pick(harness, "States");
    // Each sample is a picture of the control drawn in its state: on a page too, with nothing in
    // it to press or type into.
    let samples = named(&composition, "InState");
    assert!(
        samples.len() >= 40,
        "the page shows each control at rest, hovered, pressed and focused: {} samples",
        samples.len()
    );
    assert!(
        samples
            .iter()
            .all(|sample| sample.picture.is_some() && sample.children.is_empty()),
        "every sample is a picture of its paint"
    );
    let html = Page::new().render(&composition);
    assert!(
        html.matches("data-w=\"InState\"").count() >= samples.len(),
        "the page is sent every sample"
    );
    // A sample is a picture of a control, not one to use: what it would say to assistive
    // technology is hidden, and with it from the keyboard's walk.
    let samples: Vec<_> = ui::headless::test_support::semantic_nodes(harness)
        .into_iter()
        .filter(|(_, node)| node.role == Role::Button && node.name.as_deref() == Some("Save"))
        .collect();
    assert!(
        samples.len() >= 5,
        "the primary button's samples are in the tree"
    );
    assert!(
        samples
            .iter()
            .all(|(_, node)| node.state.hidden && !node.state.focusable),
        "no sample button is offered to assistive technology or the keyboard: {samples:?}"
    );
}

#[test]
fn a_radio_group_chooses_by_its_radios() {
    crate::support::installed("gallery");
    let mut app = TestApp::<TlApp>::drive_at("controls", (1280, 800)).expect("the scenario stages");
    let harness = app.harness();
    let composition = pick(harness, "Controls");
    let groups = named(&composition, "RadioGroup");
    assert_eq!(groups.len(), 2, "the controls page shows two radio groups");
    // Named, not counted: the walk of the page's parts gives no order between siblings.
    let report = report_group(&composition).expect("the controls page asks how to send the report");
    let group = report.path.clone();
    let radios: Vec<(Option<String>, Option<bool>, bool)> = radios_of(report)
        .into_iter()
        .map(|access| {
            (
                access.action_target.clone(),
                access.state.checked,
                access.state.disabled,
            )
        })
        .collect();
    assert_eq!(
        radios.len(),
        3,
        "the report's group offers three radios: {:#?}",
        report.semantics
    );
    assert_eq!(radios[0].1, Some(true), "the first is chosen");
    assert!(radios[2].2, "the third cannot be chosen");
    let html = Page::new().render(&composition);
    assert!(
        html.matches("type=\"radio\"").count() >= 5,
        "the page gives each radio an input of its own"
    );
    let second = radios[1]
        .0
        .clone()
        .expect("a radio is acted on by its target");
    harness
        .perform_semantic_named(&group, Some(&second), &Action::Activate)
        .unwrap_or_else(|refused| panic!("the second radio is chosen: {refused}"));
    let _ = until(harness, "the second radio shows chosen", |composition| {
        report_group(composition).is_some_and(|group| {
            radios_of(group)
                .get(1)
                .is_some_and(|access| access.state.checked == Some(true))
        })
    });
}
