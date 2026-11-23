//! The streamed import of a profile straight from the granted home, over the profile
//! `super::super::tests` writes for both import paths.

use super::super::tests::{
    BODY, cache_of, existing_mailbox, imap, kept, leave_out_drafts, local, mail_app, message,
    profile, reviewed, text_under,
};
use super::super::{CachedFolder, ThunderbirdSelection};
use super::*;
use std::sync::atomic::Ordering;

const BINDING: &[u8] = b"deferred:operation-1";

/// A profile whose IMAP Inbox holds `count` messages of `body` bytes, as the review found it. A
/// large `count` makes the folder bigger than the former 64 MiB inspection cap.
fn fixture(count: usize, body: usize) -> (tempfile::TempDir, ReviewedThunderbird) {
    let home = profile(count, body);
    let reviewed = reviewed(&home);
    (home, reviewed)
}

fn import(
    target: &Path,
    reviewed: &ReviewedThunderbird,
    selection: &ThunderbirdSelection,
) -> io::Result<ThunderbirdImportReceipt> {
    import_reviewed(
        target,
        reviewed,
        selection,
        BINDING,
        &AtomicBool::new(false),
    )
}

#[test]
fn a_folder_beyond_the_former_inspection_cap_streams_into_the_mail_cache() {
    const COUNT: usize = 1400;
    const MESSAGE_BYTES: usize = 48 * 1024;
    let (_home, reviewed) = fixture(COUNT, MESSAGE_BYTES);
    let imap_reviewed = reviewed
        .accounts
        .iter()
        .find(|account| account.inventory.needs_reconnect)
        .unwrap();
    let inbox = imap_reviewed
        .folders
        .iter()
        .find(|folder| folder.inventory.hierarchy == ["Inbox"])
        .unwrap();
    assert!(
        inbox.inventory.bytes > 64 * 1024 * 1024,
        "the fixture folder is {} bytes",
        inbox.inventory.bytes
    );
    assert_eq!(inbox.inventory.messages, Some(COUNT as u64));
    assert!(inbox.inventory.counted);

    let selection = ThunderbirdSelection::all(&reviewed);
    let target = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let (receipt, report) = import_with(
        target.path(),
        &reviewed,
        &selection,
        BINDING,
        &cancel,
        &mut |_: u64| {},
    )
    .unwrap();
    assert_eq!(
        imap(&receipt)
            .folders
            .iter()
            .find(|folder| folder.source_id == inbox.inventory.source_id)
            .unwrap()
            .messages,
        COUNT as u64
    );
    assert_eq!(
        report.messages,
        (COUNT as u64) + 3,
        "every selected message"
    );
    assert!(
        report.peak_message_bytes < 128 * 1024,
        "held {} bytes of a folder larger than 64 MiB",
        report.peak_message_bytes
    );
    assert!(
        report.peak_buffered_bytes < 2 * mbox::CHUNK,
        "held {} bytes of read buffer",
        report.peak_buffered_bytes
    );
    let store = Accounts::read(&target.path().join(".config/tilo")).unwrap();
    assert_eq!(store.all().unwrap().len(), 2);
    let kept = kept(target.path(), imap(&receipt));
    assert_eq!(
        kept.messages.len(),
        COUNT + 1,
        "Inbox plus the Drafts folder"
    );
    assert_eq!(kept.version, super::super::CACHE_VERSION);
    // The kept file is exactly what the Mail app deserializes.
    let app: mail_app::Kept =
        serde_json::from_slice(&std::fs::read(cache_of(target.path(), imap(&receipt))).unwrap())
            .unwrap();
    assert_eq!(app.messages.len(), COUNT + 1);
    assert_eq!(
        app.messages
            .iter()
            .filter(|message| message.body.len().saturating_add(2) >= MESSAGE_BYTES)
            .count(),
        COUNT - 1,
        "every message without the attachment carries its whole body"
    );
}

#[cfg(unix)]
#[test]
fn a_redirected_folder_file_is_refused_rather_than_followed() {
    use std::os::unix::fs::symlink;
    let (home, reviewed) = fixture(2, BODY);
    let selection = ThunderbirdSelection::all(&reviewed);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("Inbox"),
        message("outside", "Outside", "0001", b"outside", false),
    )
    .unwrap();
    let inbox = home
        .path()
        .join(".thunderbird/abc.default/ImapMail/imap.example.test/Inbox");
    std::fs::remove_file(&inbox).unwrap();
    symlink(outside.path().join("Inbox"), &inbox).unwrap();
    let target = tempfile::tempdir().unwrap();
    assert!(import(target.path(), &reviewed, &selection).is_err());
    assert!(!target.path().join(".config/tilo/accounts").exists());
}

#[test]
fn a_message_larger_than_a_review_reads_comes_over_whole() {
    const BODY: usize = 17 * 1024 * 1024;
    let (_home, reviewed) = fixture(1, BODY);
    let account = reviewed
        .accounts
        .iter()
        .find(|account| account.inventory.needs_reconnect)
        .unwrap();
    let inbox = account
        .folders
        .iter()
        .find(|folder| folder.inventory.hierarchy == ["Inbox"])
        .unwrap();
    assert!(
        inbox.inventory.counted,
        "the review counts a folder whose one message it could not read whole"
    );
    assert_eq!(inbox.inventory.messages, Some(1));
    let selection = ThunderbirdSelection::all(&reviewed);
    let target = tempfile::tempdir().unwrap();
    let receipt = import(target.path(), &reviewed, &selection).unwrap();
    let imported = imap(&receipt);
    assert_eq!(
        imported
            .folders
            .iter()
            .find(|folder| folder.source_id == inbox.inventory.source_id)
            .unwrap()
            .messages,
        1,
        "the folder came over"
    );
    let kept = kept(target.path(), imported);
    let carried = kept
        .messages
        .iter()
        .find(|message| message.body.len() >= BODY)
        .unwrap_or_else(|| {
            panic!(
                "no message came over whole: the kept bodies are {:?} bytes",
                kept.messages
                    .iter()
                    .map(|message| message.body.len())
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        carried.body.len() + 1024 >= BODY,
        "the body came over whole: {} bytes of {BODY}",
        carried.body.len()
    );
}

/// The streamed import publishes the reviewed selection or nothing: a selection of another
/// operation, a cancel part way through a folder and a source changed since its review publish no
/// account and no kept mail; a whole import keeps flags, folders, attachments and the person's
/// existing mailbox, and a retry inserts and rewrites nothing; a folder left out is never read.
#[test]
fn a_streamed_import_publishes_the_reviewed_selection_or_nothing() {
    let (home, reviewed) = fixture(8, BODY);
    let selection = ThunderbirdSelection::all(&reviewed);
    let accounts_dir = |target: &tempfile::TempDir| target.path().join(".config/tilo/accounts");

    let other = tempfile::tempdir().unwrap();
    assert_eq!(
        import_reviewed(
            other.path(),
            &reviewed,
            &selection,
            b"",
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidData
    );
    let mut widened = selection.clone();
    widened.accounts[0].folders.push("not-a-folder".into());
    assert!(import(other.path(), &reviewed, &widened).is_err());
    assert!(!accounts_dir(&other).exists());

    let cancelled = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let error = import_with(
        cancelled.path(),
        &reviewed,
        &selection,
        BINDING,
        &cancel,
        &mut |messages: u64| {
            if messages >= 3 {
                cancel.store(true, Ordering::Release);
            }
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(!accounts_dir(&cancelled).exists());
    let cache = cancelled.path().join(".cache").join(MAIL_APP).join("mail");
    if cache.exists() {
        let kept: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().into_string().unwrap();
                name.rsplit_once('.')
                    .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("json"))
                    .then_some(name)
            })
            .collect();
        assert!(
            kept.is_empty(),
            "a cancelled import leaves no kept mail behind: {kept:?}"
        );
    }

    let profile_root = home.path().join(".thunderbird/abc.default");
    let source_inbox = profile_root.join("ImapMail/imap.example.test/Inbox");
    let original = std::fs::read(&source_inbox).unwrap();
    let target = tempfile::tempdir().unwrap();
    let existing = existing_mailbox(target.path());
    let receipt = import(target.path(), &reviewed, &selection).unwrap();
    assert!(!imap(&receipt).created);
    let root = target.path().join(".config/tilo");
    let stored = Accounts::read(&root).unwrap();
    assert_eq!(stored.all().unwrap().len(), 2);
    let account = stored.get(existing.id.as_str()).unwrap().unwrap();
    assert_eq!(account.type_id.as_str(), "email");
    assert_eq!(account.config["owner_extension"], "preserve-me");
    assert_eq!(
        account.config["source_provenance"]["mail_client"],
        "thunderbird"
    );
    let kept_mail = kept(target.path(), imap(&receipt));
    assert_eq!(kept_mail.messages.len(), 9, "the Inbox and Drafts folders");
    let flagged = kept_mail
        .messages
        .iter()
        .find(|message| message.subject == "Message 1")
        .expect("the flagged message");
    assert!(flagged.read && flagged.starred);
    let unread = kept_mail
        .messages
        .iter()
        .find(|message| message.subject == "Message 2")
        .unwrap();
    assert!(!unread.read && !unread.starred);
    let attached = kept_mail
        .messages
        .iter()
        .find(|message| !message.attachments.is_empty())
        .unwrap();
    assert_eq!(attached.attachments[0].filename, "report.pdf");
    assert_eq!(attached.folder, CachedFolder::Inbox);
    let files = cache_of(target.path(), imap(&receipt)).with_extension("files");
    assert_eq!(
        std::fs::read(files.join(format!("{}.0", attached.id))).unwrap(),
        b"%PDF-1.7"
    );
    let local_kept = kept(target.path(), local(&receipt));
    assert!(local_kept.messages.iter().any(|message| {
        message
            .labels
            .iter()
            .any(|label| label == "Thunderbird/Projects/2024")
    }));
    let retained = target
        .path()
        .join(".local/share/tilo/mail-import/thunderbird");
    assert!(std::fs::read_dir(&retained).unwrap().count() > 0);
    let receipt_json = serde_json::to_string(&receipt).unwrap();
    assert!(!receipt_json.contains("person@example.test"));
    assert!(!receipt_json.contains(&target.path().to_string_lossy().to_string()));
    assert_eq!(
        std::fs::read(&source_inbox).unwrap(),
        original,
        "the import never rewrites the source mail"
    );
    let cache_bytes = std::fs::read(cache_of(target.path(), imap(&receipt))).unwrap();
    let again = import(target.path(), &reviewed, &selection).unwrap();
    assert!(again.accounts.iter().all(|account| !account.created));
    assert!(
        again
            .accounts
            .iter()
            .all(|account| account.folders.iter().all(|folder| folder.messages == 0)),
        "a second import inserts nothing"
    );
    assert_eq!(
        std::fs::read(cache_of(target.path(), imap(&again))).unwrap(),
        cache_bytes,
        "a retry that inserts nothing rewrites nothing"
    );
    assert_eq!(Accounts::read(&root).unwrap().all().unwrap().len(), 2);

    // An excluded folder is never opened: were it still read, its absence would fail the import
    // instead of leaving the folder out.
    let mut without_drafts = selection;
    let drafts = leave_out_drafts(&reviewed, &mut without_drafts);
    std::fs::remove_file(profile_root.join("ImapMail/imap.example.test/Drafts")).unwrap();
    let excluded = tempfile::tempdir().unwrap();
    let receipt = import(excluded.path(), &reviewed, &without_drafts).unwrap();
    assert!(
        !imap(&receipt)
            .folders
            .iter()
            .any(|folder| folder.source_id == drafts)
    );
    let cache = excluded.path().join(".cache").join(MAIL_APP).join("mail");
    assert!(!text_under(&cache).contains("Excluded draft"));
    let retained = excluded
        .path()
        .join(".local/share/tilo/mail-import/thunderbird");
    assert!(!text_under(&retained).contains("Excluded draft"));

    let mut grown = original;
    grown.extend_from_slice(&message("late", "Added later", "0001", b"late", false));
    std::fs::write(&source_inbox, &grown).unwrap();
    let changed = tempfile::tempdir().unwrap();
    let error = import(changed.path(), &reviewed, &without_drafts).unwrap_err();
    assert!(
        error.to_string().contains("changed since it was reviewed"),
        "{error}"
    );
    assert!(!accounts_dir(&changed).exists());
}
