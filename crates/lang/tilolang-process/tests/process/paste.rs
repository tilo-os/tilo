//! A paragraph pasted past a mebibyte into a note: the note keeps it whole, the workspace's store
//! holds it, and one undo takes it back to what was typed.

use super::*;

/// The body of the note `target` names, as the instance's runtime reads it.
fn body_of(host: &TilolangProcess, target: &patch::ResourceRef) -> String {
    let instance = host.instances().next().unwrap().instance();
    let resource = tilolang_tilo::instance::wire::runtime_resource(target).unwrap();
    match instance
        .host()
        .unwrap()
        .runtime()
        .read(&resource, "body")
        .unwrap()
    {
        Value::Rich(body) => body.written_text(),
        other => panic!("a note's body is {other:?}"),
    }
}

#[test]
fn one_paragraph_pasted_past_a_mebibyte_is_kept_and_undoes() {
    let (mut host, mut window, sent, _inbox, _route) = running_app("ada", NOTES, notes());
    settle(&mut host, &mut window);
    press_new_note(&mut window);
    settle(&mut host, &mut window);
    let field = node_with(
        window.root.as_ref().expect("the notebook is shown"),
        "placeholder",
        "Write something",
    )
    .expect("the open note has a body")
    .key
    .clone();
    let note = window
        .in_place
        .clone()
        .expect("the press showed the note beside the list");
    let edit = |rich| ToApp::EditRich {
        level: Level::Root,
        target: note.clone(),
        key: field.clone(),
        rich,
        gesture: None,
    };
    window.send(&[edit(typed_rich("draft"))]);
    settle(&mut host, &mut window);
    assert_eq!(body_of(&host, &note), "draft", "what Ada typed shows");

    // Ada pastes one paragraph past a mebibyte over it.
    let pasted = format!("Pasted: {}", "words ".repeat(190_000));
    let before = writes_sent(&sent);
    window.send(&[edit(typed_rich(&pasted))]);
    settle(&mut host, &mut window);
    assert!(
        window.notices.is_empty(),
        "the paste is kept: {:?}",
        window.notices
    );
    assert!(pasted.len() > 1 << 20);
    assert_eq!(
        body_of(&host, &note),
        pasted,
        "the note keeps the paste whole"
    );
    assert!(
        writes_sent(&sent) > before,
        "the paste went to the workspace's store"
    );

    // One undo takes the paste back to what she typed.
    window.send(&[ToApp::Undo]);
    settle(&mut host, &mut window);
    assert_eq!(body_of(&host, &note), "draft", "undo takes the paste back");
}
