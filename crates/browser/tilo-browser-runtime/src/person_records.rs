//! The person's shared records ([`ui::record::person`]) as a page hands them
//! to the windows it mounts: the contacts and accounts other apps read, and
//! the settings sections.
//!
//! Every mount of the page keeps them in the store of the profile it shows,
//! and one writer holds each app's records for all of them: the person's
//! records are the same whichever room a window is open in, so two
//! desktops of one page must not open them twice. The page writes them out
//! with its other records ([`flush`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use durable_store::DurableStore;
use ui::permissions::UserId;
use ui::record::RecordOwners;
use ui::record::person::PersonRecords;

/// The person's records kept in one store, by app and person.
struct Kept {
    owners: RecordOwners,
    records: HashMap<(String, String), PersonRecords>,
}

thread_local! {
    /// By the address of the store they are kept in. Each entry holds that
    /// store, so the address names it for as long as the page runs.
    static KEPT: RefCell<HashMap<usize, Kept>> = RefCell::new(HashMap::new());
}

fn key<T: ?Sized>(store: &Rc<T>) -> usize {
    Rc::as_ptr(store).cast::<()>() as usize
}

/// `app_id`'s records of the person `author`, kept in `store` by the one
/// writer every mount of the page shares.
///
/// # Errors
/// Rejects an invalid app id.
pub(crate) fn of(
    store: &Rc<dyn DurableStore>,
    app_id: &str,
    author: &str,
) -> Result<PersonRecords, String> {
    KEPT.with_borrow_mut(|kept| {
        let kept = kept.entry(key(store)).or_insert_with(|| Kept {
            owners: RecordOwners::with_store(Rc::clone(store)),
            records: HashMap::new(),
        });
        let held = (app_id.to_owned(), author.to_owned());
        if let Some(records) = kept.records.get(&held) {
            return Ok(records.clone());
        }
        let records = PersonRecords::open(&mut kept.owners, app_id, UserId(author.to_owned()))
            .map_err(|error| error.to_string())?;
        kept.records.insert(held, records.clone());
        Ok(records)
    })
}

/// Give `context` its app's records of the person it is prepared for.
/// Not for a private window, which keeps nothing of theirs.
///
/// # Errors
/// Rejects an invalid app id.
pub(crate) fn provide(
    context: &mut ui::application::HostContext,
    store: &Rc<dyn DurableStore>,
) -> Result<(), String> {
    if context.is_private() {
        return Ok(());
    }
    let records = of(store, context.app_id(), &context.author().0)?;
    context.provide_person_records(records);
    Ok(())
}

/// Write out what the person's records in `store` hold that is not stored
/// yet, as the page does with every mount's records.
pub(crate) fn flush<T: ?Sized>(store: &Rc<T>) {
    for error in store_all(store) {
        tracing::warn!(%error, "the person's records could not be stored");
    }
}

/// Hand the store what the person's records in `store` hold, and say what
/// it refused. The page never waits on its store: the host commits the
/// writes the store accepted later, from its own queue, so an accepted
/// write that is not committed yet is not a failure.
fn store_all<T: ?Sized>(store: &Rc<T>) -> Vec<String> {
    let _page = durable_store::waiting::forbid_on_this_thread();
    KEPT.with_borrow(|kept| {
        kept.get(&key(store))
            .into_iter()
            .flat_map(|kept| kept.records.values())
            .filter_map(|records| records.flush().err())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::application::{
        AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget,
        RestoreInput,
    };
    use ui::application_view::InstanceViewStore;
    use ui::automerge::transaction::Transactable as _;
    use ui::automerge::{AutoCommit, ROOT, ReadDoc as _};
    use ui::record::RecordKind;

    const APP: &str = "so.tilo.chat";
    const PERSON: &str = "4f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
    const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn page() -> Rc<dyn DurableStore> {
        Rc::new(durable_store::MemoryStore::new())
    }

    fn drafts() -> RecordKind {
        RecordKind::new("tilo.chat.drafts")
    }

    fn genesis(doc: &mut AutoCommit) -> Result<(), String> {
        doc.put(ROOT, "draft", "")
            .map_err(|error| error.to_string())
    }

    fn write(records: &PersonRecords, draft: &str) {
        let record = records.open_record(drafts(), &genesis).unwrap();
        record
            .edit(true, |doc| {
                doc.put(ROOT, "draft", draft)
                    .map_err(|error| error.to_string())
            })
            .unwrap();
    }

    fn read(records: &PersonRecords) -> Option<String> {
        let record = records.open_record(drafts(), &genesis).unwrap();
        record
            .read(|doc| {
                doc.get(ROOT, "draft")
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.into_string().ok())
            })
            .flatten()
    }

    fn unused(
        _: HostContext,
        _: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, ui::launch::LaunchError> {
        Err(ui::launch::LaunchError::Operation(
            "the tests prepare contexts only".into(),
        ))
    }

    static CHAT: std::sync::LazyLock<ApplicationDefinition> =
        std::sync::LazyLock::new(|| ApplicationDefinition {
            app_id: APP.into(),
            title: "Chat".into(),
            icon: ui::IconDescriptor::Glyph("C"),
            record_kind: RecordKind::new("chat"),
            required: vec![],
            optional: vec![],
            opens: &[],
            room_record: false,
            member_record: false,
            opens_before_record: false,
            create: unused,
        });

    /// A window of the page's desktop, prepared as the desktop prepares it.
    fn window(store: &Rc<dyn DurableStore>, instance: u64) -> HostContext {
        let records = RecordOwners::with_store(Rc::clone(store))
            .open(APP, ROOM)
            .unwrap();
        let views = Rc::new(InstanceViewStore::with_store(Rc::clone(store), "D", ROOM).unwrap());
        HostContext::prepare(
            &CHAT,
            HostContextInputs {
                records,
                views,
                device_id: "D".into(),
                instance_id: instance,
                permissions: ui::permissions::SharedPermissions::new(
                    ui::permissions::PermissionContext::permissive(),
                ),
                author: UserId(PERSON.into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            &LaunchTarget::New,
        )
        .unwrap()
    }

    #[test]
    fn a_window_the_page_mounts_holds_the_persons_records() {
        let store = page();
        let mut first = window(&store, 1);
        provide(&mut first, &store).unwrap();
        write(
            first.person_records().expect("the person's records"),
            "half a thought",
        );
        // Another window of the page reads what this one wrote.
        let mut second = window(&store, 2);
        provide(&mut second, &store).unwrap();
        assert_eq!(
            read(second.person_records().unwrap()).as_deref(),
            Some("half a thought")
        );
    }

    #[test]
    fn a_private_window_keeps_nothing_of_the_persons() {
        let store = page();
        let mut private = window(&store, 1);
        private.provide_private();
        provide(&mut private, &store).unwrap();
        assert!(private.person_records().is_none());
    }

    #[test]
    fn every_mount_of_the_page_shares_one_writer() {
        let store = page();
        write(&of(&store, APP, PERSON).unwrap(), "see you at six");
        // A second desktop of the page opens the same records, not a
        // replica that stores nothing.
        assert_eq!(
            read(&of(&store, APP, PERSON).unwrap()).as_deref(),
            Some("see you at six")
        );
    }

    /// The page's store commits through its host later: a write it
    /// accepted and has not committed yet is stored as far as the page goes.
    #[test]
    fn writes_the_host_has_yet_to_commit_are_no_failure() {
        let memory = Rc::new(durable_store::MemoryStore::queued());
        let store: Rc<dyn DurableStore> = memory.clone();
        write(&of(&store, APP, PERSON).unwrap(), "back in five");
        assert_eq!(store_all(&store), Vec::<String>::new());
        assert!(
            !memory.drain().is_empty(),
            "the writes wait for the host to commit them"
        );
    }

    #[test]
    fn what_the_person_wrote_outlives_the_page() {
        let store = page();
        write(&of(&store, APP, PERSON).unwrap(), "on my way");
        flush(&store);
        // The page closes; the next one reads what its profile stored.
        KEPT.with_borrow_mut(HashMap::clear);
        let mut owners = RecordOwners::with_store(Rc::clone(&store));
        let reloaded = PersonRecords::open(&mut owners, APP, UserId(PERSON.into())).unwrap();
        assert_eq!(read(&reloaded).as_deref(), Some("on my way"));
    }
}
