use super::*;

/// The catalogue of a tilolang app: its root's record type, by the name a program types it
/// with (`notes.Note`), and how that app's actions are called.
pub(super) fn tilolang_notes_catalogue() -> Value {
    json!({
        "notes": {
            "app": "so.tilo.notes",
            "signatures": [
                {"name": "Note", "id": "tl.so.tilo.notes.Note", "kind": "type",
                 "signature": "type notes.Note", "call": "notes.Note"},
                {"name": "Notebook", "id": "tl.so.tilo.notes.Notebook", "kind": "type",
                 "signature": "type notes.Notebook", "call": "notes.Notebook"},
                {"name": "rename", "id": "rename_note", "kind": "action",
                 "signature": "action notes.rename(n: Note, title: Text)",
                 "call": "notes.rename(n: notes.Note, title: Text)", "on": "Note"},
            ],
        },
    })
}

/// The directory of a live Notes (`host`) with the `Todo` page published under a kind of a
/// tilolang app's (`tl.so.tilo.notes.Note`): what a listing of it names by the program
/// form, and the one resource of that kind there, so the name means one.
pub(super) fn tilolang_kind(host: &Notes) -> Notes {
    const NOTE: &str = "tl.so.tilo.notes.Note";
    let live = host.clone().live();
    // The live shape names a page by the key its notebook gives it, bound per publication.
    let todo = live.id(&Notes::live_key(TODO));
    let published: Vec<CatalogueRow> = live
        .rows("1")
        .into_iter()
        .map(|mut row| {
            if row.resource.id == todo {
                row.resource.kind = NOTE.to_owned();
                if let Some(action) = row.action.as_mut() {
                    action.resource_kind = NOTE.to_owned();
                }
            }
            row
        })
        .collect();
    *live.published_kinds.lock().unwrap() = Some(published);
    live
}
