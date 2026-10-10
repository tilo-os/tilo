use super::*;
use crate::actions::{self, NotesAction};
use ui::tilo_resource::Reader;

fn apply(doc: &mut Doc, action: &NotesAction) {
    tilo_actions::apply_local(doc, action, actions::encode(action)).unwrap();
}

fn fixture() -> (Doc, [PageId; 2]) {
    let mut doc = Doc::new();
    Notebook::initialize(&mut doc).unwrap();
    let pages = [PageId::from_u128(1), PageId::from_u128(2)];
    for (index, page) in pages.iter().enumerate() {
        apply(
            &mut doc,
            &actions::append_page_action(*page, None, &format!("Page {index}")),
        );
        let blocks = crate::MainPanel::seed_blocks("Original é😀", |_| None);
        apply(&mut doc, &actions::write_body_action(*page, &blocks));
    }
    doc.get_heads();
    (doc, pages)
}

fn edit(doc: &mut Doc, page: PageId) {
    let block = NotesDoc::page_block_snapshots(doc, page)[0].0.clone();
    apply(
        doc,
        &actions::edit_body_action(
            page,
            tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block,
                at: 0,
                del: 0,
                ins: "Remote ".into(),
            },
        ),
    );
}

fn same_projection(actual: &Workspace, expected: &Workspace) {
    assert_eq!(actual.data.pages, expected.data.pages);
    assert_eq!(actual.data.tags, expected.data.tags);
    assert_eq!(actual.data.tag_assignments, expected.data.tag_assignments);
    assert_eq!(actual.bodies, expected.bodies);
    assert_eq!(actual.body_states, expected.body_states);
    assert_eq!(
        actual.canonical_projection_heads,
        expected.canonical_projection_heads
    );
}

#[test]
fn actual_reader_refreshes_a_body_and_metadata_like_a_fresh_read() {
    let (mut doc, pages) = fixture();
    let mut reader = Reader::<Notebook>::read(&doc).unwrap();
    let untouched = reader.state().body_states.get(&pages[1]).unwrap().clone();
    edit(&mut doc, pages[0]);
    assert!(reader.sync(&doc).unwrap());
    same_projection(reader.state(), &Notebook::read(&doc).unwrap());
    assert_eq!(reader.state().body_states.get(&pages[1]), Some(&untouched));
    assert_eq!(
        reader.state().bodies[&pages[0]][0].text,
        "Remote Original é😀"
    );

    let page = reader.state().get(pages[0]).unwrap().clone();
    apply(
        &mut doc,
        &actions::rename_page_action(&page, "Renamed remotely"),
    );
    assert!(reader.sync(&doc).unwrap());
    same_projection(reader.state(), &Notebook::read(&doc).unwrap());
    assert_eq!(
        reader.state().get(pages[0]).unwrap().title,
        "Renamed remotely"
    );
}

#[test]
fn replacing_the_root_page_map_takes_a_fresh_canonical_projection() {
    let (mut doc, _) = fixture();
    let mut reader = Reader::<Notebook>::read(&doc).unwrap();
    doc.put_object(ROOT, "pages", ObjType::Map).unwrap();
    doc.get_heads();
    assert!(reader.sync(&doc).unwrap());
    same_projection(reader.state(), &Notebook::read(&doc).unwrap());
    assert!(reader.state().pages().is_empty());
    assert!(reader.state().bodies.is_empty());
    assert!(reader.state().body_states.is_empty());
}

#[test]
fn an_invalid_second_body_refuses_without_partially_refreshing_the_first() {
    let (mut doc, pages) = fixture();
    let mut reader = Reader::<Notebook>::read(&doc).unwrap();
    let before = (**reader.state()).clone();
    edit(&mut doc, pages[0]);
    let (_, entries) = doc.get(ROOT, "pages").unwrap().unwrap();
    let (_, page) = doc.get(&entries, pages[1].to_string()).unwrap().unwrap();
    doc.put(&page, "body", "corrupted body").unwrap();
    doc.get_heads();
    assert!(reader.sync(&doc).is_err());
    same_projection(reader.state(), &before);
    assert!(
        !reader.follows(&doc),
        "a refused projection cannot claim the new revision"
    );
}
