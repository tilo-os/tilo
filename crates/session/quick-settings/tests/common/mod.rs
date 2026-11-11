//! What the panel's headless tests share: the panel on a desktop at a size and scale, and the
//! person's ways to find and press its controls.

use std::rc::Rc;

use tilo_quick_settings::host::{FixtureHost, Lent, PanelHost};
use tilo_quick_settings::{QuickSettingsApp, scenarios};
use ui::framework::testing::HeadlessApp;
use ui::headless::InspectionNode;
use ui::vello::kurbo::Rect;

/// The panel on a desktop drawing at `scale`, in a window of `viewport`: the scenario's
/// readings, and the host it records what it is asked to do on.
pub fn hosted_at(
    scenario: &str,
    viewport: (u32, u32),
    scale: f64,
) -> (HeadlessApp<QuickSettingsApp>, Rc<FixtureHost>) {
    let host = Rc::new(FixtureHost::new(scenarios::readings(Some(scenario))));
    let lent: Rc<dyn PanelHost> = host.clone();
    let mut app =
        HeadlessApp::<QuickSettingsApp>::for_person_with_lent("person", Lent(lent)).unwrap();
    for invoke in scenarios::actions(Some(scenario)) {
        app.invoke(invoke).unwrap();
    }
    app.harness().set_input_scale(scale).unwrap();
    app.harness().resize(viewport).unwrap();
    app.settle();
    (app, host)
}

/// Where the control `name` stands.
pub fn target(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) -> Rect {
    let targets = app.harness().targets();
    *targets.get(name).unwrap_or_else(|| {
        let mut names: Vec<&String> = targets.keys().collect();
        names.sort();
        panic!("no target named {name}; there are {names:?}")
    })
}

/// Press `name` as a person does, and let the panel answer.
pub fn click(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) {
    app.harness().click_named(name).unwrap();
    app.settle();
}

/// Whether the panel shows the control `name`.
pub fn has(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) -> bool {
    app.harness().targets().contains_key(name)
}

/// What the panel draws on top of something else of its own: two words, or two controls, neither
/// holding the other, whose boxes meet where the panel shows them. The panel lays its parts out
/// in turn and stands no surface over another, so every meeting is two things in one place.
pub fn drawn_on_each_other(app: &mut HeadlessApp<QuickSettingsApp>) -> Vec<String> {
    let inspection = app.harness().inspection();
    let mut parts = Vec::new();
    gather(
        &inspection.root,
        Rect::new(0.0, 0.0, f64::MAX, f64::MAX),
        &mut parts,
    );
    let mut found = Vec::new();
    for (i, (a, ra)) in parts.iter().enumerate() {
        for (b, rb) in parts.iter().skip(i.saturating_add(1)) {
            let holds = |outer: &str, inner: &str| inner.starts_with(&format!("{outer}/"));
            let meet = ra.intersect(*rb);
            if meet.width() > 1.0 && meet.height() > 1.0 && !holds(a, b) && !holds(b, a) {
                found.push(format!("{b} {rb:?} on {a} {ra:?}"));
            }
        }
    }
    found
}

/// The words and the controls under `node`, each as much of it as shows inside `clip`.
fn gather(node: &InspectionNode, clip: Rect, out: &mut Vec<(String, Rect)>) {
    let [x, y, width, height] = node.rect;
    let shown = Rect::new(x, y, x + width, y + height).intersect(clip);
    if (node.widget == "Text" || node.test_id.is_some()) && shown.width() > 0.0 {
        out.push((node.path.clone(), shown));
    }
    let clip = node.child_clip.map_or(clip, |[x, y, width, height]| {
        clip.intersect(Rect::new(x, y, x + width, y + height))
    });
    for child in &node.children {
        gather(child, clip, out);
    }
}
