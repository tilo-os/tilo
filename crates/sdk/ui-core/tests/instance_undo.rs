use crate::support::application::{ProbeHost, author, existing, instantiate_probe};
use automerge::ReadDoc;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use std::collections::HashMap;
use tilo_ui::application::{LaunchTarget, RestoreInput};
use tilo_ui::bound_dispatch::{BoundNodeMap, invoke_bound_node};
use tilo_ui::state::UndoLog;
use tilo_ui::widget::{TileHost, Widget};

struct BoundProbe {
    _tile: Box<TileHost>,
    bindings: BoundNodeMap,
    nodes: HashMap<taffy::NodeId, *mut dyn Widget>,
    node: taffy::NodeId,
    _tree: taffy::TaffyTree<()>,
}
impl BoundProbe {
    fn new(component: tilo_ui::application::AppComponent) -> Self {
        let mut tile = Box::new(TileHost::from_component(component));
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = HashMap::new();
        let layout = tile.build_layout(&mut tree, &tilo_ui::Theme::dark(), &mut nodes);
        // Select the actual probe leaf, not a nondeterministic retained wrapper.
        let node = *nodes
            .keys()
            .find(|node| tree.children(**node).unwrap().is_empty())
            .unwrap();
        let mut bindings = BoundNodeMap::new();
        // SAFETY: heap-owned tile and this matching layout remain stable until Drop.
        unsafe {
            bindings.rebuild(tile.as_ref(), &tree, layout, &nodes);
        }
        Self {
            _tile: tile,
            bindings,
            nodes,
            node,
            _tree: tree,
        }
    }
    fn invoke<R>(
        &mut self,
        outer: &mut AutoCommit,
        callback: impl FnOnce(&mut dyn Widget, &mut AutoCommit, &automerge::ObjId) -> R,
    ) -> Result<R, tilo_ui::bound_dispatch::BoundDispatchError> {
        // SAFETY: exclusive self owns the stable, live tile and both admission maps.
        unsafe {
            invoke_bound_node(
                &self.bindings,
                &self.nodes,
                self.node,
                outer,
                &ROOT,
                callback,
            )
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn typing_bursts_finalize_at_the_right_record_boundary() {
    // Case `readonly_mount_and_noop_foreign_update_keep_one_logical_typing_burst`.
    {
        let host = ProbeHost::new();
        let first = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let checkout = first.record().clone();
        let mut a = BoundProbe::new(first);
        let mut outer = AutoCommit::new();
        a.invoke(&mut outer, |_, doc, _| doc.put(ROOT, "a", "a1").unwrap())
            .unwrap();
        let pending = host
            .records
            .read(
                &checkout.id,
                automerge::transaction::Transactable::pending_ops,
            )
            .unwrap();
        assert!(pending > 0);
        let mut b = BoundProbe::new(
            instantiate_probe(&host, 12, existing(&checkout), RestoreInput::None).unwrap(),
        );
        assert_eq!(
            host.records
                .read(
                    &checkout.id,
                    automerge::transaction::Transactable::pending_ops
                )
                .unwrap(),
            pending,
            "mount must not close A's physical transaction"
        );
        a.invoke(&mut outer, |_, doc, _| doc.put(ROOT, "a", "a2").unwrap())
            .unwrap();
        assert!(
            host.records
                .read(
                    &checkout.id,
                    automerge::transaction::Transactable::pending_ops
                )
                .unwrap()
                > pending
        );
        b.invoke(&mut outer, |widget, doc, path| {
            widget.update(doc, path);
        })
        .unwrap();
        host.records
            .read_history(&checkout.id, |doc, history| {
                assert!(history.next_undo_in(doc, &author(), 11).is_none());
            })
            .unwrap();
        a.invoke(&mut outer, |_, doc, _| doc.put(ROOT, "a", "a3").unwrap())
            .unwrap();
        b.invoke(&mut outer, |_, doc, _| doc.put(ROOT, "b", "b1").unwrap())
            .unwrap();
        host.records
            .with_history(&checkout.id, |doc, history| {
                history
                    .undo_in(doc, &author(), 11, None, None, None)
                    .unwrap();
                assert!(history.next_undo_in(doc, &author(), 11).is_none());
                assert_eq!(read_string(doc, "a"), None);
                assert_eq!(read_string(doc, "b").as_deref(), Some("b1"));
            })
            .unwrap();
        assert!(outer.get_heads().is_empty());
    }

    // Case `another_instance_mutation_finalizes_the_original_record_boundary`.
    {
        let host = ProbeHost::new();
        let first = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let checkout = first.record().clone();
        let second = instantiate_probe(&host, 12, existing(&checkout), RestoreInput::None).unwrap();
        let mut first = TileHost::from_component(first);
        let mut second = TileHost::from_component(second);
        let mut outer = AutoCommit::new();
        let outer_heads = outer.get_heads();
        for (tile, value) in [(&mut first, "a1"), (&mut second, "b1")] {
            let mut tree = taffy::TaffyTree::new();
            let mut nodes = HashMap::new();
            let layout = tile.build_layout(&mut tree, &tilo_ui::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            let node = *nodes.keys().next().unwrap();
            // SAFETY: this test exclusively owns the matching live tree and node map.
            unsafe {
                bindings.rebuild(tile, &tree, layout, &nodes);
                invoke_bound_node(&bindings, &nodes, node, &mut outer, &ROOT, |_, doc, _| {
                    doc.put(ROOT, value, value).unwrap();
                })
                .unwrap();
            }
        }
        assert_eq!(outer.get_heads(), outer_heads);
        host.records
            .read_history(&checkout.id, |doc, history| {
                assert!(
                    history.next_undo_as(doc, Some(&author())).is_some(),
                    "B's first mutation must finalize A's pending boundary in the record history"
                );
            })
            .unwrap();
    }

    // Case `pending_local_burst_finalizes_before_an_external_remote_merge`.
    {
        let host = ProbeHost::new();
        let component =
            instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let checkout = component.record().clone();
        let mut remote = host.records.with(&checkout.id, AutoCommit::fork).unwrap();
        remote.set_actor(automerge::ActorId::from(b"remote".to_vec()));
        remote.put(ROOT, "remote", "kept").unwrap();
        let mut probe = BoundProbe::new(component);
        let mut outer = AutoCommit::new();
        probe
            .invoke(&mut outer, |_, doc, _| {
                doc.put(ROOT, "local", "a1").unwrap();
            })
            .unwrap();
        host.records
            .with(&checkout.id, |doc| doc.merge(&mut remote).unwrap())
            .unwrap();
        host.records
            .with_history(&checkout.id, |doc, history| {
                history
                    .undo_in(doc, &author(), 11, None, None, None)
                    .unwrap();
                assert!(read_string(doc, "local").is_none());
                assert_eq!(read_string(doc, "remote").as_deref(), Some("kept"));
                assert_eq!(read_string(doc, "body").as_deref(), Some("initialized"));
            })
            .unwrap();
        assert!(outer.get_heads().is_empty());
    }

    // Case `foreign_noop_commit_notification_preserves_pending_owner`.
    {
        use crate::support::application::{probe_commit_drains, report_probe_action};
        let host = ProbeHost::new();
        let first = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let checkout = first.record().clone();
        let mut a = BoundProbe::new(first);
        let mut b = BoundProbe::new(
            instantiate_probe(&host, 12, existing(&checkout), RestoreInput::None).unwrap(),
        );
        host.records
            .with_history(&checkout.id, |doc, history| {
                let before = doc.get_heads();
                doc.put(ROOT, "redo-b", true).unwrap();
                history.record_committed_action_in(doc, before, None, None, author(), 12);
                history
                    .undo_in(doc, &author(), 12, None, None, None)
                    .unwrap();
            })
            .unwrap();
        let revision = host
            .records
            .read_history(&checkout.id, |_, history| history.revision())
            .unwrap();
        let mut outer = AutoCommit::new();
        let outer_before = outer.get_heads();
        // A's burst opens before its first write; its owner reports that boundary
        // once the burst ends.
        let a_burst = host
            .records
            .with(&checkout.id, AutoCommit::get_heads)
            .unwrap();
        a.invoke(&mut outer, |widget, doc, _| {
            tilo_ui::undo::set_state(widget, "A metadata");
            doc.put(ROOT, "a", "first").unwrap();
        })
        .unwrap();
        // B reports an action that wrote nothing while A's burst is pending.
        b.invoke(&mut outer, |widget, doc, _| {
            tilo_ui::undo::set_state(widget, "B metadata");
            report_probe_action(doc.get_heads());
        })
        .unwrap();
        assert_eq!(probe_commit_drains(), 1);
        host.records
            .read_history(&checkout.id, |doc, history| {
                assert_eq!(history.revision(), revision);
                assert!(history.next_undo_in(doc, &author(), 11).is_none());
                assert!(history.next_redo_in(doc, &author(), 12).is_some());
            })
            .unwrap();
        a.invoke(&mut outer, |_, doc, _| {
            doc.put(ROOT, "a", "continued").unwrap();
            report_probe_action(a_burst);
        })
        .unwrap();
        assert_eq!(probe_commit_drains(), 2);
        host.records
            .with_history(&checkout.id, |doc, history| {
                let entry = history.next_undo_in(doc, &author(), 11).unwrap();
                assert_eq!(entry.widget_metadata.as_deref(), Some("A metadata"));
                assert_eq!(entry.author.as_ref(), Some(&author()));
                assert!(history.next_redo_in(doc, &author(), 12).is_some());
                history
                    .undo_in(doc, &author(), 11, None, None, None)
                    .unwrap();
                assert!(history.next_undo_in(doc, &author(), 11).is_none());
                assert!(doc.get(ROOT, "a").unwrap().is_none());
            })
            .unwrap();
        assert_eq!(outer.get_heads(), outer_before);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn record_entry_unwind_and_restore_keep_exact_instance_ownership() {
    // Case `nested_same_record_entry_and_return_keep_separate_owner_segments`.
    {
        let host = ProbeHost::new();
        let first = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let checkout = first.record().clone();
        let mut b = BoundProbe::new(
            instantiate_probe(&host, 12, existing(&checkout), RestoreInput::None).unwrap(),
        );
        let mut a = BoundProbe::new(first);
        let mut outer = AutoCommit::new();
        a.invoke(&mut outer, |_, doc, _| {
            doc.put(ROOT, "a", "a1").unwrap();
            b.invoke(doc, |_, doc, _| doc.put(ROOT, "b", "b1").unwrap())
                .unwrap();
            doc.put(ROOT, "a", "a2").unwrap();
        })
        .unwrap();
        b.invoke(&mut outer, |_, doc, _| doc.put(ROOT, "b", "b2").unwrap())
            .unwrap();
        host.records
            .with_history(&checkout.id, |doc, history| {
                assert!(history.next_undo_in(doc, &author(), 12).is_some());
                history
                    .undo_in(doc, &author(), 11, None, None, None)
                    .unwrap();
                assert_eq!(read_string(doc, "a").as_deref(), Some("a1"));
                assert_eq!(read_string(doc, "b").as_deref(), Some("b2"));
                assert_eq!(
                    history.next_undo_in(doc, &author(), 11).unwrap().author,
                    Some(author())
                );
            })
            .unwrap();
        assert!(outer.get_heads().is_empty());
    }

    // Case `unwind_restores_owned_history_and_cross_record_entry_refuses_before_delivery`.
    {
        let host = ProbeHost::new();
        let first = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let checkout = first.record().clone();
        let mut b = BoundProbe::new(
            instantiate_probe(&host, 12, existing(&checkout), RestoreInput::None).unwrap(),
        );
        let other = instantiate_probe(&host, 13, LaunchTarget::New, RestoreInput::None).unwrap();
        let other_checkout = other.record().clone();
        let mut c = BoundProbe::new(other);
        let mut a = BoundProbe::new(first);
        let mut outer = AutoCommit::new();
        let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            a.invoke(&mut outer, |_, doc, _| {
                assert!(matches!(
                    c.invoke(doc, |_, _, _| panic!("cross-record delivery")),
                    Err(tilo_ui::bound_dispatch::BoundDispatchError::CrossRecordReentry)
                ));
                doc.put(ROOT, "a", "survives unwind").unwrap();
                panic!("controlled mutation unwind");
            })
            .unwrap();
        }));
        assert!(failure.is_err());
        b.invoke(&mut outer, |_, doc, _| doc.put(ROOT, "b", "b1").unwrap())
            .unwrap();
        host.records
            .with_history(&checkout.id, |doc, history| {
                assert_eq!(
                    history
                        .next_undo_in(doc, &author(), 11)
                        .unwrap()
                        .instance_id,
                    Some(11)
                );
                history
                    .undo_in(doc, &author(), 11, None, None, None)
                    .unwrap();
                assert_eq!(read_string(doc, "a"), None);
                assert_eq!(read_string(doc, "b").as_deref(), Some("b1"));
            })
            .unwrap();
        host.records
            .read_history(&other_checkout.id, |doc, history| {
                assert!(history.next_undo_in(doc, &author(), 13).is_none());
            })
            .unwrap();
        assert!(outer.get_heads().is_empty());
    }

    // Case `bound_revision_restore_and_inverse_keep_exact_instance`.
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "body", "initialized").unwrap();
        let target = doc.get_heads();
        let mut history = UndoLog::new(&mut doc);
        let before = doc.get_heads();
        doc.put(ROOT, "body", "edited").unwrap();
        history.record_committed_action_in(&mut doc, before, None, None, author(), 11);
        assert!(history.restore_to_heads_in(&mut doc, &target, None, None, author(), 12));
        assert_eq!(read_string(&doc, "body").as_deref(), Some("initialized"));
        assert_eq!(
            history
                .undo_in(&mut doc, &author(), 12, None, None, None)
                .unwrap()
                .instance_id,
            Some(12)
        );
        assert_eq!(read_string(&doc, "body").as_deref(), Some("edited"));
        assert_eq!(
            history
                .redo_in(&mut doc, &author(), 12, None, None, None)
                .unwrap()
                .instance_id,
            Some(12)
        );
        assert_eq!(read_string(&doc, "body").as_deref(), Some("initialized"));
        assert_eq!(
            history
                .next_undo_in(&doc, &author(), 11)
                .unwrap()
                .instance_id,
            Some(11)
        );
    }
}

fn read_string(doc: &AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .unwrap()
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
}

fn edit(doc: &mut AutoCommit, history: &mut UndoLog, instance: u64, key: &str, value: &str) {
    let before = doc.get_heads();
    doc.put(ROOT, key, value).unwrap();
    assert!(history.record_committed_action_in(doc, before, None, None, author(), instance));
}

#[test]
fn instance_history_selects_only_its_own_attributed_entries() {
    // Case `interleaved_instance_history_preserves_the_author_and_remote_changes`.
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "schema", "initialized").unwrap();
        let mut history = UndoLog::new(&mut doc);
        edit(&mut doc, &mut history, 11, "a", "a1");
        let mut remote = doc.fork();
        remote.put(ROOT, "remote", "kept").unwrap();
        doc.merge(&mut remote).unwrap();
        edit(&mut doc, &mut history, 12, "b", "b1");
        edit(&mut doc, &mut history, 11, "a", "a2");
        assert_eq!(
            history
                .next_undo_in(&doc, &author(), 11)
                .unwrap()
                .instance_id,
            Some(11)
        );
        assert_eq!(
            history.next_undo_in(&doc, &author(), 12).unwrap().author,
            Some(author())
        );
        history
            .undo_in(&mut doc, &author(), 11, None, None, None)
            .unwrap();
        assert_eq!(read_string(&doc, "a").as_deref(), Some("a1"));
        assert_eq!(read_string(&doc, "b").as_deref(), Some("b1"));
        assert_eq!(read_string(&doc, "remote").as_deref(), Some("kept"));
        history
            .undo_in(&mut doc, &author(), 12, None, None, None)
            .unwrap();
        assert_eq!(read_string(&doc, "b"), None);
        history
            .redo_in(&mut doc, &author(), 11, None, None, None)
            .unwrap();
        history
            .redo_in(&mut doc, &author(), 12, None, None, None)
            .unwrap();
        assert_eq!(read_string(&doc, "a").as_deref(), Some("a2"));
        assert_eq!(read_string(&doc, "b").as_deref(), Some("b1"));
        assert_eq!(read_string(&doc, "schema").as_deref(), Some("initialized"));
        assert_eq!(read_string(&doc, "remote").as_deref(), Some("kept"));
    }

    // Case `an_edit_in_b_keeps_a_redo_and_setup_is_never_undone`.
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "schema", "initialized").unwrap();
        let mut history = UndoLog::new(&mut doc);
        edit(&mut doc, &mut history, 11, "a", "a1");
        history
            .undo_in(&mut doc, &author(), 11, None, None, None)
            .unwrap();
        edit(&mut doc, &mut history, 12, "b", "b1");
        assert_eq!(
            history
                .next_redo_in(&doc, &author(), 11)
                .unwrap()
                .instance_id,
            Some(11)
        );
        history
            .redo_in(&mut doc, &author(), 11, None, None, None)
            .unwrap();
        assert_eq!(read_string(&doc, "a").as_deref(), Some("a1"));
        assert_eq!(read_string(&doc, "b").as_deref(), Some("b1"));
        history
            .undo_in(&mut doc, &author(), 11, None, None, None)
            .unwrap();
        assert!(
            history
                .undo_in(&mut doc, &author(), 11, None, None, None)
                .is_none()
        );
        assert_eq!(read_string(&doc, "schema").as_deref(), Some("initialized"));
    }

    // Case `bound_history_selection_skips_newer_foreign_and_unattributed_entries`.
    {
        let mut doc = AutoCommit::new();
        let mut history = UndoLog::new(&mut doc);
        edit(&mut doc, &mut history, 11, "a", "a1");
        edit(&mut doc, &mut history, 12, "b", "b1");
        let before = doc.get_heads();
        doc.put(ROOT, "unattributed", "kept").unwrap();
        history.record_committed_action(&mut doc, before, None, None);
        let selected = history
            .undo_in(&mut doc, &author(), 11, None, None, None)
            .unwrap();
        assert_eq!(selected.instance_id, Some(11));
        assert_eq!(read_string(&doc, "a"), None);
        assert_eq!(read_string(&doc, "b").as_deref(), Some("b1"));
        assert_eq!(read_string(&doc, "unattributed").as_deref(), Some("kept"));
        assert!(history.next_undo_in(&doc, &author(), 11).is_none());
        assert!(
            history
                .next_undo_in(&doc, &tilo_ui::permissions::UserId("other".into()), 12)
                .is_none()
        );
        assert_eq!(
            history.next_redo_in(&doc, &author(), 11).unwrap().author,
            Some(author())
        );
    }
}
