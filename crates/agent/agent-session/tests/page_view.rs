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

/// The page's row saying `words`: the number of the element a press lands on (`page.mjs` finds
/// the nearest element carrying `data-a` over the words), the target it carries and its tag.
fn row_saying(html: &str, words: &str) -> Option<(u32, Option<String>, String)> {
    let at = html.find(&format!(">{words}<"))?;
    let before = html.get(..at)?;
    let start = before.rfind("<li")?;
    let open = before.get(start..)?.split('>').next()?;
    let number = open.split("data-p=\"").nth(1)?.split('"').next()?;
    let target = open
        .split("data-t=\"")
        .nth(1)
        .map(|rest| rest.split('"').next().unwrap_or_default().to_owned());
    Some((number.parse().ok()?, target, open.to_owned()))
}

/// A person in a browser opens Settings as a page and presses a category in the sidebar: that
/// category is the chosen one and its pane stands beside it, as a press in a window chooses it.
/// The category is one below the window's own height, which a page shows all the same.
#[test]
fn a_person_chooses_a_settings_category_on_the_page() {
    let label = settings::categories::Category::ExternalAccounts.label();
    let settings = ConfinedReplica::open_page(
        &catalogue(),
        ("so.tilo.settings", WORKSPACE),
        &MemberId::person("ada"),
        &ReplicaActor::Owner,
        Seed::New,
    )
    .expect("settings opens as a page");
    let _ = settings.take_mutation();
    let _ = settings.page_show(None);
    // A new workspace holds none of what the settings page keeps beside it, as its serving
    // devices answer a page's worker.
    for wanted in settings.wanted_resources() {
        settings
            .workspace_answered(&wanted, ui::resource::WorkspaceAnswer::HoldsNone)
            .expect("told");
    }
    let _ = settings.take_document_mutations();
    let (_, _, body) = settings.page_show(None).expect("the page shows");
    let (row, target, tag) = row_saying(&body, &label)
        .unwrap_or_else(|| panic!("the page's sidebar offers {label}: {body}"));
    assert!(
        tag.contains("data-a=\"activate\""),
        "a press on the row activates it: {tag}"
    );

    let patches = settings
        .page_act(Some((row, target, PageAction::Activate)))
        .expect("the person's press is acted on");
    let (_, _, after) = settings.page_show(None).expect("the page shows again");
    let (_, _, chosen) = row_saying(&after, &label).expect("the row stays");
    assert!(
        chosen.contains("aria-selected=\"true\""),
        "the pressed category is the chosen one: {chosen}; the page answered {patches:?}"
    );
}
