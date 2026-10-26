use super::*;
use ui::headless::Framed;

fn fixture_bar() -> TopBarChrome {
    let workspace =
        tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId(1), "Design review");
    let tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    let mut bar = TopBarChrome::from_tree_with_local_user(&tree, "fixture-person".into());
    bar.clock = ClockSnapshot {
        clock: "09:41".into(),
        ..ClockSnapshot::default()
    };
    bar
}

#[test]
fn a_phone_wide_bar_keeps_its_targets_apart_at_320() {
    let frame = Framed::new(ui::State::new(), fixture_bar(), (320, 480), Theme::dark());
    let bar = &*frame;
    let visible: Vec<_> = bar
        .control_targets()
        .into_iter()
        .map(|target| (target.clone(), bar.control_rect(&target)))
        .filter(|(_, rect)| !rect.is_zero_area())
        .collect();
    assert!(
        !visible.is_empty(),
        "the actual bar must expose visible controls"
    );
    for (index, (left, a)) in visible.iter().enumerate() {
        assert!(a.x0 >= 0.0 && a.x1 <= 320.0, "{left} escapes: {a:?}");
        for (right, b) in visible.iter().skip(index + 1).filter(|(_, b)| b != a) {
            assert!(
                a.intersect(*b).is_zero_area(),
                "{left} {a:?} overlaps {right} {b:?}"
            );
        }
    }
}
