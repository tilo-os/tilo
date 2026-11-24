use std::rc::Rc;

use super::*;
use std::path::Path;

use automerge::{ObjType, ROOT, ReadDoc};

use crate::resource::{
    children_resource_id, commit_genesis, genesis, genesis_actor_of, home, identity, init,
};

const APP: &str = "so.tilo.test-documents";

fn workspace_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn put_text(doc: &mut AutoCommit, key: &str, value: &str) {
    doc.put(ROOT, key, value).unwrap();
}

fn text(doc: &AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|scalar| scalar.into_string().ok())
}

tilo_test_env::scenario! {
    fn record_ids_name_their_parts_children_and_homes;

    phase a_chain_part_and_an_on_demand_record_are_record_ids;
    // A chain's parts and an on-demand record are ordinary record ids, in
    // the wire form the host reads them by.
    {
        let chat = DocumentId::workspace_default();
        let part = chat.part(12);
        assert_eq!(part.as_str(), format!("derived:part.12@{}", chat.as_str()));
        assert_eq!(DocumentId::parse(part.as_str()), Some(part.clone()));
        assert_eq!(part.chain_part(), Some((chat.clone(), 12)));
        assert_eq!(chat.part(0).chain_part(), Some((chat.clone(), 0)));
        assert_eq!(chat.chain_part(), None);
        let archive = DocumentId::minted().on_demand();
        assert!(archive.as_str().starts_with("derived:on-demand@uuid:"));
        assert_eq!(DocumentId::parse(archive.as_str()), Some(archive));
    }

    phase a_child_record_names_its_parent_and_its_own_id;
    // A child record is an ordinary record id that names its parent and
    // its own id, whatever lists it; an id that cannot name a record is
    // refused.
    {
        let chat = DocumentId::workspace_default();
        let channel = chat.child("c_design").unwrap();
        assert_eq!(
            channel.as_str(),
            format!("derived:child.c_design@{}", chat.as_str())
        );
        assert_eq!(DocumentId::parse(channel.as_str()), Some(channel.clone()));
        assert_eq!(
            channel.child_of(),
            Some((chat.clone(), "c_design".to_owned()))
        );
        assert_eq!(chat.child_of(), None);
        assert_eq!(chat.part(1).child_of(), None);
        for refused in ["", "has space", "a/b", &"x".repeat(80)] {
            assert_eq!(chat.child(refused), None, "{refused:?}");
        }
        assert_eq!(
            children_resource_id(&chat, "chat.channel"),
            format!("children:chat.channel@{}", chat.as_str())
        );
    }

    phase record_identities_keys_and_homes_are_stable_and_distinct;
    {
        {
            #[cfg(windows)]
            let (root, expected) = ("C:/work", r"file:C:\work\report.odt");
            #[cfg(not(windows))]
            let (root, expected) = ("/work", "file:/work/report.odt");
            let path = Path::new(root).join("report.odt");
            let id = DocumentId::for_file(&path);
            assert_eq!(id.as_str(), expected);
            assert_eq!(id.file_path().unwrap(), path);
            assert_eq!(DocumentId::parse(expected), Some(id.clone()));
            // Same file, same record, including lexical path aliases.
            assert_eq!(id, DocumentId::for_file(Path::new(root).join("./report.odt")));
            assert_eq!(
                id,
                DocumentId::for_file(Path::new(root).join("drafts/../report.odt"))
            );
        }

        {
            assert_ne!(DocumentId::minted(), DocumentId::minted());
        }

        {
            let a = DocumentId::for_file("/work/report.odt");
            let b = DocumentId::for_file("/work/other.odt");
            assert_eq!(
                a.storage_key(),
                DocumentId::for_file("/work/report.odt").storage_key()
            );
            assert_ne!(a.storage_key(), b.storage_key());
            // A traversal in the identity must not survive into the key.
            let key = DocumentId::for_file("/work/../../etc/shadow").storage_key();
            assert!(!key.contains('/'), "key {key} must name one directory");
            assert!(!key.contains(".."), "key {key} must not traverse");

            // Paths that agree past the readable part of the key are still
            // two documents — a sanitise-only key would collide them.
            let deep_a =
                DocumentId::for_file("/work/clients/northwind/2026/contracts/final/appendix-a.odt");
            let deep_b =
                DocumentId::for_file("/work/clients/northwind/2026/contracts/final/appendix-b.odt");
            assert_ne!(
                deep_a.storage_key(),
                deep_b.storage_key(),
                "two long paths sharing a prefix must not share a file"
            );
        }

        {
            let mut doc = AutoCommit::new();
            let id = DocumentId::for_file("/work/report.odt");
            let kind = ResourceType::new("document");
            init(&mut doc, &id, &kind, "workspace-1");
            let got = identity(&doc).expect("the document knows which record it is");
            assert_eq!(got.id, id);
            assert_eq!(got.kind, kind);
            assert_eq!(home(&doc).as_deref(), Some("workspace-1"));
        }

        {
            let mut doc = AutoCommit::new();
            let id = DocumentId::minted();
            let kind = ResourceType::new("book");
            init(&mut doc, &id, &kind, "workspace-1");
            init(&mut doc, &id, &kind, "workspace-2");
            assert_eq!(
                home(&doc).as_deref(),
                Some("workspace-1"),
                "a record has one home and any number of references"
            );
        }

        {
            let file = DocumentId::for_file("/tmp/shared/report.pdf");
            let derived = DocumentId::derived("annotations", &file);
            assert!(DocumentId::parse(derived.as_str()).is_some(), "{derived}");
        }

    }
}

tilo_test_env::scenario! {
    fn creators_of_one_record_converge_on_one_genesis;

    phase two_creators_of_one_record_make_one_identical_genesis;
    // Two devices creating the same record make byte for byte the same
    // single genesis change, under the record's own actor at time zero,
    // and converge on one record.
    {
        let kind = ResourceType::new("test.genesis-note");
        // The app's surface is named in the record's identity.
        collab_types::app_surface::register_package(
            "so.tilo.test-genesis",
            "{\"resources\":[{\"kind\":\"test.genesis-note\",\"label\":\"Note\",\"version\":1}]}"
                .as_bytes(),
        );
        collab_types::app_surface::register_kind(kind.as_str(), "so.tilo.test-genesis");
        let checkout = DocumentCheckout::new(DocumentId::for_tab(3), kind);
        let create = |actor: &[u8]| {
            let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(actor));
            genesis(&mut doc, &checkout, "workspace", |doc| {
                doc.put(automerge::ROOT, "title", "Untitled")
                    .map(drop)
                    .map_err(|error| error.to_string())
            })
            .unwrap();
            // What a store commits after the seed adds nothing to it.
            commit_genesis(&mut doc);
            doc
        };
        let mut one = create(b"device-one");
        let mut two = create(b"device-two");
        let changes = one.get_changes(&[]);
        assert_eq!(changes.len(), 1, "genesis is one change");
        let genesis = &changes[0];
        assert_eq!(
            genesis.actor_id(),
            &genesis_actor_of(&checkout.id, &checkout.kind, "workspace")
        );
        assert_eq!(genesis.timestamp(), 0);
        assert_eq!(
            one.get_changes(&[])
                .iter()
                .map(|change| change.hash())
                .collect::<Vec<_>>(),
            two.get_changes(&[])
                .iter()
                .map(|change| change.hash())
                .collect::<Vec<_>>(),
            "both devices make the identical genesis"
        );
        one.merge(&mut two).unwrap();
        assert_eq!(one.get_changes(&[]).len(), 1, "they converge on one record");
    }

    phase members_creating_the_same_record_converge_on_one;
    // Two members of a workspace each create the record the workspace derives for
    // them, before either has heard of the other's: the same record, from
    // the same initial content. Their documents merge into one record whose
    // content has one value at every key, not two competing roots.
    {
        let initialize = |doc: &mut AutoCommit| {
            for key in ["pages", "tags"] {
                doc.put_object(ROOT, key, ObjType::Map)
                    .map_err(|e| e.to_string())?;
            }
            doc.put(ROOT, "schema_version", 1_u64)
                .map_err(|e| e.to_string())?;
            Ok(())
        };
        let checkout = DocumentCheckout::new(
            DocumentId::derived("default-notebook", &DocumentId::for_tab(7)),
            ResourceType::new("notebook"),
        );

        let created: Vec<Vec<u8>> = ["ada", "bea"]
            .into_iter()
            .map(|_member| {
                let mut owners = ResourceOwners::in_memory();
                let store = owners.open("so.tilo.test", "workspace-a").unwrap();
                store
                    .reserve_resource(&checkout)
                    .unwrap()
                    .initialize(initialize)
                    .unwrap();
                store.read(&checkout.id, |doc| doc.clone().save()).unwrap()
            })
            .collect();
        let mut merged = AutoCommit::load(&created[0]).unwrap();
        let mut other = AutoCommit::load(&created[1]).unwrap();
        tilo_automerge::ApplyIncoming::merge_incoming(&mut merged, &mut other).unwrap();
        for key in ["pages", "tags", "schema_version"] {
            assert_eq!(
                merged.get_all(ROOT, key).unwrap().len(),
                1,
                "{key} has one value once both members' records meet"
            );
        }
        assert_eq!(
            identity(&merged).map(|found| found.id),
            Some(checkout.id.clone()),
            "the record keeps its one identity"
        );
        // Each member still writes as itself after the shared creation.
        let actors: std::collections::BTreeSet<_> = [&created[0], &created[1]]
            .iter()
            .map(|bytes| AutoCommit::load(bytes).unwrap().get_actor().clone())
            .collect();
        assert_eq!(actors.len(), 2, "the members keep their own actors");
    }
}

/// The workspace in this process the tests stand on answers as the workspace's one store does:
/// the scenarios the workspace service passes over an app's connection.
#[test]
fn the_memory_workspace_answers_as_the_workspace_service_does() {
    let port: Rc<dyn DocumentPort> = Rc::new(MemoryWorkspace::default());
    let (one, two) = (workspace_id(), workspace_id());
    crate::resource::conformance::run(&mut |_| port.clone(), APP, [&one, &two]);
}

tilo_test_env::scenario! {
    fn a_record_lives_in_its_workspace_not_in_the_process;

    phase a_refused_write_is_said_and_nothing_claims_it_saved;
    {
        let port = Rc::new(MemoryWorkspace::default());
        let workspace = workspace_id();
        let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        let mut owners = ResourceOwners::writing(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        store.create_resource(&note).unwrap();
        store.flush().unwrap();
        port.refuse.set(true);
        store.with(&note.id, |doc| {
            put_text(doc, "body", "refused");
            doc.commit();
        });
        let error = store.flush().expect_err("the workspace refused the write");
        assert!(error.contains("the workspace refused it"), "{error}");
        assert_eq!(
            store.save_status(&note.id).unwrap(),
            ResourceSaveStatus::Failed("the workspace refused it".into())
        );
        port.refuse.set(false);
        store.flush().expect("the next write is taken");
        assert_eq!(store.save_status(&note.id).unwrap(), ResourceSaveStatus::Saved);
    }

    phase an_edit_still_being_typed_is_not_saved;
    // Saved means the workspace holds everything the record holds, its open typing transaction
    // included: a save indicator and a receipt for what the workspace took both trust it.
    {
        let port = Rc::new(MemoryWorkspace::default());
        let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        let mut owners = ResourceOwners::writing(port);
        let store = owners.open(APP, &workspace_id()).unwrap();
        store.create_resource(&note).unwrap();
        store.flush().unwrap();
        assert_eq!(store.save_status(&note.id).unwrap(), ResourceSaveStatus::Saved);
        store.with(&note.id, |doc| put_text(doc, "body", "typing"));
        assert_eq!(
            store.save_status(&note.id).unwrap(),
            ResourceSaveStatus::Saving,
            "an edit not yet committed is not the workspace's"
        );
        store.flush().unwrap();
        assert_eq!(store.save_status(&note.id).unwrap(), ResourceSaveStatus::Saved);
    }

    phase a_store_without_a_port_keeps_its_records_in_memory;
    {
        let store = ResourceOwners::in_memory()
            .open(APP, &workspace_id())
            .unwrap();
        let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        store.create_resource(&note).unwrap();
        assert_eq!(
            store.save_status(&note.id).unwrap(),
            ResourceSaveStatus::MemoryOnly
        );
        store.flush().unwrap();
    }

    phase a_windows_record_is_saved_once_the_workspace_took_what_the_window_published;
    // A window's routes publish what its record changes, and the window tells the store what the
    // workspace took: the record is saving until then, and the window showing it is woken once
    // the answer came. A refusal shows as a failure until the next answer takes it all.
    {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let port = Rc::new(MemoryWorkspace::default());
        let workspace = workspace_id();
        let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        {
            let mut owners = ResourceOwners::writing(port.clone());
            let store = owners.open(APP, &workspace).unwrap();
            store.create_resource(&note).unwrap();
            store.flush().unwrap();
        }
        let mut owners = ResourceOwners::reading(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        assert_eq!(
            store.try_open_resource(&note).unwrap(),
            ResourceOpenOutcome::Loaded
        );
        assert_eq!(
            store.save_status(&note.id).unwrap(),
            ResourceSaveStatus::Saved,
            "what the workspace holds is saved"
        );
        let edit = |body: &str| {
            store.with(&note.id, |doc| {
                put_text(doc, "body", body);
                doc.commit();
            });
            store.read(&note.id, AutoCommit::base_heads).unwrap()
        };

        let typed = edit("typed");
        assert_eq!(
            store.save_status(&note.id).unwrap(),
            ResourceSaveStatus::Saving
        );
        let woken = std::sync::Arc::new(AtomicUsize::new(0));
        let wake: ReadyWake = {
            let woken = std::sync::Arc::clone(&woken);
            std::sync::Arc::new(move || {
                woken.fetch_add(1, SeqCst);
            })
        };
        store.wake_on_save_progress(&note.id, std::sync::Arc::clone(&wake));
        assert_eq!(woken.load(SeqCst), 0, "nothing answered yet");
        store.note_published(&note.id, &typed);
        assert_eq!(woken.load(SeqCst), 1, "the answer wakes the window");
        assert_eq!(store.save_status(&note.id).unwrap(), ResourceSaveStatus::Saved);

        let refused = edit("refused");
        store.wake_on_save_progress(&note.id, std::sync::Arc::clone(&wake));
        store.note_unpublished(&note.id, "the workspace refused it".into());
        assert_eq!(woken.load(SeqCst), 2);
        assert_eq!(
            store.save_status(&note.id).unwrap(),
            ResourceSaveStatus::Failed("the workspace refused it".into())
        );
        store.note_published(&note.id, &refused);
        assert_eq!(store.save_status(&note.id).unwrap(), ResourceSaveStatus::Saved);
    }

    phase nothing_unchanged_is_written_again;
    {
        let port = Rc::new(MemoryWorkspace::default());
        let workspace = workspace_id();
        let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        let mut owners = ResourceOwners::writing(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        store.create_resource(&note).unwrap();
        store.flush().unwrap();
        let writes = port.writes.get();
        store.tick();
        store.flush().unwrap();
        assert_eq!(port.writes.get(), writes);
    }

    phase what_the_workspace_brought_is_not_written_back;
    {
        let port = Rc::new(MemoryWorkspace::default());
        let workspace = workspace_id();
        let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        let mut owners = ResourceOwners::writing(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        store.create_resource(&note).unwrap();
        store.flush().unwrap();
        // Another device's change reaches the workspace, and this store merges the workspace's
        // copy of the record.
        let theirs = |body: &str| {
            let saved = port.saved(APP, &workspace, note.id.route()).unwrap();
            let mut doc = AutoCommit::load(&saved).unwrap();
            put_text(&mut doc, "body", body);
            doc.commit();
            port.hold(APP, &workspace, note.id.route(), &doc.save()).unwrap();
            let held = port.saved(APP, &workspace, note.id.route()).unwrap();
            (held, doc.get_heads())
        };
        let (held, heads) = theirs("from another device");
        let writes = port.writes.get();
        assert!(store.merge_workspace_document(&note, held, &heads).unwrap());
        store.flush().unwrap();
        assert_eq!(port.writes.get(), writes, "nothing the workspace holds is sent back");
        assert_eq!(store.save_status(&note.id).unwrap(), ResourceSaveStatus::Saved);
        // A change of this store's made before the merge is still written, and alone.
        store.with(&note.id, |doc| {
            put_text(doc, "title", "mine");
            doc.commit();
        });
        let (held, heads) = theirs("again from another device");
        assert!(store.merge_workspace_document(&note, held, &heads).unwrap());
        store.flush().unwrap();
        assert_eq!(port.writes.get(), writes + 1, "this store's own change goes");
        assert_eq!(store.save_status(&note.id).unwrap(), ResourceSaveStatus::Saved);
        let saved = port.saved(APP, &workspace, note.id.route()).unwrap();
        let saved = AutoCommit::load(&saved).unwrap();
        assert_eq!(text(&saved, "title").as_deref(), Some("mine"));
        assert_eq!(text(&saved, "body").as_deref(), Some("again from another device"));
    }

    phase a_burst_of_edits_while_a_write_is_under_way_is_one_more_write;
    {
        let memory = Rc::new(MemoryWorkspace::default());
        let port = Rc::new(Gated(memory.clone(), Rc::default()));
        let workspace = workspace_id();
        let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        let mut owners = ResourceOwners::writing(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        store.create_resource(&note).unwrap();
        store.tick();
        let founding = memory.writes.get();
        for key in 0..50 {
            store.with(&note.id, |doc| {
                put_text(doc, "body", &format!("keystroke {key}"));
                doc.commit();
            });
            store.tick();
        }
        assert_eq!(memory.writes.get(), founding, "nothing more is sent while a write is under way");
        port.1.set(true);
        store.flush().unwrap();
        assert_eq!(memory.writes.get(), founding + 1, "the burst went as one write");
        let mut owners = ResourceOwners::reading(memory);
        let reread = owners.open(APP, &workspace).unwrap();
        reread.try_open_resource(&note).unwrap();
        assert_eq!(
            reread.with(&note.id, |doc| text(doc, "body")).flatten().as_deref(),
            Some("keystroke 49")
        );
    }
}

/// A workspace whose answers to writes wait until it is opened.
struct Gated(Rc<MemoryWorkspace>, Rc<Cell<bool>>);

impl DocumentPort for Gated {
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String> {
        self.0.read(app, workspace, reads)
    }

    fn write(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        changes: Vec<u8>,
        founding: bool,
        readers: Option<&str>,
    ) -> Written {
        let mut written = self.0.write(app, workspace, id, changes, founding, readers);
        let open = Rc::clone(&self.1);
        Box::new(move |wait| if open.get() { written(wait) } else { None })
    }
}

/// A part another device started reaches the workspace while this window already holds the
/// chain: it is a held part, and it opens.
#[test]
fn a_part_the_workspace_took_after_the_window_opened_is_held_and_opens() {
    let port = Rc::new(MemoryWorkspace::default());
    let workspace = workspace_id();
    let chain = DocumentId::derived("messages", &DocumentId::workspace_default());
    let part = DocumentCheckout::new(chain.part(0), ResourceType::new("test.chain-part"));

    let mut owners = ResourceOwners::reading(port.clone());
    let store = owners.open(APP, &workspace).unwrap();
    assert!(store.held_parts(&chain).is_empty());

    let mut elsewhere = ResourceOwners::writing(port.clone());
    let started = elsewhere.open(APP, &workspace).unwrap();
    started
        .reserve_resource(&part)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "messages", 1)
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    started.flush().unwrap();

    assert_eq!(store.held_parts(&chain), vec![0]);
    store.try_open_resource(&part).unwrap();
}

tilo_test_env::scenario! {
    fn primary_records_are_admitted_by_their_own_rule;

    phase package_primary_admission_uses_the_primary_identity_rule;
    {
        let store = ResourceOwners::in_memory()
            .open(APP, &workspace_id())
            .unwrap();
        let primary = DocumentCheckout::package_primary(ResourceType::new("primary"));
        store
            .reserve_resource(&primary)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        store.try_open_resource(&primary).unwrap();
        // Admission takes a write to the record the store opened, and no other.
        let admission = store.admission(&primary).unwrap();
        assert!(admission.matches(&primary));
        let minted = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("primary"));
        assert!(!admission.matches(&minted));
    }

    phase malformed_primaries_are_refused;
    {
        let store = ResourceStore::in_memory();
        let malformed = DocumentCheckout::package_primary(ResourceType::new("1invalid"));
        assert!(matches!(
            store.reserve_resource(&malformed),
            Err(ResourceOpenError::IdentityMismatch)
        ));
        let malformed = DocumentCheckout::package_primary(ResourceType::new("primary")).referencing(
            DocumentCheckout::new(DocumentId::for_tab(1), ResourceType::new("target")),
            "bad relation",
        );
        assert!(matches!(
            store.reserve_resource(&malformed),
            Err(ResourceOpenError::IdentityMismatch)
        ));
    }
}

/// A record the workspace holds none of is asked for once, however often a list opens it, and
/// asked for again once something arrived.
#[test]
fn a_record_not_there_costs_one_question() {
    let port = Rc::new(MemoryWorkspace::default());
    let workspace = workspace_id();
    let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
    let mut owners = ResourceOwners::reading(port.clone());
    let store = owners.open(APP, &workspace).unwrap();
    for _ in 0..100 {
        assert!(matches!(
            store.open_if_held(&note),
            Err(ResourceOpenError::MissingResource)
        ));
    }
    assert_eq!(port.reads.get(), 1);
    {
        let mut elsewhere = ResourceOwners::writing(port.clone());
        let writer = elsewhere.open(APP, &workspace).unwrap();
        writer.create_resource(&note).unwrap();
        writer.flush().unwrap();
    }
    let before = port.reads.get();
    store.note_kept();
    assert!(matches!(
        store.open_if_held(&note),
        Ok(ResourceOpenOutcome::Loaded)
    ));
    assert_eq!(port.reads.get(), before + 1);
}

mod undo_without_the_toolkit {
    //! A process without the toolkit keeps a record's undo, history and save state with the code
    //! native apps keep them with: the record's own undo log over its document, the steps the
    //! document records with their authors, and the record's copy in the workspace's one store.

    use std::rc::Rc;

    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT, ReadDoc};
    use collab_types::MemberId;

    use crate::resource::{
        DocumentCheckout, DocumentId, MemoryWorkspace, ResourceOwners, ResourceSaveStatus,
        ResourceType,
    };
    use collab_types::history::UndoLog;
    use collab_types::history::authors::{HistoryAuthor, ResolveAuthor, UNKNOWN_AUTHOR_NAME};
    use collab_types::history::steps::{self, Scope};
    use collab_types::history::words::HistoryWords;

    const PACKAGE: &str = "dev.tilo.undo-without-toolkit";
    const WORKSPACE: &str = "33333333-3333-4333-8333-333333333333";

    /// The host's words: a person by their actor, a step by its purpose.
    struct Words;

    impl ResolveAuthor for Words {
        fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
            HistoryAuthor::new(UNKNOWN_AUTHOR_NAME, 0.0, actor_hex)
        }
    }

    impl HistoryWords for Words {
        fn sentence(&self, entry: &tilo_actions::ActionContext) -> String {
            entry.purpose.clone().unwrap_or_default()
        }
    }

    fn title(doc: &AutoCommit) -> Option<String> {
        let (value, _) = doc.get(ROOT, "title").ok()??;
        value.into_string().ok()
    }

    /// The authors of the document's steps, oldest first, as their entries record them.
    fn step_authors(doc: &mut AutoCommit) -> Vec<String> {
        tilo_resource::history::versions(doc)
            .iter()
            .filter_map(|version| version.context.as_ref()?.invoker.as_ref())
            .filter_map(|invoker| invoker.acting.person_id().map(str::to_owned))
            .collect()
    }

    #[test]
    fn a_record_keeps_its_undo_history_and_save_state_without_the_toolkit() {
        let workspace = Rc::new(MemoryWorkspace::default());
        let record = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        let ada = MemberId::person("ada");

        // The record, kept by the workspace from its creation on.
        let mut owners = ResourceOwners::writing(workspace.clone()).on_device("ada-laptop");
        let store = owners.open(PACKAGE, WORKSPACE).unwrap();
        store.create_resource(&record).unwrap();
        store.flush().unwrap();
        assert_eq!(
            store.save_status(&record.id).unwrap(),
            ResourceSaveStatus::Saved
        );

        store
            .with_history(&record.id, |doc, history| {
                // An edit is a step by its author.
                doc.put(ROOT, "title", "Draft").unwrap();
                assert!(history.commit_action_authored(doc, None, None, Some(ada.clone())));
                assert_eq!(title(doc).as_deref(), Some("Draft"));
                let step = history
                    .next_undo_as(doc, Some(&ada))
                    .expect("a step to undo");
                assert_eq!(step.author, Some(ada.clone()));
                assert_eq!(step_authors(doc), ["ada"]);
            })
            .unwrap();
        assert_eq!(
            store.save_status(&record.id).unwrap(),
            ResourceSaveStatus::Saving
        );

        store
            .with_history(&record.id, |doc, history| {
                // Undo takes it back as a new change; redo brings it back.
                history.undo_as(doc, Some(&ada), None, None).expect("undo");
                assert_eq!(title(doc), None);
                assert!(!history.can_undo_as(doc, Some(&ada)));
                assert!(history.can_redo_as(doc, Some(&ada)));
                history.redo_as(doc, Some(&ada), None, None).expect("redo");
                assert_eq!(title(doc).as_deref(), Some("Draft"));
                assert!(steps::undone(doc).is_empty(), "the redo restored the step");
                // The undo and redo are steps of their own, by the same author.
                assert_eq!(step_authors(doc), ["ada", "ada", "ada"]);
                doc.commit();
            })
            .unwrap();

        // Saved: the store says so once the workspace took it.
        store.flush().unwrap();
        assert_eq!(
            store.save_status(&record.id).unwrap(),
            ResourceSaveStatus::Saved
        );
        let heads = store
            .read(&record.id, |doc| doc.clone().get_heads())
            .unwrap();
        drop(store);
        drop(owners);

        // On another device the step is not one Ctrl+Z there takes back.
        {
            let mut owners = ResourceOwners::writing(workspace.clone()).on_device("ada-phone");
            let store = owners.open(PACKAGE, WORKSPACE).unwrap();
            store.try_open_resource(&record).unwrap();
            let mut elsewhere = store.read(&record.id, AutoCommit::clone).unwrap();
            let history = UndoLog::new(&mut elsewhere);
            assert!(!history.can_undo_as(&elsewhere, Some(&ada)));
        }

        // Reopened by another process on the same device (the app restarted), the record holds
        // its history: the same heads and authors, and a new session's undo log takes the step
        // back again, as the person's own.
        let mut owners = ResourceOwners::writing(workspace).on_device("ada-laptop");
        let store = owners.open(PACKAGE, WORKSPACE).unwrap();
        store.try_open_resource(&record).unwrap();
        let mut reloaded = store.read(&record.id, AutoCommit::clone).unwrap();
        assert_eq!(reloaded.get_heads(), heads);
        assert_eq!(title(&reloaded).as_deref(), Some("Draft"));
        assert_eq!(step_authors(&mut reloaded), ["ada", "ada", "ada"]);
        let mut history = UndoLog::new(&mut reloaded);
        assert!(history.can_undo_as(&reloaded, Some(&ada)));

        // An agent's undo by scope reads the same steps from the document.
        let undone = steps::undo(
            &mut reloaded,
            &Scope::Invoker(tilo_actions::MemberId::person("ada").into()),
            1,
            |_, _| Ok(None),
            &Words,
        )
        .expect("undo by scope");
        assert_eq!(undone.len(), 1);
        reloaded.commit();
        assert_eq!(title(&reloaded), None);
        history.rebase(&mut reloaded);
    }
}

tilo_test_env::scenario! {
    fn a_window_shows_before_its_record_and_takes_it_in;

    phase the_record_is_read_off_the_frame_and_open_only_once_taken_in;
    // A window that can show before its record opens with it on its way: nothing reads or
    // writes it meanwhile, and once taken in it is the record the workspace holds.
    {
        let port = Rc::new(MemoryWorkspace::default());
        let workspace = workspace_id();
        let primary = DocumentCheckout::package_primary(ResourceType::new("note"));
        {
            let mut owners = ResourceOwners::writing(port.clone());
            let store = owners.open(APP, &workspace).unwrap();
            store.create_resource(&primary).unwrap();
            store.with(&primary.id, |doc| {
                put_text(doc, "body", "kept");
                doc.commit();
            });
            store.flush().unwrap();
        }
        let mut owners = ResourceOwners::reading(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        assert!(store.begin_opening(&primary).unwrap());
        assert!(store.is_opening(&primary.id));
        assert!(!store.is_ready(&primary.id), "nothing reads it before it is taken in");
        let opening = store.opening(&primary);
        assert!(opening.take_in(true).unwrap());
        assert!(!store.is_opening(&primary.id));
        assert_eq!(
            store.read(&primary.id, |doc| text(doc, "body")).flatten().as_deref(),
            Some("kept")
        );
    }

    phase an_open_while_it_is_on_its_way_waits_for_it;
    // Opening the record another way takes the read in first, so what the window then writes
    // lands on the record the workspace holds.
    {
        let port = Rc::new(MemoryWorkspace::default());
        let workspace = workspace_id();
        let primary = DocumentCheckout::package_primary(ResourceType::new("note"));
        {
            let mut owners = ResourceOwners::writing(port.clone());
            let store = owners.open(APP, &workspace).unwrap();
            store.create_resource(&primary).unwrap();
            store.with(&primary.id, |doc| {
                put_text(doc, "body", "kept");
                doc.commit();
            });
            store.flush().unwrap();
        }
        let mut owners = ResourceOwners::reading(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        assert!(store.begin_opening(&primary).unwrap());
        assert_eq!(
            store.try_open_resource(&primary).unwrap(),
            ResourceOpenOutcome::AlreadyOpen
        );
        assert_eq!(
            store.read(&primary.id, |doc| text(doc, "body")).flatten().as_deref(),
            Some("kept")
        );
    }

    phase a_record_the_workspace_has_not_answered_for_is_on_its_way_until_it_does;
    // A workspace still starting answers nothing yet: the record is on its way, every open of it
    // says so rather than waiting, and the windows reading the store are woken once the
    // workspace answers, when it opens as the workspace holds it.
    {
        let port = Rc::new(MemoryWorkspace::default());
        let workspace = workspace_id();
        let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        {
            let mut owners = ResourceOwners::writing(port.clone());
            let store = owners.open(APP, &workspace).unwrap();
            store.create_resource(&note).unwrap();
            store.with(&note.id, |doc| {
                put_text(doc, "body", "kept");
                doc.commit();
            });
            store.flush().unwrap();
        }
        port.withhold(note.id.route());
        let mut owners = ResourceOwners::reading(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        let window = crate::wake::WakeScope::new();
        store.wake_on_arrival(window.handle());
        assert!(matches!(
            store.try_open_resource(&note),
            Err(ResourceOpenError::Arriving)
        ));
        assert!(store.begin_opening(&note).unwrap());
        let opening = store.opening(&note);
        assert!(!opening.take_in(true).unwrap(), "nothing waits for the workspace");
        assert!(matches!(
            store.try_open_resource(&note),
            Err(ResourceOpenError::Arriving)
        ));
        let _ = window.take_unbound();
        port.answer(note.id.route());
        assert!(window.is_pending(), "the workspace's answer wakes the window");
        assert_eq!(
            store.try_open_resource(&note).unwrap(),
            ResourceOpenOutcome::AlreadyOpen
        );
        assert!(!store.is_opening(&note.id));
        assert_eq!(
            store.read(&note.id, |doc| text(doc, "body")).flatten().as_deref(),
            Some("kept")
        );
    }

    phase a_store_in_memory_opens_on_the_frame;
    {
        let mut owners = ResourceOwners::in_memory();
        let store = owners.open(APP, "memory").unwrap();
        let primary = DocumentCheckout::package_primary(ResourceType::new("note"));
        assert!(!store.begin_opening(&primary).unwrap());
    }
}

/// A workspace whose whole answers carry the store's load hint with its first two change hashes
/// swapped. A load that takes the hint lists the changes in that order; one that rebuilds them
/// lists the document's own, so what the reader lists says which it did.
struct SwappedHints(Rc<MemoryWorkspace>);

impl DocumentPort for SwappedHints {
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String> {
        let mut read = self.0.read(app, workspace, reads)?;
        // The workspace keeps what it read on this thread, as a reader there would; a workspace
        // is another process, so a reader here loads the bytes it answered.
        tilo_automerge::shared::clear();
        for (_, answer) in &mut read {
            if let DocumentRead::Whole {
                bytes, heads, hint, ..
            } = answer
            {
                let Some(real) = hint.as_deref() else {
                    continue;
                };
                let mut hashes = tilo_automerge::load_hint::hashes_for(real, bytes)
                    .expect("the store's hint names these bytes");
                hashes.swap(0, 1);
                let heads: Vec<ChangeHash> = heads.iter().copied().map(ChangeHash).collect();
                *hint = tilo_automerge::load_hint::encode(bytes, &hashes, &heads);
            }
        }
        Ok(read)
    }

    fn write(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        changes: Vec<u8>,
        founding: bool,
        readers: Option<&str>,
    ) -> Written {
        self.0.write(app, workspace, id, changes, founding, readers)
    }
}

/// Fill the record `id` with `keys` values, one change each: more than a load is worth hinting.
fn fill(store: &ResourceStore, id: &DocumentId, keys: usize) {
    store.with(id, |doc| {
        for key in 0..keys {
            put_text(doc, "n", &key.to_string());
            doc.commit();
        }
    });
}

/// A record of many changes is answered whole with the load hint for exactly its bytes, and the
/// reading store loads it taking the hint's change hashes rather than rebuilding them.
#[test]
fn a_large_record_reads_with_the_hint_its_load_uses() {
    tilo_automerge::shared::clear();
    let port = Rc::new(MemoryWorkspace::default());
    let workspace = workspace_id();
    let record = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
    let keys = tilo_automerge::load_hint::MIN_HINTED_CHANGES + 4;
    {
        let mut owners = ResourceOwners::writing(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        store.create_resource(&record).unwrap();
        fill(&store, &record.id, keys);
        store.flush().expect("the workspace took the record");
    }

    let read = port
        .read(
            APP,
            &workspace,
            Some(vec![DocumentAsk {
                id: record.id.route().to_owned(),
                since: None,
            }]),
        )
        .unwrap()
        .pop()
        .expect("the workspace answers the record")
        .1;
    let (bytes, hint) = match read {
        DocumentRead::Whole { bytes, hint, .. } => (bytes, hint),
        other => panic!("the workspace answers a record whole: {other:?}"),
    };
    let hint = hint.expect("a record of many changes comes with a hint");
    let hashes = tilo_automerge::load_hint::hashes_for(&hint, &bytes)
        .expect("the hint names exactly these bytes");
    assert!(hashes.len() > tilo_automerge::load_hint::MIN_HINTED_CHANGES);

    let mut owners = ResourceOwners::reading(Rc::new(SwappedHints(port)));
    let store = owners.open(APP, &workspace).unwrap();
    assert_eq!(
        store.try_open_resource(&record).unwrap(),
        ResourceOpenOutcome::Loaded
    );
    let mut swapped = hashes;
    swapped.swap(0, 1);
    assert_eq!(
        store
            .read(&record.id, |doc| doc
                .committed_document()
                .change_hashes()
                .to_vec())
            .unwrap(),
        swapped,
        "the load took the hint's hashes instead of rebuilding them"
    );
    assert_eq!(
        store.read(&record.id, |doc| text(doc, "n")).flatten(),
        Some((keys - 1).to_string()),
        "the record read back is the one written"
    );
}

/// The same for the record a window shows before it: the decode off the frame loads it with the
/// hint it was read with.
#[test]
fn a_large_record_opening_off_the_frame_loads_with_its_hint() {
    tilo_automerge::shared::clear();
    let port = Rc::new(MemoryWorkspace::default());
    let workspace = workspace_id();
    let primary = DocumentCheckout::package_primary(ResourceType::new("note"));
    let keys = tilo_automerge::load_hint::MIN_HINTED_CHANGES + 4;
    {
        let mut owners = ResourceOwners::writing(port.clone());
        let store = owners.open(APP, &workspace).unwrap();
        store
            .reserve_resource(&primary)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        fill(&store, &primary.id, keys);
        store.flush().expect("the workspace took the record");
    }
    let hint = match port
        .read(
            APP,
            &workspace,
            Some(vec![DocumentAsk {
                id: primary.id.route().to_owned(),
                since: None,
            }]),
        )
        .unwrap()
        .pop()
        .map(|(_, read)| read)
    {
        Some(DocumentRead::Whole { bytes, hint, .. }) => hint
            .and_then(|hint| tilo_automerge::load_hint::hashes_for(&hint, &bytes))
            .expect("the workspace answers the record with a hint for its bytes"),
        other => panic!("the workspace answers the record whole: {other:?}"),
    };

    let mut owners = ResourceOwners::reading(Rc::new(SwappedHints(port)));
    let store = owners.open(APP, &workspace).unwrap();
    assert!(store.begin_opening(&primary).unwrap());
    assert!(store.opening(&primary).take_in(true).unwrap());
    let mut swapped = hint;
    swapped.swap(0, 1);
    assert_eq!(
        store
            .read(&primary.id, |doc| doc
                .committed_document()
                .change_hashes()
                .to_vec())
            .unwrap(),
        swapped,
        "the decode off the frame took the hint's hashes"
    );
}

/// A workspace that counts the writes sent to it.
struct Counted(Rc<MemoryWorkspace>, Rc<Cell<usize>>);

impl DocumentPort for Counted {
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String> {
        self.0.read(app, workspace, reads)
    }

    fn write(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        changes: Vec<u8>,
        founding: bool,
        readers: Option<&str>,
    ) -> Written {
        self.1.set(self.1.get() + 1);
        self.0.write(app, workspace, id, changes, founding, readers)
    }
}

/// A window's change goes to the workspace once: on the window's route, never also through the
/// process's own path to the workspace, which stays open beside it for the records no window
/// shows. The route sends the record whole the first time and then each change once; however
/// often the store is ticked and flushed, the process's path sends none of it.
#[test]
fn a_windows_change_goes_to_the_workspace_once() {
    let memory = Rc::new(MemoryWorkspace::default());
    let workspace = workspace_id();
    let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
    {
        let mut owners = ResourceOwners::writing(memory.clone());
        let store = owners.open(APP, &workspace).unwrap();
        store.create_resource(&note).unwrap();
        store.flush().unwrap();
    }
    let sent = Rc::new(Cell::new(0));
    let port = Rc::new(Counted(memory, Rc::clone(&sent)));
    let mut owners = ResourceOwners::reading(port);
    let store = owners.open(APP, &workspace).unwrap();
    store.try_open_resource(&note).unwrap();
    let mut route = DocumentSync::new();
    let mut published = Vec::new();
    for body in ["typed", "typed more"] {
        store.with(&note.id, |doc| {
            put_text(doc, "body", body);
            doc.commit();
        });
        for _ in 0..3 {
            store.tick();
            store.try_flush().unwrap();
            if let Some(outbound) = route.take_own(&store, &note.id).unwrap() {
                route.advance(outbound.heads.clone());
                published.push(outbound.mutation);
            }
        }
    }
    assert_eq!(
        sent.get(),
        0,
        "the process's own path sends nothing of the window's record"
    );
    assert!(
        matches!(
            published.as_slice(),
            [
                collab_types::DocumentMutation::Snapshot(_),
                collab_types::DocumentMutation::Incremental(_)
            ]
        ),
        "the route sends the record whole, then the next change once: {published:?}"
    );
}
