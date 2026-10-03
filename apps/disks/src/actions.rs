//! Resource + action-model status for the Disks app.
//!
//! The Tilo action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`](tilo_actions::Action) — a serializable value that
//! deterministically changes the shared Automerge document, so undo and
//! multi-peer replay come for free — while presentation / view state stays plain
//! app code. Applying that split to Disks leaves no *replicated document*
//! actions, but it does publish its real machine-local effects as resource
//! operations so search, agents, history and collaboration all use one safety
//! contract.
//!
//! ## Everything that changes the world is external
//!
//! Mount, unmount, format, eject and benchmark are privileged, **per-machine**
//! effects against real block devices: each dispatches a udisks2 D-Bus call (or
//! the `udisksctl` / `mount` / `mkfs` CLI fallback) through [`crate::ops`], the
//! app's external side-effect boundary. The live event path runs them off the UI
//! thread — [`DisksApp`](crate::app::DisksApp)'s worker calls `ops::run`, which
//! takes device leaf names and returns a device-level `Result<String, String>`;
//! it is handed **no** `AutoCommit` and can no more mutate the shared document
//! than `mount(8)` can. By the recipe's rule none of this is an action.
//!
//! Modelling any of these as a replicated [`Action`](tilo_actions::Action) would
//! be actively **wrong**: mounting a filesystem is an effect on *one* machine, so
//! replaying a "mounted /dev/sda1" envelope on a collaborator would make their
//! document claim a device is mounted that their machine never touched. The
//! world lives outside the CRDT; the CRDT must not pretend to author it.
//!
//! ## The shared document is a system projection plus view state
//!
//! What lives in the Automerge document (see [`crate::doc::DisksDoc`]) is exactly:
//!
//! - `topology` — a JSON serialisation of the live block-device inventory the
//!   [`crate::probe`] derives from `lsblk` + `smartctl`. It is machine-derived,
//!   reconciled **wholesale** every time the system is re-probed (the exact call
//!   `Widget::sync` makes after a mount/format changes the on-disk layout), so it
//!   is a *mirror* of external system state, not a record a user edits character
//!   by character. A plain string scalar is the right granularity precisely
//!   because no one collaboratively authors it.
//! - `selected` / `selected_part` — which drive and partition are in focus;
//! - `tab` — the active detail tab (overview / smart / partitions);
//! - `pending` — the target of an open type-to-confirm dialog, or empty;
//! - `status` — the last status-line message.
//!
//! The recipe names "the current selection, an overlay being open" as
//! presentation that must *never* be an action; `selected`, `selected_part`,
//! `tab`, `pending` and `status` are exactly that. They live in the CRDT only so
//! the toolkit's screen persistence and any invited collaborator see the same
//! view — the generic `ui::persist` snapshot every app shares, not a
//! Disks-specific authored-record model.
//!
//! ## There is no user-authored collection to migrate
//!
//! What *would* legitimately be actions — a user-authored set of saved format
//! presets, named benchmark results, scheduled SMART checks, bookmarks or a
//! settings map persisted in the document — **do not exist** in this app. The
//! tests below prove the document holds only `topology` + the five view-state
//! keys and none of `presets`, `saved`, `configs`, `jobs`, `schedule`,
//! `benchmarks`, `history`, `bookmarks`, `favourites` or `settings`. The recipe
//! is explicit that fabricating a model just to have actions is the wrong move,
//! so this app does not.
//!
//! ## Consequence
//!
//! [`Widget::resources`](ui::resources::resources) publishes a storage
//! root, a resource for every drive and child resources for every partition.
//! Each meaningful operation is an `OperationKind::Effect` handled by
//! `disks.operation`: format and benchmark for disks/partitions, eject for a
//! removable disk, and the applicable mount/unmount operation for a partition.
//! The descriptors declare admin permission, background progress, retry policy
//! and an exact typed phrase for destructive formatting. Before an operation
//! starts, `DisksApp` revalidates the resource leaf against its latest local
//! probe and derives the filesystem type from that probe, rejecting stale or
//! forged remote payloads. [`take_committed_action`](ui::undo::CommittedActions::take_committed_action)
//! remains `None` because none of these effects may be replayed as a CRDT
//! mutation; the tests below prove both halves of that boundary.

#[cfg(test)]
mod tests {
    use ui::automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
    use ui::taffy::{NodeId, TaffyTree};
    use ui::widget::Widget;
    use ui::winit::dpi::PhysicalPosition;
    use ui::winit::event::{DeviceId, Modifiers, WindowEvent};

    use crate::app::DisksApp;
    use crate::doc::DisksDoc;
    use crate::model::Topology;
    use crate::ops::{self, CommandSpec, Op};

    fn device_id() -> DeviceId {
        // winit exposes no public DeviceId constructor; tests only need a value.
        unsafe { std::mem::zeroed() }
    }

    /// A cursor move through the real winit event path — exercises the live
    /// `Widget::handle_event` without hitting any action affordance.
    fn cursor_at(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: device_id(),
            position: PhysicalPosition::new(x, y),
        }
    }

    /// Pump one event through the production `Widget::handle_event`.
    fn send(app: &mut DisksApp, doc: &mut AutoCommit, ev: &WindowEvent) {
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: NodeId::new(0),
                taffy: &taffy,
            },
        );
    }

    /// A Disks app with its first `sync` run — the same state the live app (and
    /// the `--shots` path) reaches on its first frame. With `DISKS_LIVE` unset
    /// (the case in tests) the seed is the curated sample topology, so no probe,
    /// subprocess or device is ever touched.
    fn synced() -> (DisksApp, AutoCommit) {
        let mut app = DisksApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// What a Disks app that adopted `doc` on its first frame publishes.
    fn resources_for(doc: &AutoCommit) -> Vec<ui::tilo_actions::Resource> {
        let mut app = DisksApp::new();
        app.sync(&mut doc.clone(), &ROOT);
        ui::resources::resources(&app, doc)
    }

    // Select is a document action; effects stay unpublished without live disks
    // so the palette section is empty. The sweep still needs the disks decoder.
    ui::action_contract_tests! {
        seed: synced().1,
        registry: {
            let mut registry = ui::tilo_actions::ActionRegistry::new();
            <crate::resource::Disks as ui::tilo_resource::Resource>::register(&mut registry);
            registry
        },
        resources: |doc| resources_for(doc),
    }

    /// The CRDT holds only a verbatim, wholesale-reconciled topology projection plus view state, and round-trips byte-identically.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn document_is_a_durable_projection_of_topology_and_view_state() {
        // --- phase: document_holds_only_projected_topology_and_view_state ---
        // The whole CRDT footprint is a system projection (`topology`) plus per-peer
        // view state (`selected`, `selected_part`, `tab`, `pending`, `status`).
        // There is no user-authored record collection anywhere — no saved presets,
        // scheduled jobs, benchmark history, bookmarks or settings map — so there is
        // nothing for an action to mutate. Driven through the real `Widget::sync`.
        {
            let (_app, doc) = synced();

            // Exactly the six keys the app seeds — nothing structural besides them.
            let mut root_keys: Vec<String> = doc.keys(&ROOT).collect();
            root_keys.sort();
            assert_eq!(
                root_keys,
                vec![
                    "pending".to_string(),
                    "selected".to_string(),
                    "selected_part".to_string(),
                    "status".to_string(),
                    "tab".to_string(),
                    "topology".to_string(),
                ],
                "the document holds only the projected topology + view state: {root_keys:?}",
            );

            // `topology` is a JSON string scalar — a machine-derived projection, not a
            // collaboratively authored structure.
            match doc.get(&ROOT, "topology").unwrap() {
                Some((Value::Scalar(v), _)) => assert!(
                    matches!(v.as_ref(), ScalarValue::Str(_)),
                    "topology is a serialised projection string, got {v:?}",
                ),
                other => panic!("topology must be a scalar string, got {other:?}"),
            }

            // The remaining keys are plain view-state scalars: two Ints (selection)
            // and three Strs (tab / pending / status) — the recipe's "presentation".
            for key in ["selected", "selected_part"] {
                assert!(
                    matches!(
                        doc.get(&ROOT, key).unwrap(),
                        Some((Value::Scalar(v), _)) if matches!(v.as_ref(), ScalarValue::Int(_))
                    ),
                    "`{key}` is an integer view-state scalar",
                );
            }
            for key in ["tab", "pending", "status"] {
                assert!(
                    matches!(
                        doc.get(&ROOT, key).unwrap(),
                        Some((Value::Scalar(v), _)) if matches!(v.as_ref(), ScalarValue::Str(_))
                    ),
                    "`{key}` is a string view-state scalar",
                );
            }

            // No durable, user-authored record collection exists to hang an action
            // off — proving this isn't a "saved presets are actions" case and that we
            // did not fabricate a saved-configs model.
            for forbidden in [
                "presets",
                "preset",
                "saved",
                "configs",
                "config",
                "jobs",
                "schedule",
                "benchmarks",
                "history",
                "bookmarks",
                "favourites",
                "favorites",
                "settings",
                "prefs",
            ] {
                assert!(
                    doc.get(&ROOT, forbidden).unwrap().is_none(),
                    "document must not carry a `{forbidden}` record collection",
                );
            }
        }

        // --- phase: topology_is_a_verbatim_projection_reconciled_wholesale ---
        // `topology` is a *verbatim projection* of the probed inventory, reconciled
        // wholesale — exactly how the store keeps a mirror of external system state
        // rather than a user-authored record. The real `Widget::sync` serialises the
        // sample topology into the doc byte-for-byte; a subsequent re-probe (the same
        // `write_topology` call `sync` makes after a mount changes the layout)
        // *replaces* the scalar rather than editing it, which is what marks it a
        // mirror and not a collaborative document.
        {
            let mut app = DisksApp::new();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);

            let stored = DisksDoc::topology_json(&doc).expect("topology projected into the doc");
            assert_eq!(
                stored,
                serde_json::to_string(&Topology::sample()).unwrap(),
                "the doc holds a verbatim serialisation of the probed inventory",
            );

            // Re-probing to a different inventory overwrites the projection wholesale
            // (the production reconciliation path used by `Widget::sync`).
            DisksDoc::write_topology(&mut doc, &Topology::default());
            let after = DisksDoc::topology_json(&doc).unwrap();
            assert_eq!(
                after,
                serde_json::to_string(&Topology::default()).unwrap(),
                "re-probing replaces the projection wholesale",
            );
            assert_ne!(
                after, stored,
                "the mirror tracks the live system, not a user edit",
            );
        }

        // --- phase: projected_state_round_trips_byte_identically ---
        // The projected + view state is durable and converges for free through the
        // toolkit's native CRDT path — save/load lands a peer on the byte-identical
        // document (same head hashes, not merely the same values). This is precisely
        // the persistence + convergence an action executor would provide, so wrapping
        // the projection in an action would add nothing: no action is needed.
        {
            let (_app, mut doc) = synced();
            doc.commit();
            let bytes = doc.save();

            let mut peer = AutoCommit::load(&bytes).unwrap();
            assert_eq!(
                doc.get_heads(),
                peer.get_heads(),
                "byte-identical document on load",
            );

            let here = DisksDoc::topology_json(&doc).expect("topology present here");
            let there = DisksDoc::topology_json(&peer).expect("topology present on the peer");
            assert!(
                !here.is_empty() && here != serde_json::to_string(&Topology::default()).unwrap(),
                "the seed produced a real projected topology",
            );
            assert_eq!(here, there, "the peer sees the same projected inventory");
        }
    }

    /// Device operations are external effects exported as resource actions; they never write the shared document.
    #[test]
    fn device_operations_are_external_resource_actions() {
        // --- phase: device_operations_are_external_and_never_touch_the_document ---
        // A device operation is **external**: `ops::run` — the exact call the live
        // worker thread makes — opens/commands the real device and returns a
        // device-level `Result`, while the shared document's heads stay **unchanged**.
        // The execution boundary takes no `AutoCommit`, so it writes nothing to the
        // CRDT; hence mount/unmount/format/eject/benchmark are not actions. Uses a
        // device name that cannot exist, so nothing on the machine is touched.
        {
            let (_app, mut doc) = synced();
            doc.commit();
            let heads_before = doc.get_heads();

            // The production execution path (`DisksApp`'s worker calls `ops::run`).
            // Benchmark opens `/dev/<leaf>` directly; a bogus leaf fails immediately
            // with no side effect and no subprocess.
            let bench = ops::run(Op::Benchmark, "tilo-nonexistent-block-device", None);
            assert!(
                bench.is_err(),
                "the external op ran against the device and reported a device-level result",
            );

            // The CLI fallback chain is likewise a pure process effect — run it
            // against a tool that does not exist so nothing on the host is mutated.
            let chain = ops::run_command_chain(&[CommandSpec {
                program: "tilo-no-such-disk-tool".to_string(),
                args: Vec::new(),
            }]);
            assert!(
                chain.is_err(),
                "the CLI fallback is a process effect, not a doc write"
            );

            assert_eq!(
                doc.get_heads(),
                heads_before,
                "device operations mutate the system, never the shared document",
            );
        }

        // --- phase: exports_external_operations_without_document_actions ---
        // Device operations are exported as external resource actions while the
        // projected document remains action-free.
        {
            let (mut app, mut doc) = synced();

            // Real event path: move the pointer over the surface.
            send(&mut app, &mut doc, &cursor_at(200.0, 200.0));

            let resources = ui::resources::resources(&app, &doc);
            assert_eq!(resources.len(), 1, "one storage-system root");
            let system = &resources[0];
            assert_eq!(system.kind, "disks.system");
            let partition = system
                .children
                .iter()
                .flat_map(|drive| &drive.children)
                .find(|resource| resource.kind == "disks.partition")
                .expect("a partition resource");
            let actions: Vec<&str> = partition
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            for expected in ["benchmark", "format"] {
                assert!(
                    actions.contains(&expected),
                    "partition publishes {expected}: {actions:?}",
                );
            }
            assert!(
                actions.iter().any(|id| matches!(*id, "mount" | "unmount")),
                "partition publishes the mount action appropriate to its state: {actions:?}",
            );
            assert!(
                ui::undo::take_committed_action(&mut app).is_none(),
                "no action is dispatched, so no action-host undo boundary is recorded",
            );
        }
    }
}
