//! A document's view shown as a web page by its app worker: the page a person's
//! browser renders, and what they do on it acted on the view as a click or
//! typing in a window would be, its changes handed back to be saved.

use crate::common::{WORKSPACE, catalogue};
use tilo_actions::MemberId;
use tilo_agent_session::{ConfinedReplica, PageAction, ReplicaActor, Seed};

/// The number of the first element whose opening tag carries all of `marks`.
fn element(html: &str, marks: &[&str]) -> Option<u32> {
    html.split('<').find_map(|tag| {
        let open = tag.split('>').next()?;
        if !marks.iter().all(|mark| open.contains(mark)) {
            return None;
        }
        let number = open.split("data-p=\"").nth(1)?.split('"').next()?;
        number.parse().ok()
    })
}

#[test]
fn a_person_writes_a_note_on_the_notebooks_page() {
    let notebook = ConfinedReplica::open_page(
        &catalogue(),
        ("so.tilo.notes", WORKSPACE),
        &MemberId::person("ada"),
        &ReplicaActor::Owner,
        Seed::New,
    )
    .expect("the notebook opens as a page");
    let _ = notebook.take_mutation();
    // A new workspace holds none of what the notebook keeps beside it, as its
    // serving devices answer a page's worker.
    let _ = notebook.page_show(None);
    for wanted in notebook.wanted_resources() {
        notebook
            .workspace_answered(&wanted, ui::resource::WorkspaceAnswer::HoldsNone)
            .expect("told");
    }
    let _ = notebook.take_document_mutations();

    let shown = std::time::Instant::now();
    let (title, css, body) = notebook.page_show(None).expect("the page shows");
    println!("page shown in {:?}, {} bytes", shown.elapsed(), body.len());
    assert_eq!(title, "Notes");
    assert!(css.contains("--tilo-ground"), "the theme's colours: {css}");
    assert!(
        body.starts_with("<div data-p=\"0\""),
        "the page's body: {body}"
    );

    let new_note = element(&body, &["data-a=\"activate\"", "aria-label=\"New note\""])
        .unwrap_or_else(|| panic!("the page offers a new note: {body}"));
    let acted = std::time::Instant::now();
    let patches = notebook
        .page_act(Some((new_note, None, PageAction::Activate)))
        .expect("the person's click is acted on");
    println!("click answered in {:?}", acted.elapsed());
    assert!(!patches.is_empty(), "a new note changes the page");
    let field = patches
        .iter()
        .find_map(|(_, html)| element(html, &["data-w=\"TextInput\"", "data-a=\"value\""]))
        .unwrap_or_else(|| panic!("the new note's title is a field: {patches:?}"));

    let typed = std::time::Instant::now();
    let patches = notebook
        .page_act(Some((field, None, PageAction::Value("Groceries".into()))))
        .expect("the person's typing is acted on");
    println!("typing answered in {:?}", typed.elapsed());
    assert!(
        patches.iter().any(|(_, html)| html.contains("Groceries")),
        "the note's row says what was typed: {patches:?}"
    );
    let wrote = notebook.take_mutation().expect("the notebook's changes");
    let wrote_beside = notebook
        .take_document_mutations()
        .expect("the changes to its notes");
    assert!(
        wrote.is_some() || !wrote_beside.is_empty(),
        "what was typed is handed back to be saved"
    );

    // Nothing moved since: the page answers no change.
    assert!(
        notebook.page_act(None).expect("asked again").is_empty(),
        "an unchanged page sends nothing"
    );
}
