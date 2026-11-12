//! A header row whose window's actions the host lists elsewhere (the desktop bar's window
//! menu): the row keeps none of them, each standing once; where nothing lists them, the row
//! shows what fits and its door holds the rest.

use super::*;
use crate::headless::InspectionNode;
use crate::widget::Button;

fn actions() -> Vec<Element> {
    (0..6)
        .map(|i| Box::new(Button::new(&format!("act-{i}"), "A labelled action")) as Element)
        .collect()
}

fn shell() -> NavShell {
    NavShell::new()
        .content(Probe::boxed("content").0)
        .content_header("Q3 plan", actions())
}

fn shows(node: &InspectionNode, id: &str) -> bool {
    if node.test_id.as_deref() == Some(id) {
        let [_, _, w, h] = node.rect;
        return w > 0.0 && h > 0.0;
    }
    node.children.iter().any(|child| shows(child, id))
}

fn bar(lists_actions: bool) -> Option<crate::widget::WindowBar> {
    Some(crate::widget::WindowBar {
        controls_width: 32.0,
        lists_actions,
    })
}

#[test]
fn actions_listed_elsewhere_stand_only_where_the_host_lists_them() {
    let (w, h) = (390, 700);
    for listed in [false, true] {
        let mut harness = harness(shell(), w, h, Theme::light());
        assert!(harness.root_mut::<NavShell>().set_window_bar(bar(listed)));
        settle(&mut harness, w, h);
        let inspection = harness.inspection();
        let shown = (0..6)
            .filter(|i| shows(&inspection.root, &format!("act-{i}")))
            .count();
        if listed {
            assert_eq!(shown, 0, "the host lists them: the row keeps none");
        } else {
            assert!(
                shown < 6,
                "six labelled actions do not fit at {w}: {shown} shown"
            );
        }
        assert_eq!(
            shows(&inspection.root, "nav-shell-more-content"),
            !listed,
            "the row's door shows only where nothing lists the window's actions (listed: {listed})"
        );
    }
}
