use super::*;
use crate::account_sources::{Inventory, SourceRoots};
use accounts::AccountStore;

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn message(id: &str, subject: &str, status: &str, attachment: bool) -> String {
    let attachment = if attachment {
        "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nBody with an attachment.\r\n--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjc=\r\n--b--\r\n"
    } else {
        "MIME-Version: 1.0\r\nContent-Type: text/plain\r\n\r\nPlain body.\r\n"
    };
    format!(
        "From - Mon Jan 01 00:00:00 2024\r\nFrom: Ada <ada@example.test>\r\nTo: Person <person@example.test>\r\nSubject: {subject}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{id}@example.test>\r\nX-Mozilla-Status: {status}\r\n{attachment}"
    )
}

fn profile() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".thunderbird/abc.default");
    std::fs::create_dir_all(root.join("ImapMail/imap.example.test")).unwrap();
    std::fs::create_dir_all(root.join("Mail/Local Folders/Projects.sbd")).unwrap();
    std::fs::write(
        home.path().join(".thunderbird/profiles.ini"),
        "[Profile0]\nName=Personal\nIsRelative=1\nPath=abc.default\n",
    )
    .unwrap();
    std::fs::write(
        root.join("prefs.js"),
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
"#,
    )
    .unwrap();
    write(
        &root.join("ImapMail/imap.example.test/Inbox"),
        format!(
            "{}{}",
            message("one", "One", "0001", true),
            message("two", "Two", "0005", false)
        ),
    );
    write(
        &root.join("Mail/Local Folders/Inbox"),
        message("local", "Local only", "0000", false),
    );
    write(
        &root.join("Mail/Local Folders/Projects.sbd/2024"),
        message("project", "Project mail", "0001", false),
    );
    home
}

fn discovered(home: &tempfile::TempDir) -> Inventory {
    crate::account_sources::discover(&SourceRoots {
        home: home.path().into(),
        config: home.path().join(".config"),
    })
}

fn fixture() -> (tempfile::TempDir, Inventory) {
    let home = profile();
    let inventory = discovered(&home);
    (home, inventory)
}

fn cache_path_for(target: &Path, stem: &str) -> PathBuf {
    target.join(cache_file(stem))
}

fn cache_file(stem: &str) -> PathBuf {
    PathBuf::from(".cache")
        .join(MAIL_APP)
        .join("mail")
        .join(format!("{stem}.json"))
}

/// The account whose mail is only on this computer (Local Folders).
fn local_account(receipt: &ThunderbirdImportReceipt) -> &ThunderbirdAccountReceipt {
    receipt
        .accounts
        .iter()
        .find(|account| !account.needs_reconnect)
        .unwrap()
}

/// The kept-mail file stem for a receipt account. Receipts carry no address,
/// so the canonical store is what names the account.
fn cache_stem(target: &Path, receipt: &ThunderbirdAccountReceipt) -> String {
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
    file_stem(account.id.as_str())
}

fn read_kept(path: &Path) -> Kept {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn key() -> SymmetricKey {
    SymmetricKey::generate().unwrap()
}

const BINDING: &[u8] = b"operation-7/target-user-1000";

#[test]
fn selected_local_mail_round_trips_into_the_canonical_mail_store() {
    let (home, inventory) = fixture();
    let source = home
        .path()
        .join(".thunderbird/abc.default/ImapMail/imap.example.test/Inbox");
    let original = std::fs::read(&source).unwrap();
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    assert!(!String::from_utf8_lossy(sealed.as_bytes()).contains("person@example.test"));
    let target = tempfile::tempdir().unwrap();
    let receipt = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    assert_eq!(receipt.accounts.len(), 2);
    let imap = receipt
        .accounts
        .iter()
        .find(|account| account.needs_reconnect)
        .unwrap();
    assert!(imap.created);
    assert_eq!(
        imap.folders
            .iter()
            .find(|folder| folder.messages == 2)
            .unwrap()
            .messages,
        2
    );
    let root = target.path().join(".config/tilo");
    let store = AccountStore::load_in(&root);
    let account = store
        .get(&AccountId::new("person@example.test"))
        .expect("the IMAP account");
    assert_eq!(
        account.type_id.as_str(),
        accounts::thunderbird::PENDING_TYPE
    );
    let config = account.config_as::<AccountConfig>().unwrap();
    assert!(config.import_pending);
    assert!(config.password.is_empty());
    assert_eq!(config.imap_host, "imap.example.test");
    assert_eq!(
        account.config["source_provenance"]["mail_client"],
        "thunderbird"
    );
    let metadata = std::fs::read_to_string(accounts::store_path_in(&root)).unwrap();
    assert!(!metadata.contains("password"));
    assert!(!metadata.contains("launch_a_program"));

    let cache = target
        .path()
        .join(".cache/so.tilo.mail/mail/person@example.test.json");
    let kept: Kept = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert_eq!(kept.messages.len(), 2);
    let flagged = kept
        .messages
        .iter()
        .find(|message| message.subject == "Two")
        .unwrap();
    assert!(flagged.read && flagged.starred);
    let attachment = kept
        .messages
        .iter()
        .find(|message| !message.attachments.is_empty())
        .unwrap();
    assert_eq!(attachment.attachments[0].filename, "report.pdf");
    assert!(
        target
            .path()
            .join(format!(
                ".cache/so.tilo.mail/mail/person@example.test.files/{}.0",
                attachment.id
            ))
            .exists()
    );
    let local = receipt
        .accounts
        .iter()
        .find(|account| !account.needs_reconnect)
        .unwrap();
    let local_cache = cache_path_for(target.path(), &cache_stem(target.path(), local));
    let local_kept: Kept = serde_json::from_slice(&std::fs::read(local_cache).unwrap()).unwrap();
    assert_eq!(local_kept.messages.len(), 2);
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
        std::fs::read(&source).unwrap(),
        original,
        "import never rewrites the source mail"
    );

    let again = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    assert!(again.accounts.iter().all(|account| !account.created));
    assert_eq!(AccountStore::load_in(&root).len(), 2);
}

#[test]
fn the_same_identity_keeps_an_existing_mailbox_auth_and_gains_local_messages() {
    let (_home, inventory) = fixture();
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let target = tempfile::tempdir().unwrap();
    let root = target.path().join(".config/tilo");
    let mut store = AccountStore::new();
    let id = accounts::AccountId::new("person@example.test");
    let mut existing = accounts::Account::new(id.clone(), "email");
    existing.display_name = "Existing mailbox".into();
    existing.config = serde_json::json!({
        "display_name": "Existing mailbox",
        "email": "person@example.test",
        "imap_host": "mail.example.test",
        "imap_port": 993,
        "imap_tls": true,
        "smtp_host": "mail.example.test",
        "smtp_port": 465,
        "smtp_tls": true,
        "username": "person@example.test",
        "auth": "OAuth",
        "import_pending": false,
    });
    store.upsert(existing);
    store.save_in(&root).unwrap();
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    let receipt = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    let imported = receipt
        .accounts
        .iter()
        .find(|account| account.source_id == reviewed.accounts[0].inventory.source_id)
        .unwrap();
    assert!(!imported.created);
    let store = AccountStore::load_in(&root);
    let account = store.get(&id).expect("the existing mailbox");
    assert_eq!(account.type_id.as_str(), "email");
    assert_eq!(account.config["imap_host"], "mail.example.test");
    assert_eq!(account.config["auth"], "OAuth");
    let cache = target
        .path()
        .join(".cache/so.tilo.mail/mail/person@example.test.json");
    let kept: Kept = serde_json::from_slice(&std::fs::read(cache).unwrap()).unwrap();
    assert_eq!(kept.messages.len(), 2, "the local messages still arrive");
}

#[test]
fn excluding_a_folder_removes_its_messages_from_the_capture_and_the_destination() {
    let (_home, inventory) = fixture();
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let mut selection = ThunderbirdSelection::all(&reviewed);
    let imap = selection
        .accounts
        .iter_mut()
        .find(|account| account.folders.len() == 1)
        .unwrap();
    assert_eq!(imap.folders.len(), 1);
    imap.folders.clear();
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    let target = tempfile::tempdir().unwrap();
    let receipt = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    assert!(
        receipt
            .accounts
            .iter()
            .find(|account| account.needs_reconnect)
            .is_some_and(|account| account.folders.is_empty())
    );
    let cache = target
        .path()
        .join(".cache/so.tilo.mail/mail/person@example.test.json");
    assert!(!cache.exists());
    let retained = target
        .path()
        .join(".local/share/tilo/mail-import/thunderbird");
    if retained.exists() {
        assert!(
            !std::fs::read_dir(&retained).unwrap().any(|entry| entry
                .unwrap()
                .path()
                .join("Inbox.mbox")
                .exists())
        );
    }
}

#[test]
fn a_wrong_key_binding_or_selection_cannot_open_or_widen_the_artifact() {
    let (_home, inventory) = fixture();
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let transport_key = key();
    let sealed = seal_selected(&reviewed, &selection, &transport_key, BINDING).unwrap();
    let target = tempfile::tempdir().unwrap();
    assert_eq!(
        import_selected(target.path(), &sealed, &key(), BINDING, &selection).unwrap_err(),
        ConnectionError::CredentialUnavailable
    );
    assert_eq!(
        import_selected(
            target.path(),
            &sealed,
            &transport_key,
            b"another-operation",
            &selection
        )
        .unwrap_err(),
        ConnectionError::CredentialUnavailable
    );
    let mut widened = selection;
    widened.accounts[0].folders.push("not-a-folder".into());
    assert_eq!(
        import_selected(target.path(), &sealed, &transport_key, BINDING, &widened).unwrap_err(),
        ConnectionError::InvalidConfiguration
    );
    assert!(
        !target
            .path()
            .join(".config/tilo/accounts/store.json")
            .exists()
    );
}

#[test]
fn an_existing_account_with_the_same_email_is_reused_without_overwriting_its_configuration() {
    let (_home, inventory) = fixture();
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    let target = tempfile::tempdir().unwrap();
    let root = target.path().join(".config/tilo");
    let mut store = AccountStore::new();
    let mut existing = Account::new(AccountId::new("person@example.test"), "email");
    existing.display_name = "Existing owner label".into();
    existing.config =
        serde_json::json!({"email":"person@example.test","owner_extension":"preserve-me"});
    store.upsert(existing.clone());
    store.save_in(&root).unwrap();
    let receipt = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    assert!(
        receipt
            .accounts
            .iter()
            .find(|account| account.needs_reconnect)
            .is_some_and(|account| !account.created)
    );
    let stored = AccountStore::load_in(&root)
        .get(&existing.id)
        .unwrap()
        .clone();
    assert_eq!(stored.type_id.as_str(), "email");
    assert_eq!(stored.config["owner_extension"], "preserve-me");
    assert_eq!(
        stored.config["source_provenance"]["mail_client"],
        "thunderbird"
    );
}

/// Mirrors `apps/mail/src/model.rs` (`Message`, `Addr`, `Attachment`, `Folder`,
/// `ServerPlace`) and the `Kept` envelope in `apps/mail/src/store.rs`.
/// `deny_unknown_fields` makes import fail this test if it writes anything the
/// Mail app cannot deserialize. The unused variants keep the mirror exact.
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

#[test]
fn the_canonical_mail_cache_is_exactly_what_the_mail_app_reads() {
    let (_home, inventory) = fixture();
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    let target = tempfile::tempdir().unwrap();
    let receipt = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    let imap = receipt
        .accounts
        .iter()
        .find(|account| account.needs_reconnect)
        .unwrap();
    let path = cache_path_for(target.path(), &cache_stem(target.path(), imap));
    let kept: mail_app::Kept = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(kept.version, 1);
    assert_eq!(kept.messages.len(), 2);
    let flagged = kept
        .messages
        .iter()
        .find(|message| message.subject == "Two")
        .unwrap();
    assert!(flagged.read && flagged.starred);
    assert_eq!(flagged.folder, mail_app::Folder::Inbox);
    assert_eq!(flagged.from.email, "ada@example.test");
    assert_eq!(flagged.account, imap.source_id);
    let attached = kept
        .messages
        .iter()
        .find(|message| !message.attachments.is_empty())
        .unwrap();
    let attachment = &attached.attachments[0];
    assert_eq!(attachment.filename, "report.pdf");
    assert!(attachment.hydrated && attachment.kept);
    let bytes = std::fs::read(
        path.with_extension("files")
            .join(format!("{}.0", attached.id)),
    )
    .unwrap();
    assert_eq!(bytes, b"%PDF-1.7");
    assert_eq!(attachment.size, bytes.len() as u64);
}

#[test]
fn kept_mail_is_merged_and_never_loses_the_persons_flags_folder_or_labels() {
    let (_home, inventory) = fixture();
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    let target = tempfile::tempdir().unwrap();
    let receipt = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    let local = local_account(&receipt).clone();
    let path = cache_path_for(target.path(), &cache_stem(target.path(), &local));
    let mut kept = read_kept(&path);
    assert_eq!(kept.messages.len(), 2);
    if let Some(message) = kept
        .messages
        .iter_mut()
        .find(|message| message.subject == "Local only")
    {
        message.read = true;
        message.starred = true;
        message.folder = CachedFolder::Trash;
        message.labels = vec!["keep-me".into()];
    } else {
        panic!("the local-only message is kept");
    }
    let mut foreign = kept
        .messages
        .iter()
        .find(|message| message.subject == "Project mail")
        .unwrap()
        .clone();
    foreign.id = uuid::Uuid::new_v4();
    foreign.subject = "Written here".into();
    foreign.rfc_message_id = None;
    foreign.attachments = Vec::new();
    kept.messages.push(foreign);
    std::fs::write(&path, serde_json::to_vec(&kept).unwrap()).unwrap();

    let again = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    assert!(again.accounts.iter().all(|account| !account.created));
    let kept = read_kept(&path);
    assert_eq!(
        kept.messages.len(),
        3,
        "the imported copy is not added twice"
    );
    assert_eq!(
        kept.messages
            .iter()
            .filter(|message| message.subject == "Local only")
            .count(),
        1
    );
    let one = kept
        .messages
        .iter()
        .find(|message| message.subject == "Local only")
        .unwrap();
    assert!(one.read && one.starred);
    assert_eq!(one.folder, CachedFolder::Trash);
    assert_eq!(one.labels, ["keep-me"]);
    assert!(
        kept.messages
            .iter()
            .any(|message| message.subject == "Written here")
    );
}

#[test]
fn an_unreadable_kept_file_is_refused_and_never_reports_a_half_written_import() {
    let (_home, inventory) = fixture();
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    // Source IDs are deterministic, so one target teaches the other its file.
    let learned = tempfile::tempdir().unwrap();
    let first = import_selected(learned.path(), &sealed, &key, BINDING, &selection).unwrap();
    let local_stem = cache_stem(learned.path(), local_account(&first));

    let target = tempfile::tempdir().unwrap();
    let cache = target.path().join(".cache").join(MAIL_APP).join("mail");
    std::fs::create_dir_all(&cache).unwrap();
    let corrupt = cache.join(format!("{local_stem}.json"));
    std::fs::write(&corrupt, b"{not json").unwrap();
    assert_eq!(
        import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap_err(),
        ConnectionError::StorageUnavailable
    );
    assert!(
        !accounts::store_path_in(&target.path().join(".config/tilo")).exists(),
        "an account is not published while its mail is only half written"
    );
    assert_eq!(
        std::fs::read(&corrupt).unwrap(),
        b"{not json",
        "an unreadable file is left for its owner instead of being overwritten"
    );

    std::fs::remove_file(&corrupt).unwrap();
    let again = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    assert_eq!(again.accounts.len(), 2);
    assert_eq!(
        read_kept(&cache_path_for(target.path(), &local_stem))
            .messages
            .len(),
        2
    );
    assert_eq!(
        AccountStore::load_in(&target.path().join(".config/tilo")).len(),
        2
    );
    let before = std::fs::read(cache_path_for(target.path(), &local_stem)).unwrap();
    let third = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    assert!(third.accounts.iter().all(|account| !account.created));
    assert_eq!(
        std::fs::read(cache_path_for(target.path(), &local_stem)).unwrap(),
        before,
        "a retry that inserts nothing rewrites nothing"
    );
}

#[test]
fn an_excluded_drafts_folder_never_reaches_the_cache_or_the_retained_copy() {
    let home = profile();
    let root = home.path().join(".thunderbird/abc.default");
    let drafts = root.join("ImapMail/imap.example.test/Drafts");
    write(
        &drafts,
        message("excluded", "Excluded draft", "0000", false),
    );
    let original = std::fs::read(&drafts).unwrap();
    let inventory = discovered(&home);
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let mut selection = ThunderbirdSelection::all(&reviewed);
    let imap = reviewed
        .accounts
        .iter()
        .find(|account| account.inventory.needs_reconnect)
        .unwrap();
    let drafts_id = imap
        .folders
        .iter()
        .find(|folder| {
            folder
                .inventory
                .hierarchy
                .last()
                .is_some_and(|name| name == "Drafts")
        })
        .unwrap()
        .inventory
        .source_id
        .clone();
    selection
        .accounts
        .iter_mut()
        .find(|account| account.source_id == imap.inventory.source_id)
        .unwrap()
        .folders
        .retain(|id| id != &drafts_id);
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    let target = tempfile::tempdir().unwrap();
    let receipt = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    let imported = receipt
        .accounts
        .iter()
        .find(|account| account.needs_reconnect)
        .unwrap();
    assert!(
        !imported
            .folders
            .iter()
            .any(|folder| folder.source_id == drafts_id)
    );
    let cache = target.path().join(".cache").join(MAIL_APP).join("mail");
    assert!(!text_under(&cache).contains("Excluded draft"));
    let retained = target
        .path()
        .join(".local/share/tilo/mail-import/thunderbird");
    assert!(!text_under(&retained).contains("Excluded draft"));
    assert_eq!(
        std::fs::read(&drafts).unwrap(),
        original,
        "the excluded original is untouched"
    );
}

#[test]
fn folder_labels_keep_the_whole_thunderbird_hierarchy() {
    let home = profile();
    let root = home.path().join(".thunderbird/abc.default");
    write(
        &root.join("Mail/Local Folders/Projects.sbd/2024.sbd/Q3"),
        message("q3", "Q3 mail", "0001", false),
    );
    let inventory = discovered(&home);
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    let target = tempfile::tempdir().unwrap();
    let receipt = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    let local = local_account(&receipt).clone();
    let kept = read_kept(&cache_path_for(
        target.path(),
        &cache_stem(target.path(), &local),
    ));
    assert!(kept.messages.iter().any(|message| {
        message
            .labels
            .iter()
            .any(|label| label == "Thunderbird/Projects/2024/Q3")
    }));
}

#[test]
fn a_conflicting_attachment_is_refused_and_the_import_can_be_retried() {
    let (_home, inventory) = fixture();
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let key = key();
    let sealed = seal_selected(&reviewed, &selection, &key, BINDING).unwrap();
    let target = tempfile::tempdir().unwrap();
    let files = target
        .path()
        .join(".cache")
        .join(MAIL_APP)
        .join("mail")
        .join("person@example.test.files");
    std::fs::create_dir_all(&files).unwrap();
    let attachment = files.join(format!("{}.0", stable_message_id("one@example.test")));
    std::fs::write(&attachment, b"someone else's bytes").unwrap();
    assert_eq!(
        import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap_err(),
        ConnectionError::AlreadyPresent
    );
    assert!(
        !accounts::store_path_in(&target.path().join(".config/tilo")).exists(),
        "a conflicting attachment stops the import before any account is published"
    );
    std::fs::remove_file(&attachment).unwrap();
    let receipt = import_selected(target.path(), &sealed, &key, BINDING, &selection).unwrap();
    assert!(
        receipt
            .accounts
            .iter()
            .find(|account| account.needs_reconnect)
            .is_some_and(|account| account.created)
    );
    assert_eq!(std::fs::read(&attachment).unwrap(), b"%PDF-1.7");
}
