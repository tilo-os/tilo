use super::*;

struct Fixture {
    app: App,
    pages: [PageId; 3],
}

impl Fixture {
    fn new() -> Self {
        let (mut app, source) = seeded();
        let pages = [source, PageId::new_v4(), PageId::new_v4()];
        for page in &pages[1..] {
            apply(
                &mut app,
                &actions::create_page_action(*page, None, "Destination"),
            );
            apply(
                &mut app,
                &actions::write_body_action(*page, &MainPanel::seed_blocks("Untouched", |_| None)),
            );
        }
        app.settle();
        Self { app, pages }
    }

    fn peer(&mut self, action: &actions::Action) {
        arrive(&mut self.app, action);
    }

    fn notes(&mut self) -> &mut NotesApp {
        notes(&mut self.app)
    }

    fn present(&mut self) {
        present(&mut self.app);
        self.app.settle();
        assert_search_snapshot_matches_canonical(&mut self.app);
    }

    fn splice(&self, page: PageId, text: &str) -> actions::Action {
        let block = part_doc(&self.app, page, |doc| {
            crate::read::page_block_snapshots(doc, page)
        })[0]
            .0
            .clone();
        actions::edit_body_action(
            page,
            tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block,
                at: 0,
                del: 0,
                ins: text.into(),
            },
        )
    }
}

/// A peer's edit of one note reindexes that note alone: its text and the
/// links it makes, never another note's rows.
#[test]
fn a_peer_edit_reindexes_only_the_note_it_changed() {
    let mut f = Fixture::new();
    let [source, target, untouched] = f.pages;
    let old_untouched = f.notes().workspace.bodies[&untouched].clone();
    f.peer(&f.splice(source, "Peer é😀 "));
    present(&mut f.app);
    assert_eq!(
        f.notes().unindexed.iter().copied().collect::<Vec<_>>(),
        [source],
        "only the edited note waits to be indexed"
    );
    f.present();
    assert!(
        f.notes().workspace.bodies[&source][0]
            .text
            .starts_with("Peer é😀 ")
    );
    assert_eq!(f.notes().workspace.bodies[&untouched], old_untouched);
    let block = f.notes().workspace.bodies[&source][0].id.clone();
    f.peer(&actions::set_mark_action(
        source,
        &block,
        0..4,
        &ui::widget::Mark::Link(target),
    ));
    f.present();
    assert_eq!(
        f.notes().backlink_index.get(&target),
        Some(&vec![(source, 1)])
    );
    f.peer(&actions::set_mark_action(
        source,
        &block,
        0..4,
        &ui::widget::Mark::Link(untouched),
    ));
    f.present();
    assert!(!f.notes().backlink_index.contains_key(&target));
    assert_eq!(
        f.notes().backlink_index.get(&untouched),
        Some(&vec![(source, 1)])
    );
    assert_eq!(f.notes().workspace.bodies[&untouched], old_untouched);
    f.peer(&actions::delete_page_action(untouched));
    f.present();
    assert!(!f.notes().workspace.bodies.contains_key(&untouched));
    assert!(!f.notes().backlink_index.contains_key(&untouched));
}

/// Several changes shown at once are all indexed, whatever came between.
#[test]
fn changes_presented_together_are_all_indexed() {
    let mut f = Fixture::new();
    let [source, target, _] = f.pages;
    f.peer(&f.splice(source, "First "));
    present(&mut f.app);
    f.peer(&f.splice(target, "Second "));
    f.present();
    assert!(
        f.notes().workspace.bodies[&source][0]
            .text
            .starts_with("First ")
    );
    assert!(
        f.notes().workspace.bodies[&target][0]
            .text
            .starts_with("Second ")
    );
}

/// A notebook whose notes were replaced wholesale indexes from nothing.
#[test]
fn replacing_the_notes_drops_their_search_and_backlink_rows() {
    let mut f = Fixture::new();
    let [source, target, _] = f.pages;
    let block = f.notes().workspace.bodies[&source][0].id.clone();
    f.peer(&actions::set_mark_action(
        source,
        &block,
        0..4,
        &ui::widget::Mark::Link(target),
    ));
    f.present();
    assert_eq!(
        f.notes().backlink_index.get(&target),
        Some(&vec![(source, 1)])
    );
    // A peer replaces the notes with a new, empty map.
    f.app
        .replica_arrives(&crate::part::chain().part(0), |doc| {
            let pages = ui::tilo_resource::Slot::Key(&automerge::ROOT, "pages");
            pages.remove(doc).unwrap();
            pages.object(doc, automerge::ObjType::Map).unwrap();
        })
        .unwrap();
    f.present();
    assert!(f.notes().workspace.bodies.is_empty());
    assert!(f.notes().backlink_index.is_empty());
}
