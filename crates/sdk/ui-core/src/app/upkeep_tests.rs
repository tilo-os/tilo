use super::*;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};

/// A root that, like an app scaffolding its first run or writing its person
/// into a shared roster, writes to the document while it syncs.
struct Upkeep {
    rect: KurboRect,
    write: Option<&'static str>,
}
impl Widget for Upkeep {
    fn sync(&mut self, doc: &mut crate::widget::AppModel, path: &crate::widget::ModelPath) {
        let (doc, _) = crate::doc::document(doc, path);
        if let Some(key) = self.write.take() {
            doc.put(ROOT, key, "upkeep").unwrap();
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &crate::theme::Theme,
        _: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.new_leaf(taffy::Style::default()).unwrap()
    }
    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: vello::kurbo::Point) {}
    fn draw(
        &mut self,
        _: &mut Scene,
        _: Affine,
        _: &mut TextRenderer,
        _: &crate::theme::Theme,
        _: &PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "Upkeep"
    }
    fn get_rect(&self) -> KurboRect {
        self.rect
    }
}

fn remote_change(base: &[u8], key: &str) -> collab_types::DocumentMutation {
    let mut peer = AutoCommit::load(base).unwrap();
    let heads = peer.get_heads();
    peer.put(ROOT, key, "remote").unwrap();
    peer.commit();
    collab_types::DocumentMutation::Incremental(peer.save_after(&heads))
}

/// What an app writes while it syncs, with no edit of its own open, is not
/// typing in flight: a remote change is merged straight after it, with no
/// action ever committed.
#[test]
fn what_an_app_writes_while_it_syncs_does_not_hold_remote_changes_back() {
    let mut app = App::new(
        State::new(),
        Box::new(Upkeep {
            rect: KurboRect::ZERO,
            write: Some("scaffold"),
        }),
    );
    let base = app.state.doc.save();
    app.sync_root();
    assert!(app.state.doc.get(ROOT, "scaffold").unwrap().is_some());
    assert!(
        app.state.edits_settled(),
        "the app's upkeep is the baseline"
    );
    let (upkeep, heads) = app.state.pending_outbound_delta();
    assert!(!upkeep.is_empty(), "the upkeep still goes out");
    app.state.advance_outbound(heads);

    let merged = app
        .state
        .merge_remote_committed(&remote_change(&base, "remote"))
        .unwrap();
    assert_eq!(merged, Some(true), "the remote change is not held back");
    assert!(app.state.doc.get(ROOT, "remote").unwrap().is_some());
    assert_eq!(app.state.undo_depth(), 0, "upkeep is no undoable action");
    assert!(
        app.state.pending_outbound_delta().0.is_empty(),
        "a merged remote change is not echoed back as this device's own"
    );
}

/// An edit that is still open when the app syncs stays open: what the sync
/// writes then is part of it, and remote changes wait for it as before.
#[test]
fn an_edit_open_across_a_sync_still_holds_remote_changes_until_it_ends() {
    let mut app = App::new(
        State::new(),
        Box::new(Upkeep {
            rect: KurboRect::ZERO,
            write: Some("during typing"),
        }),
    );
    let base = app.state.doc.save();
    app.state.doc.put(ROOT, "typed", "burst").unwrap();
    app.state.doc.commit();
    app.sync_root();
    assert!(!app.state.edits_settled());
    let change = remote_change(&base, "remote");
    assert_eq!(app.state.merge_remote_committed(&change).unwrap(), None);

    app.state.commit_action(None, None);
    assert_eq!(
        app.state.merge_remote_committed(&change).unwrap(),
        Some(true)
    );
}
