use super::App;
use crate::resource::{DocumentCheckout, DocumentId, ResourceSet, ResourceType, index};
use crate::{State, widget::Text};
use automerge::AutoCommit;

#[test]
fn launch_reads_the_existing_index_without_opening_its_records() {
    let entry = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("annotations"));
    let mut workspace_index = AutoCommit::new();
    assert!(index::reference(
        &mut workspace_index,
        &entry.id,
        &entry.kind
    ));

    let app = App::new(State::new(), Box::new(Text::new("PDF"))).with_resources(
        ResourceSet::in_memory().with_home("workspace-a"),
        workspace_index,
    );
    let records = app.records().expect("record-aware launch installs the set");
    assert_eq!(records.known_resources(), vec![entry]);
    assert!(records.open_resources().is_empty());
}
