#[cfg(test)]
mod workspace_activity {
    use super::*;
    use crate::activity::ActivitySnapshot;
    use automerge::{
        AutoCommit, ROOT,
        transaction::{CommitOptions, Transactable},
    };
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
        let room = desktop.active_workspace().unwrap().room();
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
                app_title: None,
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

    // Copied native causal regression, using actual admitted room topology
    // commits and the same private playback/keyboard controller.
    #[test]
    fn replay_materializes_real_causal_layouts() {
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
                &tree.active_workspace().unwrap().room().to_string(),
            );
            doc.put(ROOT, "body", "Keep live content").unwrap();
            doc.commit_with(CommitOptions::default().with_time(5));
        });
        let heads_before = tree.doc.get_heads();
        let record_heads_before = records
            .read(&record.id, |doc| doc.clone().get_heads())
            .unwrap();
        let mut pane = RoomActivityPane::new();
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
        assert_eq!(pane.serve_replay().frames.len(), pane.replay_frames.len());
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
        // Layout/setup commits are omitted from the feed; rename the room so
        // there is a Change row to select and attempt restore against.
        // `causal_room` is a room projection doc (not a desktop_doc workspaces
        // list), so write the name on the room document directly.
        tree.workspaces[0].name = "Design studio".into();
        tree.doc.put(ROOT, "name", "Design studio").unwrap();
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
        let room = tree.active_workspace().unwrap().room();
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

/// Two rooms, each with the admitted document a host shares: real changes,
/// by their real (remote) actors, hashes and times. Nothing here is re-derived
/// from a desktop projection, so a row that is not in one of these documents
/// must not exist.
#[cfg(test)]
mod multiple_admitted_rooms {
    use super::*;
    use crate::activity::{ActivityTree, HistoryRoom, RoomScope};
    use automerge::{
        ActorId, AutoCommit, ROOT,
        transaction::{CommitOptions, Transactable},
    };
    use std::collections::BTreeMap;
    use tilo_desktop_model::{
        AppKind, DesktopTree, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        Workspace, WorkspaceId,
    };

    #[derive(Clone)]
    struct TwoRooms {
        desktop: DesktopTree,
        rooms: BTreeMap<RoomId, AutoCommit>,
        empty: AutoCommit,
    }

    impl TwoRooms {
        fn active_room(&self) -> Option<RoomId> {
            self.desktop.active_workspace().map(Workspace::room)
        }
        fn document_for(&self, room: RoomId) -> Option<&AutoCommit> {
            self.rooms.get(&room)
        }
    }

    impl std::ops::Deref for TwoRooms {
        type Target = DesktopTree;
        fn deref(&self) -> &DesktopTree {
            &self.desktop
        }
    }
    impl std::ops::DerefMut for TwoRooms {
        fn deref_mut(&mut self) -> &mut DesktopTree {
            &mut self.desktop
        }
    }
    impl ActivityTree for TwoRooms {
        fn document(&self) -> &AutoCommit {
            self.active_room()
                .and_then(|room| self.document_for(room))
                .unwrap_or(&self.empty)
        }
        fn document_mut(&mut self) -> &mut AutoCommit {
            let Some(room) = self.active_room() else {
                return &mut self.empty;
            };
            if self.rooms.contains_key(&room) {
                return self.rooms.get_mut(&room).expect("the room's document");
            }
            &mut self.empty
        }
        /// Every workspace's room, the active one first: a room whose
        /// document is absent is still named, so its gap is reported.
        fn history_rooms(&self) -> Vec<HistoryRoom> {
            let active = self.active_room();
            let mut rooms: Vec<RoomId> = self
                .desktop
                .workspaces
                .iter()
                .map(Workspace::room)
                .collect();
            if let Some(active) = active
                && let Some(index) = rooms.iter().position(|room| *room == active)
            {
                let room = rooms.remove(index);
                rooms.insert(0, room);
            }
            rooms.into_iter().map(Some).collect()
        }
        fn history_document(&self, room: HistoryRoom) -> Option<&AutoCommit> {
            self.document_for(room?)
        }
        fn history_document_mut(&mut self, room: HistoryRoom) -> Option<&mut AutoCommit> {
            self.rooms.get_mut(&room?)
        }
        fn history_room_writable(&self, room: HistoryRoom) -> bool {
            room.is_some_and(|room| self.rooms.contains_key(&room))
        }
        fn resync_activity_cache(&mut self) -> Result<(), String> {
            let rooms: Vec<(RoomId, AutoCommit)> = self
                .rooms
                .iter()
                .map(|(room, doc)| (*room, doc.clone()))
                .collect();
            for (room, doc) in rooms {
                tilo_desktop_model::projection::apply_admitted_room(&mut self.desktop, room, &doc)?;
            }
            Ok(())
        }
    }

    /// One room's admitted document: its name and one window whose title the
    /// room's people change, each change by `actor` at `time`.
    fn authored_room(
        room: RoomId,
        name: &str,
        actor: [u8; 32],
        titles: &[(i64, &str)],
    ) -> (AutoCommit, Vec<automerge::ChangeHash>) {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(actor.to_vec()));
        doc.put(ROOT, "room", room.to_string()).unwrap();
        doc.put(ROOT, "name", name).unwrap();
        let _ = doc.commit_with(CommitOptions::default().with_time(1_700_000_000));
        let strip = Strip {
            id: StripId((7 << 48) | 1),
            name: "Main".into(),
            window_groups: vec![],
        };
        let mut hashes = Vec::new();
        for (time, title) in titles {
            let mut strip = strip.clone();
            strip.window_groups = vec![WindowGroup {
                id: (7 << 48) | 2,
                active_tab: (7 << 48) | 3,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
                tabs: vec![Tab {
                    id: (7 << 48) | 3,
                    title: (*title).into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("so.tilo.notes".into()),
                        label: "Notes".into(),
                    },
                    app_title: None,
                }],
            }];
            tilo_desktop_model::projection::project_owned_contents(
                &mut doc,
                std::slice::from_ref(&strip),
                7,
                None,
                tilo_desktop_model::projection::EmptyStrips::Retain,
            )
            .unwrap();
            hashes.push(
                doc.commit_with(CommitOptions::default().with_time(*time))
                    .unwrap(),
            );
        }
        (doc, hashes)
    }

    fn two_rooms() -> (TwoRooms, RoomId, RoomId) {
        let alpha_room = RoomId::new();
        let beta_room = RoomId::new();
        let alpha = Workspace::in_room(WorkspaceId(1), alpha_room, "Alpha");
        let beta = Workspace::in_room(WorkspaceId(2), beta_room, "Beta");
        let desktop = DesktopTree {
            active: alpha.id,
            workspaces: vec![alpha, beta],
            ..DesktopTree::default()
        };
        let (alpha_doc, _) = authored_room(
            alpha_room,
            "Alpha",
            [0xa1; 32],
            &[(1_700_000_100, "Alpha plan")],
        );
        let (beta_doc, _) = authored_room(
            beta_room,
            "Beta",
            [0xb2; 32],
            &[(1_700_000_200, "Beta draft")],
        );
        (
            TwoRooms {
                desktop,
                rooms: BTreeMap::from([(alpha_room, alpha_doc), (beta_room, beta_doc)]),
                empty: AutoCommit::new(),
            },
            alpha_room,
            beta_room,
        )
    }

    fn entry(hash: &automerge::ChangeHash) -> ui::widget::HistoryEntryId {
        ui::history_source::entry_id_from_hash(&hash.to_string())
    }

    /// A host that knows no identity for a document's actor: a peer's change
    /// stays attributed to its own actor rather than to this device's person.
    #[derive(Clone, Default)]
    struct NoLocalActor;

    impl crate::activity::ActivityHost for NoLocalActor {
        fn local_user(&self) -> String {
            "local".into()
        }
        fn all_workspaces_available(&self) -> bool {
            true
        }
        fn mutations_available(&self) -> bool {
            true
        }
    }

    fn millis(secs: u64) -> ui::time::SystemTime {
        ui::time::UNIX_EPOCH
            .checked_add(ui::time::Duration::from_secs(secs))
            .expect("a representable time")
    }

    /// Activity lists each room's own changes with the actor, hash and time
    /// the room admitted them under; widening to every workspace lists both
    /// rooms' changes, and Replay shows each room as its document stood.
    #[test]
    fn every_rooms_change_reads_from_its_own_admitted_document() {
        let (mut tree, _alpha_room, beta_room) = two_rooms();
        let mut pane = RoomActivityPane::new();
        pane.set_host(Rc::new(NoLocalActor));
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &RecordStore::in_memory());

        let this_room = pane
            .entries()
            .iter()
            .map(|entry| entry.summary.clone())
            .collect::<Vec<_>>();
        assert!(
            this_room.iter().any(|row| row.contains("Alpha plan")),
            "the active room's own admitted change: {this_room:?}"
        );
        assert!(
            !this_room.iter().any(|row| row.contains("Beta draft")),
            "another room's change stayed out of this room: {this_room:?}"
        );

        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        pane.refresh_records(&mut tree, &RecordStore::in_memory());
        let rows = pane.entries().to_vec();
        let summaries = rows
            .iter()
            .map(|entry| entry.summary.clone())
            .collect::<Vec<_>>();
        assert!(
            summaries.iter().any(|row| row.contains("Alpha plan"))
                && summaries.iter().any(|row| row.contains("Beta draft")),
            "widening reads every held room's document: {summaries:?}"
        );

        // The rows are the admitted changes themselves: their hash, actor and
        // stamp, not a projection of them. The feed's own rows keep the
        // admitted change id (the timeline projects them for its widgets).
        let beta_row = pane
            .derived
            .iter()
            .find(|(row, _)| row.summary.contains("Beta draft"))
            .map(|(row, _)| row.clone())
            .expect("Beta's change is a row");
        let beta = tree
            .rooms
            .get_mut(&beta_room)
            .expect("Beta's document")
            .get_changes(&[]);
        let admitted = beta
            .iter()
            .find(|change| entry(&change.hash()) == beta_row.id)
            .expect("the row is a change of Beta's admitted document");
        assert_eq!(
            admitted.actor_id().to_hex_string(),
            ActorId::from(vec![0xb2; 32]).to_hex_string(),
            "the change reads as the peer that wrote it"
        );
        assert_eq!(beta_row.timestamp, millis(1_700_000_200));
        assert_ne!(
            beta_row.author.name,
            crate::activity::testing::LOCAL_USER,
            "a peer's change is not attributed to this device's person"
        );

        // Replay shows both rooms as their documents stood at that change.
        let frame = pane
            .replay_frames
            .iter()
            .find(|frame| frame.entry_id == beta_row.id)
            .expect("Beta's change replays");
        let names = frame
            .workspaces
            .iter()
            .map(|workspace| workspace.name.as_str())
            .collect::<Vec<_>>();
        assert!(
            names.contains(&"Alpha") && names.contains(&"Beta"),
            "every room's layout at that moment: {names:?}"
        );
        assert_eq!(
            pane.replay_unavailable_checkpoints(),
            0,
            "each held room's checkpoint resolves from its document"
        );
        assert!(pane.feed_coverage().is_complete());
    }

    /// A room whose admitted document the host does not hold contributes no
    /// rows and is reported unavailable: a projection of it never stands in.
    #[test]
    fn an_unavailable_rooms_history_is_reported_not_projected() {
        let (mut tree, alpha_room, beta_room) = two_rooms();
        let mut beta_rows = tree.rooms.remove(&beta_room).expect("Beta was held");
        let target = entry(&beta_rows.get_changes(&[])[0].hash());
        let alpha_heads = tree
            .rooms
            .get_mut(&alpha_room)
            .expect("Alpha's document")
            .get_heads();
        let mut pane = RoomActivityPane::new();
        pane.set_host(Rc::new(crate::activity::testing::TestHost::default()));
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &RecordStore::in_memory());
        assert!(
            pane.entries()
                .iter()
                .all(|entry| !entry.summary.contains("Beta draft")),
            "an unread document's change is not shown from anywhere else"
        );
        assert_eq!(
            pane.feed_coverage().unavailable_rooms,
            vec![beta_room],
            "the room is named as unavailable"
        );
        assert!(!pane.feed_coverage().is_complete());
        assert!(
            !pane.revert_to(&mut tree, &target),
            "a change of an unread document is not a state to restore"
        );
        assert!(
            !tree.rooms.contains_key(&beta_room),
            "the restore did not invent a document for the unavailable room"
        );
        assert_eq!(
            tree.rooms
                .get_mut(&alpha_room)
                .expect("Alpha's document")
                .get_heads(),
            alpha_heads,
            "another room's document is untouched"
        );
    }

    /// A restore of a row writes a forward change into that row's own room
    /// document, and leaves every other document's history alone.
    #[test]
    fn restoring_a_row_writes_to_the_document_it_came_from() {
        let beta_room = RoomId::new();
        let (beta_doc, hashes) = authored_room(
            beta_room,
            "Beta",
            [0xb2; 32],
            &[(1_700_000_200, "Beta draft"), (1_700_000_300, "Beta final")],
        );
        let alpha_room = RoomId::new();
        let (alpha_doc, _) = authored_room(
            alpha_room,
            "Alpha",
            [0xa1; 32],
            &[(1_700_000_100, "Alpha plan")],
        );
        let mut tree = TwoRooms {
            desktop: DesktopTree {
                active: WorkspaceId(1),
                workspaces: vec![
                    Workspace::in_room(WorkspaceId(1), alpha_room, "Alpha"),
                    Workspace::in_room(WorkspaceId(2), beta_room, "Beta"),
                ],
                ..DesktopTree::default()
            },
            rooms: BTreeMap::from([(alpha_room, alpha_doc), (beta_room, beta_doc)]),
            empty: AutoCommit::new(),
        };
        let mut pane = RoomActivityPane::new();
        pane.set_host(Rc::new(crate::activity::testing::TestHost::default()));
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &RecordStore::in_memory());
        let target = entry(&hashes[0]);
        let alpha_heads = tree
            .rooms
            .get_mut(&alpha_room)
            .expect("Alpha's document")
            .get_heads();
        let beta_heads = tree
            .rooms
            .get_mut(&beta_room)
            .expect("Beta's document")
            .get_heads();
        assert!(
            pane.revert_to(&mut tree, &target),
            "the state at the first change is a different state to write"
        );
        assert_ne!(
            tree.rooms
                .get_mut(&beta_room)
                .expect("Beta's document")
                .get_heads(),
            beta_heads,
            "the restore is a change of Beta's document"
        );
        assert_eq!(
            tree.rooms
                .get_mut(&alpha_room)
                .expect("Alpha's document")
                .get_heads(),
            alpha_heads,
            "another room's document is untouched"
        );
        let restored = tree
            .rooms
            .get_mut(&beta_room)
            .expect("Beta's document")
            .get_changes(&beta_heads)
            .iter()
            .map(|change| change.message().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(restored.len(), 1, "one forward change: {restored:?}");
    }
}
