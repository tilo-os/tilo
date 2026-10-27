use super::super::{
    CachedFolder, Kept, MAIL_APP, ThunderbirdAccountReceipt, ThunderbirdSelection, cache_path,
};
use super::*;
use crate::account_sources::{Inventory, SourceRoots};
use accounts::AccountStore;
use std::path::PathBuf;

const BINDING: &[u8] = b"deferred:operation-1";
const BODY: usize = 64;

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// One message with the flags the mbox itself carries, so the import has to
/// read them rather than guess.
fn message(id: &str, subject: &str, status: &str, body: &[u8], attachment: bool) -> Vec<u8> {
    let mut bytes = format!(
        "From - Mon Jan 01 00:00:00 2024\r\nFrom: Ada <ada@example.test>\r\nTo: Person <person@example.test>\r\nSubject: {subject}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{id}@example.test>\r\nX-Mozilla-Status: {status}\r\nMIME-Version: 1.0\r\n"
    )
    .into_bytes();
    if attachment {
        bytes.extend_from_slice(
            b"Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nBody with an attachment.\r\n--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjc=\r\n--b--\r\n",
        );
    } else {
        bytes.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes
}

fn prefs() -> &'static str {
    r#"user_pref("mail.accountmanager.accounts", "account1");
user_pref("mail.accountmanager.localfoldersserver", "server2");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.hostname", "imap.example.test");
user_pref("mail.server.server1.port", 993);
user_pref("mail.server.server1.userName", "person@example.test");
user_pref("mail.server.server1.socketType", 3);
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.test");
user_pref("mail.server.server2.type", "none");
user_pref("mail.server.server2.directory-rel", "[ProfD]Mail/Local Folders");
user_pref("mail.identity.id1.useremail", "person@example.test");
user_pref("mail.identity.id1.fullName", "Personal");
user_pref("mail.identity.id1.smtpServer", "smtp1");
user_pref("mail.smtpserver.smtp1.hostname", "smtp.example.test");
user_pref("mail.smtpserver.smtp1.port", 465);
user_pref("mail.smtpserver.smtp1.username", "person@example.test");
user_pref("mail.smtpserver.smtp1.try_ssl", 3);
"#
}

/// A profile whose IMAP Inbox holds `count` messages of `body` bytes each.
/// Small counts make the ordinary fixtures; a large `count` makes the folder
/// bigger than the former 64 MiB inspection cap.
fn fixture(count: usize, body: usize) -> (tempfile::TempDir, ReviewedThunderbird) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".thunderbird/abc.default");
    write(
        &home.path().join(".thunderbird/profiles.ini"),
        "[Profile0]\nName=Personal\nIsRelative=1\nPath=abc.default\n",
    );
    write(&root.join("prefs.js"), prefs());
    let filler = vec![b'x'; body];
    let mut inbox = Vec::new();
    for index in 0..count {
        let status = match index {
            1 => "0005",
            2 => "0000",
            _ => "0001",
        };
        inbox.extend_from_slice(&message(
            &format!("one-{index}"),
            &format!("Message {index}"),
            status,
            &filler,
            index == 0 && count > 1,
        ));
    }
    write(&root.join("ImapMail/imap.example.test/Inbox"), &inbox);
    write(
        &root.join("ImapMail/imap.example.test/Drafts"),
        message("draft", "Excluded draft", "0000", b"draft", false),
    );
    write(
        &root.join("Mail/Local Folders/Inbox"),
        message("local", "Local only", "0000", b"local", false),
    );
    write(
        &root.join("Mail/Local Folders/Projects.sbd/2024"),
        message("project", "Project mail", "0001", b"project", false),
    );
    let inventory: Inventory = crate::account_sources::discover(&SourceRoots {
        home: home.path().into(),
        config: home.path().join(".config"),
    });
    assert!(inventory.unreadable_sources.is_empty());
    (home, inventory.thunderbird_reviewed[0].clone())
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

fn cache_of(target: &Path, receipt: &ThunderbirdAccountReceipt) -> PathBuf {
    let store = AccountStore::load_in(&target.join(".config/tilo"));
    let account = store
        .all()
        .iter()
        .find(|account| {
            account
                .config
                .get("source_thunderbird_account")
                .and_then(serde_json::Value::as_str)
                == Some(receipt.source_id.as_str())
        })
        .unwrap();
    cache_path(
        &target.join(".cache").join(MAIL_APP).join("mail"),
        &account.id,
    )
}

fn kept(target: &Path, receipt: &ThunderbirdAccountReceipt) -> Kept {
    serde_json::from_slice(&std::fs::read(cache_of(target, receipt)).unwrap()).unwrap()
}

fn imap(receipt: &ThunderbirdImportReceipt) -> &ThunderbirdAccountReceipt {
    receipt
        .accounts
        .iter()
        .find(|account| account.needs_reconnect)
        .unwrap()
}

fn local(receipt: &ThunderbirdImportReceipt) -> &ThunderbirdAccountReceipt {
    receipt
        .accounts
        .iter()
        .find(|account| !account.needs_reconnect)
        .unwrap()
}

fn text_under(path: &Path) -> String {
    let mut text = String::new();
    let Ok(entries) = std::fs::read_dir(path) else {
        return text;
    };
    for entry in entries {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            text.push_str(&text_under(&entry.path()));
        } else if let Ok(read) = std::fs::read_to_string(entry.path()) {
            text.push_str(&read);
        }
    }
    text
}

/// The kept file the Mail app would read, declared with the same shape so a
/// field the app cannot deserialize fails here.
#[expect(
    dead_code,
    reason = "The deserialization mirror validates the complete Mail cache schema, including fields this test does not inspect."
)]
mod mail_app {
    use serde::Deserialize;
    use uuid::Uuid;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Kept {
        pub version: u32,
        pub messages: Vec<Message>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Message {
        pub id: Uuid,
        pub rfc_message_id: Option<String>,
        pub in_reply_to: Option<String>,
        pub from: Addr,
        pub to: Vec<Addr>,
        pub cc: Vec<Addr>,
        pub subject: String,
        pub body: String,
        #[serde(default)]
        pub html_body: Option<String>,
        pub attachments: Vec<Attachment>,
        pub timestamp_min: i64,
        pub read: bool,
        pub starred: bool,
        pub folder: Folder,
        #[serde(default)]
        pub labels: Vec<String>,
        #[serde(default)]
        pub account: String,
        #[serde(default)]
        pub server: Option<ServerPlace>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Addr {
        pub name: Option<String>,
        pub email: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Attachment {
        pub filename: String,
        pub mime: String,
        pub size: u64,
        pub hydrated: bool,
        #[serde(default)]
        pub content_id: Option<String>,
        #[serde(default)]
        pub kept: bool,
    }

    #[derive(Debug, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Folder {
        Inbox,
        Starred,
        Sent,
        Drafts,
        Archive,
        Trash,
        Junk,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ServerPlace {
        pub folder: Folder,
        pub uid: u32,
        pub uid_validity: u32,
    }
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
    let store = AccountStore::load_in(&target.path().join(".config/tilo"));
    assert_eq!(store.len(), 2);
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

#[test]
fn an_excluded_folder_is_neither_read_nor_copied() {
    let (home, reviewed) = fixture(2, BODY);
    let drafts_path = home
        .path()
        .join(".thunderbird/abc.default/ImapMail/imap.example.test/Drafts");
    let imap_reviewed = reviewed
        .accounts
        .iter()
        .find(|account| account.inventory.needs_reconnect)
        .unwrap();
    let drafts = imap_reviewed
        .folders
        .iter()
        .find(|folder| folder.inventory.hierarchy == ["Drafts"])
        .unwrap()
        .inventory
        .source_id
        .clone();
    let mut selection = ThunderbirdSelection::all(&reviewed);
    selection
        .accounts
        .iter_mut()
        .find(|account| account.source_id == imap_reviewed.inventory.source_id)
        .unwrap()
        .folders
        .retain(|id| id != &drafts);
    // An excluded folder is never opened: were it still read, its absence
    // would fail the import instead of leaving the folder out.
    std::fs::remove_file(&drafts_path).unwrap();
    let target = tempfile::tempdir().unwrap();
    let receipt = import(target.path(), &reviewed, &selection).unwrap();
    assert!(
        !imap(&receipt)
            .folders
            .iter()
            .any(|folder| folder.source_id == drafts)
    );
    let cache = target.path().join(".cache").join(MAIL_APP).join("mail");
    assert!(!text_under(&cache).contains("Excluded draft"));
    let retained = target
        .path()
        .join(".local/share/tilo/mail-import/thunderbird");
    assert!(!text_under(&retained).contains("Excluded draft"));
}

#[test]
fn cancelling_part_way_through_a_folder_publishes_nothing() {
    let (_home, reviewed) = fixture(8, BODY);
    let selection = ThunderbirdSelection::all(&reviewed);
    let target = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let error = import_with(
        target.path(),
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
    assert!(!target.path().join(".config/tilo/accounts").exists());
    let cache = target.path().join(".cache").join(MAIL_APP).join("mail");
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
}

#[test]
fn a_source_that_changed_after_review_is_refused_before_publication() {
    let (home, reviewed) = fixture(2, BODY);
    let selection = ThunderbirdSelection::all(&reviewed);
    let inbox = home
        .path()
        .join(".thunderbird/abc.default/ImapMail/imap.example.test/Inbox");
    let before = std::fs::read(&inbox).unwrap();
    let mut grown = before.clone();
    grown.extend_from_slice(&message("late", "Added later", "0001", b"late", false));
    std::fs::write(&inbox, &grown).unwrap();
    let target = tempfile::tempdir().unwrap();
    let error = import(target.path(), &reviewed, &selection).unwrap_err();
    assert!(
        error.to_string().contains("changed since it was reviewed"),
        "{error}"
    );
    assert!(!target.path().join(".config/tilo/accounts").exists());
    std::fs::write(&inbox, &before).unwrap();
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
fn flags_folders_attachments_and_existing_accounts_survive_the_streamed_import() {
    let (home, reviewed) = fixture(3, BODY);
    let selection = ThunderbirdSelection::all(&reviewed);
    let source_inbox = home
        .path()
        .join(".thunderbird/abc.default/ImapMail/imap.example.test/Inbox");
    let original = std::fs::read(&source_inbox).unwrap();
    let target = tempfile::tempdir().unwrap();
    let root = target.path().join(".config/tilo");
    let mut store = AccountStore::new();
    let mut existing =
        accounts::Account::new(accounts::AccountId::new("person@example.test"), "email");
    existing.display_name = "Existing owner label".into();
    existing.config =
        serde_json::json!({"email":"person@example.test","owner_extension":"preserve-me"});
    store.upsert(existing.clone());
    store.save_in(&root).unwrap();

    let receipt = import(target.path(), &reviewed, &selection).unwrap();
    assert!(!imap(&receipt).created);
    let stored = AccountStore::load_in(&root);
    assert_eq!(stored.len(), 2);
    let account = stored.get(&existing.id).unwrap();
    assert_eq!(account.type_id.as_str(), "email");
    assert_eq!(account.config["owner_extension"], "preserve-me");
    assert_eq!(
        account.config["source_provenance"]["mail_client"],
        "thunderbird"
    );

    let kept_mail = kept(target.path(), imap(&receipt));
    assert_eq!(kept_mail.messages.len(), 4, "the Inbox and Drafts folders");
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
    assert_eq!(AccountStore::load_in(&root).len(), 2);
}

#[test]
fn an_oversized_message_is_reported_by_folder_and_nothing_is_published() {
    let (_home, reviewed) = fixture(1, 17 * 1024 * 1024);
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
        !inbox.inventory.counted,
        "inspection reports the folder as not fully counted"
    );
    assert!(inbox.inventory.messages.is_none());
    let selection = ThunderbirdSelection::all(&reviewed);
    let target = tempfile::tempdir().unwrap();
    let error = import(target.path(), &reviewed, &selection).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("Inbox"), "{error}");
    assert!(
        error.to_string().contains("bound for one message"),
        "{error}"
    );
    assert!(!target.path().join(".config/tilo/accounts").exists());
}

#[test]
fn a_selection_from_another_operation_is_refused() {
    let (_home, reviewed) = fixture(1, BODY);
    let selection = ThunderbirdSelection::all(&reviewed);
    let target = tempfile::tempdir().unwrap();
    assert_eq!(
        import_reviewed(
            target.path(),
            &reviewed,
            &selection,
            b"",
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidData
    );
    let mut widened = selection;
    widened.accounts[0].folders.push("not-a-folder".into());
    assert!(
        import_reviewed(
            target.path(),
            &reviewed,
            &widened,
            BINDING,
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(!target.path().join(".config/tilo/accounts").exists());
}
