//! The references the workspaces on this device hold to records of other workspaces, and the
//! bringing that puts one there.
//!
//! Bringing a record here is one write through the store of the workspace in front: a reference
//! ([`tilo_resource_store::reference`]) naming the record's workspace, app and id. No bytes move
//! between the two workspaces and no id is minted. The reference grants nothing: the record
//! opens where it lives, for whoever may read it there, and a member who may not finds the
//! reference unavailable. Moving a record is the store's `move`.
//!
//! The references are read away from the session's loop, and again whenever the change stream
//! says a record of this device moved; search takes what was last read.

use std::sync::Arc;

use tilo_desktop_search::providers::references::{Opens, Reference};
use tilo_resource_store::reference::{self, Named};
use tilo_resource_store::wire::{Command, Reply};

use crate::platform::records::{Changes, Reading, Watch};

/// A command on the store of a workspace this device holds, run at the device's daemon as its
/// person: `(workspace, app, command)`.
type Store = dyn Fn(&str, &str, &Command) -> Result<Reply, String> + Send + Sync;

/// The app the shell keeps a workspace's references under: one for every app's records, since
/// the store places every reference of a workspace together.
pub(crate) const REFERENCES_APP: &str = tilo_desktop_model::ownership::DESKTOP_APP;

/// Why a record was not brought here.
#[derive(Debug)]
pub enum Bring {
    /// The person may not read it where it lives, or this device holds nothing of it.
    Unreadable,
    /// The workspace refused the reference, saying why.
    Refused(String),
}

/// A stamp of one invocation of the verb `verb`.
pub(crate) fn stamp(verb: &str) -> tilo_resource_store::Stamp {
    tilo_resource_store::Stamp {
        verb: verb.to_owned(),
        invocation: uuid::Uuid::new_v4().to_string(),
        ..tilo_resource_store::Stamp::default()
    }
}

/// The references the workspaces on this device hold, each by the workspace holding it.
pub struct References {
    changes: Changes,
    store: Arc<Store>,
    read: Reading<Vec<(String, Reference)>>,
}

impl References {
    /// The references the workspaces `reads` holds hold, read through the stores at `daemon` as
    /// the device's person, again whenever `changes` says a record moved.
    #[must_use]
    pub fn over(
        reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        daemon: tilo_session_client::daemon_documents::DaemonShell,
        changes: Changes,
    ) -> Self {
        let store = store_at(daemon);
        let reading = Arc::clone(&store);
        let read = Reading::start(
            "tilo-search-references",
            changes.clone(),
            Watch::Any,
            move || read_references(&reads.workspaces(), reading.as_ref()),
            || {},
        );
        Self {
            changes,
            store,
            read,
        }
    }

    /// Bring the record `record` that the app `app` keeps in the workspace `from` into the
    /// workspace `here`: a reference to it there, put through that workspace's store under the
    /// verb that brings, as the device's person. The record is named by the kind its app keeps
    /// it as, which only someone who may read it there learns.
    ///
    /// # Errors
    /// [`Bring::Unreadable`] where the person may not read the record where it lives, else
    /// [`Bring::Refused`] with why the workspace refused the reference.
    pub fn bring(&self, here: &str, from: &str, app: &str, record: &str) -> Result<(), Bring> {
        let kind = collab_types::app_surface::Surfaces::process()
            .kinds_of(app)
            .into_iter()
            .find(|kind| {
                matches!(
                    (self.store)(
                        from,
                        app,
                        &Command::Get {
                            kind: kind.clone(),
                            id: record.to_owned(),
                        },
                    ),
                    Ok(Reply::Resource(Some(_)))
                )
            })
            .ok_or(Bring::Unreadable)?;
        let named = Named {
            workspace: from.to_owned(),
            app: app.to_owned(),
            record: record.to_owned(),
            kind,
        };
        self.write(
            here,
            &named.id(),
            Command::Put {
                kind: reference::KIND.to_owned(),
                id: named.id(),
                resource: tilo_resource_store::Resource {
                    readers: collab_types::EVERY_MEMBER.to_owned(),
                    parent: None,
                    order: None,
                    fields: named.fields(),
                },
                stamp: stamp(reference::BRING),
            },
        )
        .map_err(Bring::Refused)
    }

    /// Take the reference `id` out of the workspace `here` again, under the verb that removes
    /// one: the record it names stays as it is where it lives.
    ///
    /// # Errors
    /// The workspace refused the removal.
    pub fn remove(&self, here: &str, id: &str) -> Result<(), String> {
        self.write(
            here,
            id,
            Command::Remove {
                kind: reference::KIND.to_owned(),
                id: id.to_owned(),
                stamp: stamp(reference::REMOVE),
            },
        )
    }

    /// Run `command` on the reference `id` of the workspace `here`, and tell the roles following
    /// the records of the write the workspace took.
    fn write(&self, here: &str, id: &str, command: Command) -> Result<(), String> {
        (self.store)(here, REFERENCES_APP, &command)?;
        self.changes.note(here, REFERENCES_APP, id, false);
        Ok(())
    }

    /// The rows search shows of the references `workspace` holds, as last read, each saying the
    /// workspace its record lives in by its name in `names` (id, name).
    #[must_use]
    pub fn rows(&self, workspace: &str, names: &[(String, String)]) -> Vec<Reference> {
        self.read
            .get()
            .iter()
            .filter(|(here, _)| here == workspace)
            .map(|(_, row)| {
                let mut row = row.clone();
                if let Some(opens) = &mut row.opens {
                    opens.from = names
                        .iter()
                        .find(|(id, _)| *id == opens.workspace)
                        .map(|(_, name)| name.clone());
                }
                row
            })
            .collect()
    }
}

/// The stores of the workspaces the daemon `daemon` holds, asked on the session's link to it.
fn store_at(daemon: tilo_session_client::daemon_documents::DaemonShell) -> Arc<Store> {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    Arc::new(move |workspace, app, command| {
        let command = serde_json::to_string(command).map_err(|error| error.to_string())?;
        match daemon.ask_blocking(ShellCall::Resources {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            command,
        })? {
            ShellReply::Resources(reply) => serde_json::from_str(&reply?)
                .map_err(|error| format!("the store's answer does not read: {error}")),
            other => Err(format!(
                "the daemon answered a resource command with {other:?}"
            )),
        }
    })
}

/// Every reference the workspaces `held` hold, each with what the person may read of its record.
fn read_references(held: &[String], store: &Store) -> Vec<(String, Reference)> {
    let mut rows = Vec::new();
    for workspace in held {
        let mut query = tilo_resource_store::Query::of(reference::KIND, 200);
        loop {
            let page = match store(workspace, REFERENCES_APP, &Command::Query(query.clone())) {
                Ok(Reply::Page(page)) => page,
                Ok(other) => {
                    tracing::warn!(?other, workspace, "a workspace's references are not read");
                    break;
                }
                Err(error) => {
                    tracing::warn!(%error, workspace, "a workspace's references are not read");
                    break;
                }
            };
            for (id, indexed) in page.resources {
                if let Some(named) = Named::of(&indexed.fields) {
                    rows.push((
                        workspace.clone(),
                        Reference {
                            id,
                            opens: opens(named, store),
                        },
                    ));
                }
            }
            match page.next {
                Some(next) => query.after = Some(next),
                None => break,
            }
        }
    }
    rows
}

/// What a reference to `named` opens, where the person may read the record where it lives, as
/// that workspace's store decides for them, called by its name, else its kind; nothing where
/// they may not, or this device holds nothing of that workspace.
fn opens(named: Named, store: &Store) -> Option<Opens> {
    let read = store(
        &named.workspace,
        &named.app,
        &Command::Get {
            kind: named.kind.clone(),
            id: named.record.clone(),
        },
    );
    let Ok(Reply::Resource(Some(record))) = read else {
        return None;
    };
    Some(Opens {
        title: ["title", "name"]
            .iter()
            .find_map(|field| match record.scalar(field) {
                tilo_resource_store::Value::Text(name) if !name.trim().is_empty() => Some(name),
                _ => None,
            })
            .unwrap_or_else(|| named.kind.clone()),
        workspace: named.workspace,
        app: named.app,
        record: named.record,
        from: None,
    })
}

/// What a test brings: a note of a Rust app's records in a workspace the daemon holds, written as
/// the app's window writes it.
#[cfg(test)]
pub(crate) mod testing {
    use tilo_resource_store::reference::Named;
    use tilo_resource_store::wire::{Command, Reply};
    use tilo_resource_store::{Cell, Value};

    use crate::daemon_fixture::TestDaemon;

    pub(crate) use super::stamp;

    pub(crate) const NOTES: &str = "so.tilo.notes";

    /// What Notes' package declares: one kind of record, a note with a title.
    const NOTES_TYPES: &[u8] = b"{\"format\":2,\"resources\":[{\"fields\":[{\"path\":\"title\",\"type\":\"text\"}],\"kind\":\"notes.note\",\"label\":\"Note\",\"verbs\":[{\"id\":\"add\",\"kind\":\"undoable\"}],\"version\":1}]}";

    /// Run `command` on `app`'s records of `workspace` as the device's person, as a window of the
    /// app does.
    pub(crate) fn command(
        daemon: &TestDaemon,
        workspace: &str,
        app: &str,
        command: Command,
    ) -> Result<Reply, String> {
        let workspaces = daemon.workspaces().clone();
        let (workspace, app) = (workspace.to_owned(), app.to_owned());
        tilo_rt::app()
            .block_on(async move {
                workspaces
                    .own_resource_command(&workspace, &app, command, Vec::new())
                    .await
            })
            .expect("the runtime runs")
    }

    /// A note of `workspace` called "Groceries", which Notes, installed on the device, keeps in
    /// the workspace's store: the device's daemon and the session both know the types it
    /// declares, as installing it names them to each.
    pub(crate) fn groceries(daemon: &TestDaemon, workspace: &str) -> Named {
        daemon
            .workspaces()
            .declare_package_surface(NOTES, NOTES_TYPES);
        tilo_workspace_service::apps::register_package_surface(NOTES, NOTES_TYPES);
        command(
            daemon,
            workspace,
            NOTES,
            Command::Put {
                kind: "notes.note".to_owned(),
                id: "groceries".to_owned(),
                resource: tilo_resource_store::Resource {
                    readers: collab_types::EVERY_MEMBER.to_owned(),
                    parent: None,
                    order: None,
                    fields: [(
                        "title".to_owned(),
                        Cell::One(Value::Text("Groceries".to_owned())),
                    )]
                    .into(),
                },
                stamp: stamp("add"),
            },
        )
        .expect("the workspace keeps the note");
        Named {
            workspace: workspace.to_owned(),
            app: NOTES.to_owned(),
            record: "groceries".to_owned(),
            kind: "notes.note".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use tilo_resource_store::reference;
    use tilo_resource_store::wire::{Command, Reply};

    use super::testing::{NOTES, command, groceries, stamp};
    use super::{Bring, REFERENCES_APP, References};
    use crate::daemon_fixture::TestDaemon;

    const KITCHEN: &str = "6d1c1f36-0000-4000-8000-000000000001";
    const STUDIO: &str = "6d1c1f36-0000-4000-8000-000000000002";

    /// A record of a Rust app brought into another workspace is a reference there, named as the
    /// record is; taking the reference out again leaves the record as it was where it lives.
    #[test]
    fn a_brought_record_is_a_reference_whose_removal_leaves_the_record() {
        let daemon = TestDaemon::writing(&[KITCHEN, STUDIO]);
        let note = groceries(&daemon, KITCHEN);
        let references = References::over(daemon.reads(), daemon.shell(), daemon.changes());
        let names = [
            (KITCHEN.to_owned(), "Kitchen".to_owned()),
            (STUDIO.to_owned(), "Studio".to_owned()),
        ];

        assert!(
            matches!(
                references.bring(STUDIO, KITCHEN, NOTES, "plans"),
                Err(Bring::Unreadable)
            ),
            "a record no one may read there is not brought"
        );
        references
            .bring(STUDIO, KITCHEN, NOTES, &note.record)
            .expect("brought here");
        tilo_test_env::wait_until("the reference is read", || {
            !references.rows(STUDIO, &names).is_empty()
        });
        let rows = references.rows(STUDIO, &names);
        let [row] = rows.as_slice() else {
            panic!("one reference: {rows:?}");
        };
        assert_eq!(row.id, note.id());
        let opens = row.opens.as_ref().expect("the person may read the note");
        assert_eq!(opens.title, "Groceries", "named as the record is");
        assert_eq!(
            (opens.workspace.as_str(), opens.from.as_deref()),
            (KITCHEN, Some("Kitchen"))
        );
        assert!(
            references.rows(KITCHEN, &names).is_empty(),
            "Kitchen holds the record itself"
        );

        // Only the verb that brings makes one: an app's own verb does not.
        let mut elsewhere = note.clone();
        elsewhere.record = "plans".to_owned();
        let unbrought = command(
            &daemon,
            STUDIO,
            REFERENCES_APP,
            Command::Put {
                kind: reference::KIND.to_owned(),
                id: elsewhere.id(),
                resource: tilo_resource_store::Resource {
                    readers: collab_types::EVERY_MEMBER.to_owned(),
                    parent: None,
                    order: None,
                    fields: elsewhere.fields(),
                },
                stamp: stamp("add"),
            },
        );
        assert!(unbrought.is_err(), "{unbrought:?}");

        // Taken out again under the verb that removes one.
        references
            .remove(STUDIO, &note.id())
            .expect("the reference is taken out");
        tilo_test_env::wait_until("the removal is read", || {
            references.rows(STUDIO, &names).is_empty()
        });
        let kept = command(
            &daemon,
            KITCHEN,
            NOTES,
            Command::Get {
                kind: note.kind.clone(),
                id: note.record.clone(),
            },
        );
        let Ok(Reply::Resource(Some(kept))) = kept else {
            panic!("the note stays in Kitchen: {kept:?}");
        };
        assert_eq!(
            kept.scalar("title"),
            tilo_resource_store::Value::Text("Groceries".to_owned()),
            "as it was"
        );
    }
}
