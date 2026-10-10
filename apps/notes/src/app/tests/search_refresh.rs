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
            let create = actions::append_page_action(*page, None, "Destination");
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

    fn peer(&mut self, action: &NotesAction) {
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

    fn splice(&self, page: PageId, text: &str) -> NotesAction {
        let block = NotesDoc::page_block_snapshots(&self.state.doc, page)[0]
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

#[test]
fn projected_search_updates_peer_text_and_links_without_replacing_other_sources() {
    let mut f = Fixture::new();
    let [source, target, untouched] = f.pages;
    let old_untouched = f.app.search_blocks[&untouched].clone();
    f.peer(&f.splice(source, "Peer é😀 "));
    let receipt = f.reader.state().body_projection_refresh.as_ref().unwrap();
    assert_eq!(receipt.pages, vec![source]);
    assert_eq!(f.app.search_heads.as_ref(), Some(&receipt.before));
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
    assert!(f.reader.state().body_projection_refresh.is_none());
    f.present();
    assert!(!f.app.search_blocks.contains_key(&untouched));
    assert!(!f.app.backlink_index.contains_key(&untouched));
}

#[test]
fn missed_projection_revisions_and_pending_work_rebuild_actual_search_content() {
    let mut f = Fixture::new();
    let [source, target, untouched] = f.pages;
    f.peer(&f.splice(source, "First "));
    f.peer(&f.splice(target, "Second "));
    let receipt = f.reader.state().body_projection_refresh.as_ref().unwrap();
    assert_ne!(f.app.search_heads.as_ref(), Some(&receipt.before));
    f.present();
    assert!(f.app.search_blocks[&source][0].text.starts_with("First "));
    assert!(f.app.search_blocks[&target][0].text.starts_with("Second "));
    let body = NotesDoc::page_body(&f.state.doc, untouched).unwrap();
    let action = f.splice(untouched, "Pending ");
    // Resource execution writes a real uncommitted CRDT transaction.
    let NotesAction::EditBody { edit, .. } = action else {
        panic!("body action")
    };
    edit.apply_under(&mut f.state.doc, &body).unwrap();
    assert!(f.state.doc.pending_ops() > 0);
    f.app.refresh_search_blocks(&f.state.doc);
    assert_search_snapshot_matches_canonical(&f.app, &f.state.doc);
    assert!(f.app.search_heads.is_none());
    assert!(
        f.app.search_blocks[&untouched][0]
            .text
            .starts_with("Pending ")
    );
    f.state.doc.get_heads();
    assert!(f.reader.sync(&f.state.doc).unwrap());
    f.present();
}

#[test]
fn replacing_page_map_drops_search_and_backlink_rows() {
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
    assert!(f.reader.state().body_projection_refresh.is_none());
    f.present();
    assert!(f.app.search_blocks.is_empty());
    assert!(f.app.backlink_index.is_empty());
}
