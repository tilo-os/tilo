use super::*;
use automerge::ReadDoc as _;

fn enabled(record: &PersonRecord) -> Option<bool> {
    record
        .read(|doc| {
            let Some((automerge::Value::Scalar(value), _)) =
                doc.get(automerge::ROOT, "enabled").ok()?
            else {
                return None;
            };
            match value.as_ref() {
                automerge::ScalarValue::Boolean(value) => Some(*value),
                _ => None,
            }
        })
        .flatten()
}

#[test]
fn cross_app_person_records_read_current_original_writer_without_replica_or_file_reopen() {
    let root = std::env::temp_dir().join(format!("tilo-person-owner-{}", uuid::Uuid::new_v4()));
    let author = UserId("11".repeat(32));
    let mut owners = RecordOwners::new(root.clone());
    let settings = PersonRecords::open(&mut owners, "so.tilo.settings", author.clone()).unwrap();
    let kind = RecordKind::new("synthetic.accounts");
    let original = settings
        .open_original_record(kind.clone(), |doc| {
            doc.put(automerge::ROOT, "enabled", true)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let messages = PersonRecords::open(&mut owners, "so.tilo.messages", author.clone()).unwrap();
    let shared = messages.of_package("so.tilo.settings").unwrap();
    let read = shared
        .open_original_record(kind.clone(), |_| Err("must reuse original document".into()))
        .unwrap();
    original
        .edit(false, |doc| {
            doc.put(automerge::ROOT, "enabled", false)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert_eq!(
        enabled(&read),
        Some(false),
        "an unflushed edit in the original writer is immediately visible across apps"
    );
    original
        .edit(false, |doc| {
            doc.put(automerge::ROOT, "enabled", true)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert_eq!(enabled(&read), Some(true));
    let mut other_profile = RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
    let separate = PersonRecords::open(&mut other_profile, "so.tilo.messages", author)
        .unwrap()
        .of_package("so.tilo.settings")
        .unwrap()
        .open_record(kind, &|doc| {
            doc.put(automerge::ROOT, "enabled", false)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert_eq!(enabled(&separate), Some(false));
    assert_eq!(
        enabled(&read),
        Some(true),
        "the same labels in another profile do not select this original owner"
    );
    settings.flush().unwrap();
    drop(separate);
    drop(other_profile);
    drop(read);
    drop(shared);
    drop(messages);
    drop(original);
    drop(settings);
    drop(owners);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn canonical_edit_refuses_a_replica_without_initializing_shadow_state() {
    let records = PersonRecords::replica("so.tilo.settings", UserId("11".repeat(32))).unwrap();
    let initialized = std::cell::Cell::new(0);
    let result = records.open_original_record(RecordKind::new("synthetic.accounts"), |_| {
        initialized.set(initialized.get() + 1);
        Ok(())
    });
    assert!(matches!(result, Err(RecordOpenError::IdentityMismatch)));
    assert_eq!(initialized.get(), 0);
}
