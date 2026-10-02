#[cfg(test)]
mod workspace_activity {
    use super::*;
    use automerge::{
        AutoCommit, ROOT,
        transaction::{CommitOptions, Transactable},
    };
    use tilo_desktop::activity::ActivitySnapshot;
    use tilo_desktop_model::{DesktopTree, StripId, Tab, TileContent, WindowGroup, WindowWidth};

    fn causal_room() -> ActivitySnapshot {
        let mut desktop = DesktopTree {
            workspaces: vec![tilo_desktop_model::Workspace::new(
                tilo_desktop_model::WorkspaceId(1),
                "Home",
            )],
            active: tilo_desktop_model::WorkspaceId(1),
            ..DesktopTree::default()
        };
        desktop.workspaces[0]
            .strips
            .push(tilo_desktop_model::Strip::new(StripId(1), "Main"));
        let room = desktop.active_workspace().unwrap().room;
        let strip = &mut desktop.workspaces[0].strips[0];
        strip.id = StripId::from((7 << 48) | 1);
        strip.window_groups.push(WindowGroup {
            id: (7 << 48) | 2,
            tabs: vec![],
            active_tab: (7 << 48) | 3,
            width: WindowWidth::Wide,
            maximized: false,
        });
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "room", room.to_string()).unwrap();
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
            });
            tilo_desktop_model::projection::project_owned_contents(
                &mut doc,
                std::slice::from_ref(strip),
                7,
                None,
                tilo_desktop_model::projection::EmptyStrips::Retain,
            )
            .unwrap();
            doc.commit_with(CommitOptions::default().with_time(n.cast_signed()));
        }
        ActivitySnapshot::new(desktop, doc).unwrap()
    }

    fn paint(pane: &mut RoomActivityPane) {
        pane.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    }

    // Copied native causal regression, using actual admitted room topology
    // commits and the same private playback/keyboard controller.
    #[test]
    fn replay_materializes_real_causal_layouts_and_exposes_controls() {
        let mut tree = causal_room();
        let records = RecordStore::in_memory();
        let record = ui::record::RecordCheckout::new(
            RecordId::for_file("review.note"),
            ui::record::RecordKind::new("note"),
        );
        assert!(records.open_record(&record));
        records.with(&record.id, |doc| {
            ui::record::init(
                doc,
                &record.id,
                &record.kind,
                &tree.active_workspace().unwrap().room.to_string(),
            );
            doc.put(ROOT, "body", "Keep live content").unwrap();
            doc.commit_with(CommitOptions::default().with_time(5));
        });
        let heads_before = tree.doc.get_heads();
        let record_heads_before = records
            .read(&record.id, |doc| doc.clone().get_heads())
            .unwrap();
        let mut pane = RoomActivityPane::new();
        pane.layout(Rect::new(0., 0., 1600., 900.));
        pane.begin_view(tree.active.raw());
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        pane.set_period(Period::AllTime);
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
        pane.set_view(ActivityView::Replay);
        paint(&mut pane);
        let order = pane.focus_order();
        assert!(order.contains(&ActivityFocus::ReplayBack));
        assert!(order.contains(&ActivityFocus::ReplayPlay));
        assert!(order.contains(&ActivityFocus::ReplayForward));
        assert!(!order.contains(&ActivityFocus::Scope(RoomScope::AllWorkspaces)));
        for focus in [
            ActivityFocus::ReplayForward,
            ActivityFocus::ReplayBack,
            ActivityFocus::ReplayPlay,
        ] {
            pane.keyboard_focus = Some(focus);
            assert!(pane.activate_focus(&mut tree, Some(&records)).handled);
        }
        assert!(pane.replay_is_playing());
        pane.set_author_filter(Some("absent".into()));
        pane.set_author_filter(None);
        let mut presence = PresenceCtx::empty();
        let ada = ui::permissions::UserId("ada".into());
        presence.names.insert(ada.clone(), "Ada".into());
        presence.set_pointer(
            &ui::presence::PresenceKey::desktop_pointer("design-room"),
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
        assert_eq!(pointer.point, (420., 260.));
        assert!(pointer.desktop_space);
        pane.finish_view(tree.active.raw());
        assert!(!pane.replay_is_playing());
        assert_eq!(tree.doc.get_heads(), heads_before);
        assert_eq!(
            records
                .read(&record.id, |doc| doc.clone().get_heads())
                .unwrap(),
            record_heads_before
        );
    }

    #[test]
    fn historical_topology_does_not_claim_current_geometry_is_historical() {
        let mut tree = causal_room();
        tree.resync_activity_cache().unwrap();
        assert_eq!(
            tree.workspaces[0].strips[0].window_groups[0].width,
            WindowWidth::Normal
        );
    }
    #[test]
    fn unavailable_mutation_host_hides_naming_and_rejects_restore() {
        struct Enabled;
        impl ActivityHost for Enabled {
            fn mutations_available(&self) -> bool {
                true
            }
        }
        let mut tree = causal_room();
        // Layout/setup commits are omitted from the feed; rename so there is
        // a Change row to select and attempt restore against.
        tree.workspaces[0].name = "Design studio".into();
        tilo_desktop_model::desktop_doc::seed(&mut tree.doc, &tree.desktop);
        tilo_resource::history::commit_stamped(&mut tree.doc);
        let mut pane = RoomActivityPane::new();
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

    #[test]
    fn malformed_historical_room_is_not_a_valid_replay_checkpoint() {
        let mut tree = causal_room();
        let room = tree.active_workspace().unwrap().room;
        tree.doc.delete(ROOT, "room").unwrap();
        let broken = tree
            .doc
            .commit_with(CommitOptions::default().with_time(6))
            .unwrap();
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&broken.0[..16]);
        let broken_id = HistoryEntryId::from_bytes(bytes);
        tree.doc.put(ROOT, "room", room.to_string()).unwrap();
        tree.doc.commit_with(CommitOptions::default().with_time(7));
        let mut pane = RoomActivityPane::new();
        pane.refresh(&mut tree);
        assert!(pane.entries().iter().any(|entry| entry.id == broken_id));
        assert!(
            !pane
                .replay_frames
                .iter()
                .any(|frame| frame.entry_id == broken_id)
        );
        assert_eq!(pane.replay_unavailable_checkpoints(), 1);
    }
}
