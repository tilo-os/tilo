use automerge::{AutoCommit, ROOT, transaction::Transactable};
use qos_desktop::activity::{ActivitySnapshot, ActivityView, RoomActivityPane};
use qos_desktop::{Workspace, WorkspaceId, WorkspaceTree};

// Removing causal replay or writing to the admitted document while reviewing
// must fail this regression. The root schema is a real admitted room document.
#[test]
fn workspace_activity_review_keeps_live_heads() {
    let workspace = Workspace::new(WorkspaceId::new(), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    doc.put(ROOT, "name", "Design").unwrap();
    doc.commit();
    doc.put(ROOT, "name", "Design review").unwrap();
    doc.commit();
    let mut tree = ActivitySnapshot::new(
        WorkspaceTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        },
        doc,
    )
    .unwrap();
    let before = tree.doc.get_heads();
    let mut pane = RoomActivityPane::new();
    pane.begin_view(tree.active.raw());
    pane.refresh(&mut tree);
    pane.set_view(ActivityView::Replay);
    assert!(pane.entries().len() >= 2);
    pane.set_author_filter(Some("absent-author".into()));
    pane.set_author_filter(None);
    pane.finish_view(tree.active.raw());
    assert!(!pane.replay_is_playing());
    assert_eq!(tree.doc.get_heads(), before);
}
