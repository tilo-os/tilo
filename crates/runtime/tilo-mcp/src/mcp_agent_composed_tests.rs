//! The providers a scenario composes: one whose directory never answers a search, and one
//! that answers only the catalogue an `ls` reads an app's names from.
use super::*;
use tilo_actions::remote::CatalogueRow;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};

/// [`Notes`] publishing everything it does, and no search of its own: a
/// provider that holds no records to search.
#[derive(Clone)]
pub(super) struct NoSearch(Notes);

impl NoSearch {
    pub(super) fn new(notes: &Notes) -> Self {
        Self(notes.clone())
    }
}

impl ActionProvider for NoSearch {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.0.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.0.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.0.observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.0.cancel(caller, id)
    }
}

/// What one call made the app's directory do: the queries it was asked and
/// the rows it answered.
pub(super) fn work_of(notes: &Notes, name: &str, arguments: &Value) -> (usize, usize, String) {
    notes.take_asked();
    notes.take_answered();
    let (text, failed) = run(tool(&agent(notes), name, arguments.clone()));
    assert!(!failed, "{name} {arguments}: {text}");
    (notes.take_asked().len(), notes.take_answered(), text)
}

/// The task side of a tilolang app, answering the one thing a listing asks of it:
/// the catalogue of what the apps declare. Its connection is a socket pair this
/// process holds at both ends, so a test pays for the protocol and not a subprocess.
pub(super) struct CatalogueHost {
    /// What it answers `{"catalogue": {}}` with.
    catalogue: Value,
}

impl CatalogueHost {
    pub(super) fn new(catalogue: Value) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self { catalogue })
    }
}

impl crate::TaskHosts for CatalogueHost {
    fn start(&self, _workspace: &str, _task: &str) -> std::io::Result<std::os::fd::OwnedFd> {
        let (api, answering) = std::os::unix::net::UnixStream::pair()?;
        let catalogue = self.catalogue.clone();
        std::thread::spawn(move || {
            use std::io::{BufRead as _, Write as _};
            let mut writer = answering.try_clone().expect("the other end of the pair");
            let reader = std::io::BufReader::new(answering);
            for line in reader.lines() {
                let Ok(line) = line else { return };
                let Ok(asked) = serde_json::from_str::<Value>(&line) else {
                    return;
                };
                let answer = if asked.get("catalogue").is_some() {
                    json!({"catalogue": catalogue.clone()})
                } else {
                    json!({})
                };
                if writeln!(writer, "{answer}").is_err() {
                    return;
                }
            }
        });
        Ok(std::os::fd::OwnedFd::from(api))
    }
}

/// The catalogue of a tilolang app: its root's record type, by the name a program types it
/// with (`notes.Note`), and how that app's actions are called.
pub(super) fn tilolang_notes_catalogue() -> Value {
    json!({
        "notes": {
            "app": "so.tilo.lang.notes",
            "signatures": [
                {"name": "Note", "id": "tl.so.tilo.lang.notes.Note", "kind": "type",
                 "signature": "type notes.Note", "call": "notes.Note"},
                {"name": "Notebook", "id": "tl.so.tilo.lang.notes.Notebook", "kind": "type",
                 "signature": "type notes.Notebook", "call": "notes.Notebook"},
                {"name": "rename", "id": "rename_note", "kind": "action",
                 "signature": "action notes.rename(n: Note, title: Text)",
                 "call": "notes.rename(n: notes.Note, title: Text)", "on": "Note"},
            ],
        },
    })
}

/// The directory of a live Notes with the `Todo` page published under a kind of a
/// tilolang app's (`tl.so.tilo.lang.notes.Note`): what a listing of it names by the program
/// form, and the one resource of that kind there, so the name means one.
pub(super) fn tilolang_kind(notes: &Notes) -> Notes {
    const NOTE: &str = "tl.so.tilo.lang.notes.Note";
    let live = notes.clone().live();
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
