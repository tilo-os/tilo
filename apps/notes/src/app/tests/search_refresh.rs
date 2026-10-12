use super::*;
use tilo_automerge::ApplyIncoming as _;
use ui::tilo_resource::Reader;

struct Fixture {
    app: NotesApp,
    state: ui::State,
    _tmp: TempDir,
    pages: [PageId; 3],
    reader: Reader<crate::resource::Notebook>,
}

impl Fixture {
    fn new() -> Self {
        let (mut app, mut state, tmp, source) = app_seeded_into_doc();
        let pages = [source, PageId::new_v4(), PageId::new_v4()];
        for page in &pages[1..] {
            let create = actions::create_page_action(*page, None, "Destination");
            tilo_actions::apply_local(&mut state.doc, &create, actions::encode(&create)).unwrap();
            let body =
                actions::write_body_action(*page, &MainPanel::seed_blocks("Untouched", |_| None));
            tilo_actions::apply_local(&mut state.doc, &body, actions::encode(&body)).unwrap();
        }
        state.doc.get_heads();
        let reader = Reader::<crate::resource::Notebook>::read(&state.doc).unwrap();
        app.adopt_resource_state(reader.state());
        app.frame(&mut state.doc, &state.root);
        Self {
            app,
            state,
            _tmp: tmp,
            pages,
            reader,
        }
    }

    fn peer(&mut self, action: &actions::Action) {
        let mut peer = self.state.doc.fork();
        tilo_actions::apply_local_by(&mut peer, action, actions::encode(action), Some(b"peer"))
            .unwrap();
        self.state.doc.merge_incoming(&mut peer).unwrap();
        assert!(self.reader.sync(&self.state.doc).unwrap());
    }

    fn present(&mut self) {
        self.app.adopt_resource_state(self.reader.state());
        self.app.frame(&mut self.state.doc, &self.state.root);
        assert_search_snapshot_matches_canonical(&self.app, &self.state.doc);
    }

    fn splice(&self, page: PageId, text: &str) -> actions::Action {
        let block = crate::read::page_block_snapshots(&self.state.doc, page)[0]
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
    let old_untouched = f.app.search_blocks[&untouched].clone();
    f.peer(&f.splice(source, "Peer é😀 "));
    f.app.adopt_resource_state(f.reader.state());
    assert_eq!(
        f.app.unindexed.iter().copied().collect::<Vec<_>>(),
        [source],
        "only the edited note waits to be indexed"
    );
    f.present();
    assert!(
        f.app.search_blocks[&source][0]
            .text
            .starts_with("Peer é😀 ")
    );
    assert_eq!(f.app.search_blocks[&untouched], old_untouched);
    let block = f.app.search_blocks[&source][0].id.clone();
    f.peer(&actions::set_mark_action(
        source,
        &block,
        0..4,
        &ui::widget::Mark::Link(target),
    ));
    f.present();
    assert_eq!(f.app.backlink_index.get(&target), Some(&vec![(source, 1)]));
    f.peer(&actions::set_mark_action(
        source,
        &block,
        0..4,
        &ui::widget::Mark::Link(untouched),
    ));
    f.present();
    assert!(!f.app.backlink_index.contains_key(&target));
    assert_eq!(
        f.app.backlink_index.get(&untouched),
        Some(&vec![(source, 1)])
    );
    assert_eq!(f.app.search_blocks[&untouched], old_untouched);
    f.peer(&actions::delete_page_action(untouched));
    f.present();
    assert!(!f.app.search_blocks.contains_key(&untouched));
    assert!(!f.app.backlink_index.contains_key(&untouched));
}

/// Several changes shown at once are all indexed, whatever came between.
#[test]
fn changes_presented_together_are_all_indexed() {
    let mut f = Fixture::new();
    let [source, target, _] = f.pages;
    f.peer(&f.splice(source, "First "));
    f.app.adopt_resource_state(f.reader.state());
    f.peer(&f.splice(target, "Second "));
    f.present();
    assert!(f.app.search_blocks[&source][0].text.starts_with("First "));
    assert!(f.app.search_blocks[&target][0].text.starts_with("Second "));
}

/// A notebook whose notes were replaced wholesale indexes from nothing.
#[test]
fn replacing_the_notes_drops_their_search_and_backlink_rows() {
    let mut f = Fixture::new();
    let [source, target, _] = f.pages;
    let block = f.app.search_blocks[&source][0].id.clone();
    f.peer(&actions::set_mark_action(
        source,
        &block,
        0..4,
        &ui::widget::Mark::Link(target),
    ));
    f.present();
    assert_eq!(f.app.backlink_index.get(&target), Some(&vec![(source, 1)]));
    let mut peer = f.state.doc.fork();
    peer.put_object(automerge::ROOT, "pages", automerge::ObjType::Map)
        .unwrap();
    f.state.doc.merge_incoming(&mut peer).unwrap();
    assert!(f.reader.sync(&f.state.doc).unwrap());
    f.present();
    assert!(f.app.search_blocks.is_empty());
    assert!(f.app.backlink_index.is_empty());
}
