#[cfg(test)]
mod workspace_activity {
    use super::*;
    use crate::desktop::activity::ActivitySnapshot;
    #[allow(
        unused_imports,
        reason = "the store-backed history refresh is a method of this trait"
    )]
    use crate::desktop::activity::testing::StoreRecords as _;
    use automerge::{
        AutoCommit, ROOT,
        transaction::{CommitOptions, Transactable},
    };
    use tilo_desktop_model::projection::WORKSPACE_ID_KEY;
    use tilo_desktop_model::{Layout, StripId, Tab, TileContent, WindowGroup, WindowWidth};
    use ui::resource::ResourceStore;

    fn causal_workspace() -> ActivitySnapshot {
        let mut desktop = Layout {
            workspaces: vec![tilo_desktop_model::Workspace::new(
                tilo_desktop_model::WorkspaceId::from_u128(1),
                "Home",
            )],
            active: tilo_desktop_model::WorkspaceId::from_u128(1),
            ..Layout::default()
        };
        desktop.workspaces[0]
            .strips
            .push(tilo_desktop_model::Strip::new(StripId(1), "Main"));
        let workspace = desktop.active_workspace().unwrap().id();
        let strip = &mut desktop.workspaces[0].strips[0];
        strip.id = StripId::from((7 << 48) | 1);
        strip.window_groups.push(WindowGroup {
            id: (7 << 48) | 2,
            tabs: vec![],
            active_tab: (7 << 48) | 3,
            width: WindowWidth::Wide,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut doc = AutoCommit::new();
        doc.put(ROOT, WORKSPACE_ID_KEY, workspace.to_string())
            .unwrap();
        doc.put(ROOT, "name", "Design").unwrap();
        for (n, title) in [(3, "Plan"), (4, "Review")] {
            strip.window_groups[0].tabs.push(Tab {
                id: (7 << 48) | n,
                title: title.into(),
                tile: TileContent::LocalApp {
                    app_id: "org.tilo.Notes".into(),
                    label: title.into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            });
            let mut layout = tilo_desktop_model::Workspace::new(workspace, "Design");
            layout.strips = vec![strip.clone()];
            tilo_desktop_model::shared_resource::device::write(
                &mut doc,
                workspace,
                &layout,
                &tilo_desktop_model::shared_resource::device::Writer {
                    person: Some("ada"),
                    mine: &|id| id >> 48 == 7,
                    records: tilo_desktop_model::shared_resource::device::Writer::NOTHING_NOTED,
                    items: tilo_desktop_model::shared_resource::device::Writer::NOTHING_ITEMED,
                },
            )
            .unwrap();
            doc.commit_with(CommitOptions::default().with_time(n.cast_signed()));
        }
        ActivitySnapshot::new(desktop, doc).unwrap()
    }

    tilo_test_env::scenario! {
        fn workspace_replay_shows_only_what_history_holds;

        phase replay_materializes_real_causal_layouts;
        // Copied native causal regression, using actual admitted workspace topology
        // commits and the same private playback/keyboard controller.
        {
            let mut tree = causal_workspace();
            let records = ResourceStore::in_memory();
            let record = ui::resource::DocumentCheckout::new(
                DocumentId::for_file("review.note"),
                ui::resource::ResourceType::new("note"),
            );
            assert!(records.open_resource(&record));
            records.with(&record.id, |doc| {
                ui::resource::init(
                    doc,
                    &record.id,
                    &record.kind,
                    &tree.active_workspace().unwrap().id().to_string(),
                );
                doc.put(ROOT, "body", "Keep live content").unwrap();
                doc.commit_with(CommitOptions::default().with_time(5));
            });
            let heads_before = tree.doc.get_heads();
            let record_heads_before = records
                .read(&record.id, |doc| doc.clone().get_heads())
                .unwrap();
            let mut pane = WorkspaceActivityPane::new();
            pane.begin_view(tree.active);
            pane.refresh(&mut tree);
            pane.refresh_records(&mut tree, &records);
            assert!(pane.replay_frames.len() >= 2);
            let tab_counts: Vec<usize> = pane
                .replay_frames
                .iter()
                .map(|frame| {
                    frame
                        .workspaces
                        .iter()
                        .flat_map(|workspace| &workspace.groups)
                        .map(|group| group.tabs.len())
                        .sum()
                })
                .collect();
            assert!(
                tab_counts.windows(2).any(|pair| pair[0] != pair[1]),
                "causal forks must expose a changing real layout: {tab_counts:?}"
            );
            pane.view = ActivityView::Replay;
            assert_eq!(pane.serve_replay().frames.len(), pane.replay_frames.len());
            let mut presence = PresenceCtx::empty();
            let ada = MemberId::person("ada");
            presence.names.insert(ada.clone(), "Ada".into());
            presence.set_pointer(
                &ui::presence::PresenceKey::shell_pointer("design-workspace"),
                ada,
                (420., 260.),
            );
            pane.observe_replay_presence(&presence);
            let pointer = pane
                .replay_frames
                .last()
                .and_then(|frame| frame.pointers.first())
                .expect("session presence is retained against durable checkpoint");
            assert_eq!(pointer.name, "Ada");
            assert!(pointer.desktop_space);
            pane.finish_view(tree.active);
            assert!(!(pane.view == ActivityView::Replay && pane.replay_playing));
            assert_eq!(tree.doc.get_heads(), heads_before);
            assert_eq!(
                records
                    .read(&record.id, |doc| doc.clone().get_heads())
                    .unwrap(),
                record_heads_before
            );
        }

        phase historical_topology_does_not_claim_current_geometry_is_historical;
        {
            let mut tree = causal_workspace();
            tree.resync_activity_cache().unwrap();
            assert_eq!(
                tree.workspaces[0].strips[0].window_groups[0].width,
                WindowWidth::Normal
            );
        }

        phase unavailable_mutation_host_hides_naming_and_rejects_restore;
        {
            struct Enabled;
            impl ActivityHost for Enabled {
                fn mutations_available(&self) -> bool {
                    true
                }
            }
            let mut tree = causal_workspace();
            // Layout/setup commits are omitted from the feed; rename the workspace so
            // there is a Change row to select and attempt restore against.
            // `causal_workspace` is a workspace projection doc (not a desktop_doc workspaces
            // list), so write the name on the workspace document directly.
            tree.workspaces[0].name = "Design studio".into();
            tree.doc.put(ROOT, "name", "Design studio").unwrap();
            tilo_resource::history::commit_stamped(&mut tree.doc);
            let mut pane = WorkspaceActivityPane::new();
            pane.refresh(&mut tree);
            assert!(
                !pane.entries().is_empty(),
                "expected a feed-visible rename, got {:?}",
                pane.entries()
                    .iter()
                    .map(|e| e.summary.as_str())
                    .collect::<Vec<_>>()
            );
            pane.timeline.set_selection(Some((0, 0)));
            assert!(!pane.timeline.begin_naming_selection());
            let heads = tree.doc.get_heads();
            assert!(!pane.revert_to(&mut tree, &pane.entries()[0].id.clone()));
            assert_eq!(tree.doc.get_heads(), heads);
            pane.set_host(Rc::new(Enabled));
            assert!(pane.timeline.begin_naming_selection());
            pane.set_host(Rc::new(UnavailableActivityHost));
            assert!(!pane.timeline.begin_naming_selection());
            assert!(!pane.timeline.wants_keyboard());
        }

        phase malformed_historical_workspace_is_not_a_valid_replay_checkpoint;
        {
            let mut tree = causal_workspace();
            let workspace = tree.active_workspace().unwrap().id();
            tree.doc.delete(ROOT, WORKSPACE_ID_KEY).unwrap();
            let broken = tree
                .doc
                .commit_with(CommitOptions::default().with_time(6))
                .unwrap();
            let mut bytes = [0; 16];
            bytes.copy_from_slice(&broken.0[..16]);
            let broken_id = HistoryEntryId::from_bytes(bytes);
            tree.doc
                .put(ROOT, WORKSPACE_ID_KEY, workspace.to_string())
                .unwrap();
            tree.doc.commit_with(CommitOptions::default().with_time(7));
            let mut pane = WorkspaceActivityPane::new();
            pane.refresh(&mut tree);
            assert!(pane.entries().iter().any(|entry| entry.id == broken_id));
            assert!(
                !pane
                    .replay_frames
                    .iter()
                    .any(|frame| frame.entry_id == broken_id)
            );
            assert_eq!(pane.replay_unavailable, 1);
        }
    }
}
