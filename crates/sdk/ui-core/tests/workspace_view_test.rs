use tilo_ui::widget::{WorkspaceSwitchDirection, WorkspaceView};

#[test]
fn workspace_view_switch_up_down_sets_slide_target() {
    let mut v = WorkspaceView::new(3, 0);
    v.switch(WorkspaceSwitchDirection::Down);
    assert_eq!(v.active_index(), 1);
    let target = v.slide_target();
    assert!(
        target.abs() > 0.01,
        "expected a non-zero slide target, got {target}"
    );
    v.switch(WorkspaceSwitchDirection::Down);
    assert_eq!(v.active_index(), 2);
    v.switch(WorkspaceSwitchDirection::Down);
    assert_eq!(v.active_index(), 2); // clamped
}
