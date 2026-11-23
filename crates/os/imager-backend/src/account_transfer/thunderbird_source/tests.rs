//! A Thunderbird profile's mail carried to a new home: sealed into a password artifact and
//! imported from it here, streamed straight from the granted home in `stream/tests.rs`. Both read
//! the one profile [`profile`] writes.

use super::*;
use crate::account_sources::SourceRoots;

pub(super) const BODY: usize = 64;

pub(super) fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// One message with the flags the mbox itself carries, so the import has to read them rather than
/// guess.
pub(super) fn message(
    id: &str,
    subject: &str,
    status: &str,
    body: &[u8],
    attachment: bool,
) -> Vec<u8> {
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

const PREFS: &str = r#"user_pref("mail.accountmanager.accounts", "account1");
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
"#;

/// A home with one Thunderbird profile: an IMAP account whose Inbox holds `count` messages of
/// `body` bytes ("Message 0" with an attachment when there are several, "Message 1" read and
/// starred, "Message 2" unread, the rest read) and whose Drafts holds "Excluded draft", and Local
/// Folders holding "Local only" in its Inbox and "Project mail" in Projects/2024.
pub(super) fn profile(count: usize, body: usize) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".thunderbird/abc.default");
    write(
        &home.path().join(".thunderbird/profiles.ini"),
        "[Profile0]\nName=Personal\nIsRelative=1\nPath=abc.default\n",
    );
    write(&root.join("prefs.js"), PREFS);
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
    home
}

/// The profile as the migration's review of `home` found it.
pub(super) fn reviewed(home: &tempfile::TempDir) -> ReviewedThunderbird {
    let inventory = crate::account_sources::discover(&SourceRoots {
        home: home.path().into(),
        config: home.path().join(".config"),
    });
    assert!(inventory.unreadable_sources.is_empty());
    inventory.thunderbird_reviewed[0].clone()
}

/// The kept-mail file the Mail app reads for a receipt's account. Receipts carry no address, so
/// the canonical store is what names the account.
pub(super) fn cache_of(target: &Path, receipt: &ThunderbirdAccountReceipt) -> PathBuf {
    let store = Accounts::read(&target.join(".config/tilo")).unwrap();
    let all = store.all().unwrap();
    let account = all
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

pub(super) fn kept(target: &Path, receipt: &ThunderbirdAccountReceipt) -> Kept {
    serde_json::from_slice(&std::fs::read(cache_of(target, receipt)).unwrap()).unwrap()
}

/// The IMAP account, whose password the person gives again.
pub(super) fn imap(receipt: &ThunderbirdImportReceipt) -> &ThunderbirdAccountReceipt {
    receipt
        .accounts
        .iter()
        .find(|account| account.needs_reconnect)
        .unwrap()
}

/// The account whose mail is only on this computer (Local Folders).
pub(super) fn local(receipt: &ThunderbirdImportReceipt) -> &ThunderbirdAccountReceipt {
    receipt
        .accounts
        .iter()
        .find(|account| !account.needs_reconnect)
        .unwrap()
}

pub(super) fn text_under(path: &Path) -> String {
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

/// A mailbox the person already had at the destination, under the address the profile's IMAP
/// account has, with a field of its own no import knows.
pub(super) fn existing_mailbox(target: &Path) -> Account {
    let record = Accounts::empty();
    let mut existing = Account::new(AccountId::new("person@example.test"), "email");
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
        "owner_extension": "preserve-me",
    });
    record.keep(&existing).unwrap();
    record.project(&target.join(".config/tilo")).unwrap();
    existing
}

/// The Drafts folder of the profile's IMAP account, left out of `selection`; returns its id.
pub(super) fn leave_out_drafts(
    reviewed: &ReviewedThunderbird,
    selection: &mut ThunderbirdSelection,
) -> String {
    let imap = reviewed
        .accounts
        .iter()
        .find(|account| account.inventory.needs_reconnect)
        .unwrap();
    let drafts = imap
        .folders
        .iter()
        .find(|folder| folder.inventory.hierarchy == ["Drafts"])
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
        .retain(|id| id != &drafts);
    drafts
}

/// Mirrors `crates/integrations/mail-engine/src/model.rs` (`Message`, `Addr`, `Attachment`, `Folder`,
/// `ServerPlace`) and the `Kept` envelope in `crates/integrations/mail-engine/src/store.rs`.
/// `deny_unknown_fields` makes import fail this test if it writes anything the
/// Mail app cannot deserialize. The unused variants keep the mirror exact.
#[expect(
    dead_code,
    reason = "The deserialization mirror validates the complete Mail cache schema, including fields this test does not inspect."
)]
pub(super) mod mail_app {
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

/// A fresh migration password; the artifact is written to a directory of its own that lives as
/// long as the sealed selection does.
fn key() -> Vec<u8> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).unwrap();
    format!("test-password-{}", hex::encode(random)).into_bytes()
}

type Sealed = (tempfile::TempDir, PasswordSealedThunderbird);

const BINDING: &[u8] = b"operation-7/target-user-1000";

fn seal(
    reviewed: &ReviewedThunderbird,
    selection: &ThunderbirdSelection,
    password: &[u8],
) -> Sealed {
    let directory = tempfile::tempdir().unwrap();
    let artifact = PasswordSealedThunderbird::seal(
        reviewed,
        selection,
        password,
        BINDING,
        &std::sync::atomic::AtomicBool::new(false),
        &directory.path().join("mail.sealed"),
    )
    .unwrap();
    (directory, artifact)
}

fn import_sealed(
    target: &Path,
    sealed: &Sealed,
    password: &[u8],
    binding: &[u8],
    selection: &ThunderbirdSelection,
) -> std::io::Result<ThunderbirdImportReceipt> {
    sealed.1.import(
        target,
        password,
        binding,
        selection,
        &std::sync::atomic::AtomicBool::new(false),
    )
}

/// The person's whole profile, sealed on the old computer and imported on the new one: nothing
/// opens it but its password, its operation and its selection; the accounts and their mail land
/// where Mail reads them, with flags, attachments and the folder hierarchy; and importing again
/// merges into what the person has done since without adding anything twice.
#[test]
fn sealed_thunderbird_mail_imports_whole_and_merges_again_without_loss() {
    let home = profile(2, BODY);
    let profile_root = home.path().join(".thunderbird/abc.default");
    write(
        &profile_root.join("Mail/Local Folders/Projects.sbd/2024.sbd/Q3"),
        message("q3", "Q3 mail", "0001", b"q3", false),
    );
    let source = profile_root.join("ImapMail/imap.example.test/Inbox");
    let original = std::fs::read(&source).unwrap();
    let reviewed = reviewed(&home);
    let selection = ThunderbirdSelection::all(&reviewed);
    let password = key();
    let sealed = seal(&reviewed, &selection, &password);
    assert!(
        !String::from_utf8_lossy(&std::fs::read(sealed.0.path().join("mail.sealed")).unwrap())
            .contains("person@example.test")
    );

    let target = tempfile::tempdir().unwrap();
    assert_eq!(
        import_sealed(target.path(), &sealed, &key(), BINDING, &selection)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData,
        "another password does not open the artifact"
    );
    assert!(
        import_sealed(
            target.path(),
            &sealed,
            &password,
            b"another-operation",
            &selection
        )
        .is_err(),
        "another operation's binding does not open the artifact"
    );
    let mut widened = selection.clone();
    widened.accounts[0].folders.push("not-a-folder".into());
    assert!(
        import_sealed(target.path(), &sealed, &password, BINDING, &widened).is_err(),
        "a widened selection is refused"
    );
    assert!(
        !target
            .path()
            .join(".config/tilo/accounts/store.json")
            .exists()
    );

    let receipt = import_sealed(target.path(), &sealed, &password, BINDING, &selection).unwrap();
    assert_eq!(receipt.accounts.len(), 2);
    let imap = imap(&receipt);
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
    let store = Accounts::read(&root).unwrap();
    let account = store
        .get("person@example.test")
        .unwrap()
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
    let metadata =
        String::from_utf8_lossy(&std::fs::read(accounts::person::staged_path_in(&root)).unwrap())
            .into_owned();
    assert!(!metadata.contains("password"));
    assert!(!metadata.contains("launch_a_program"));

    // The kept file is exactly what the Mail app reads.
    let path = cache_of(target.path(), imap);
    assert_eq!(
        path,
        target
            .path()
            .join(".cache/so.tilo.mail/mail/person@example.test.json")
    );
    let app: mail_app::Kept = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(app.version, 1);
    assert_eq!(app.messages.len(), 3, "the Inbox and the Drafts");
    let flagged = app
        .messages
        .iter()
        .find(|message| message.subject == "Message 1")
        .unwrap();
    assert!(flagged.read && flagged.starred);
    assert_eq!(flagged.folder, mail_app::Folder::Inbox);
    assert_eq!(flagged.from.email, "ada@example.test");
    assert_eq!(flagged.account, imap.source_id);
    let attached = app
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

    let local = local(&receipt).clone();
    let local_path = cache_of(target.path(), &local);
    let local_kept = kept(target.path(), &local);
    assert_eq!(local_kept.messages.len(), 3);
    for folder in ["Thunderbird/Projects/2024", "Thunderbird/Projects/2024/Q3"] {
        assert!(
            local_kept
                .messages
                .iter()
                .any(|message| message.labels.iter().any(|label| label == folder)),
            "the whole hierarchy is kept: {folder}"
        );
    }
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

    // What the person did with the imported mail since, and what they wrote here, outlasts
    // importing the same artifact again.
    let mut changed = local_kept;
    let one = changed
        .messages
        .iter_mut()
        .find(|message| message.subject == "Local only")
        .expect("the local-only message is kept");
    one.read = true;
    one.starred = true;
    one.folder = CachedFolder::Trash;
    one.labels = vec!["keep-me".into()];
    let mut foreign = changed
        .messages
        .iter()
        .find(|message| message.subject == "Project mail")
        .unwrap()
        .clone();
    foreign.id = uuid::Uuid::new_v4();
    foreign.subject = "Written here".into();
    foreign.rfc_message_id = None;
    foreign.attachments = Vec::new();
    changed.messages.push(foreign);
    std::fs::write(&local_path, serde_json::to_vec(&changed).unwrap()).unwrap();

    let again = import_sealed(target.path(), &sealed, &password, BINDING, &selection).unwrap();
    assert!(again.accounts.iter().all(|account| !account.created));
    assert_eq!(Accounts::read(&root).unwrap().all().unwrap().len(), 2);
    let merged = kept(target.path(), &local);
    assert_eq!(
        merged.messages.len(),
        4,
        "the imported copy is not added twice"
    );
    assert_eq!(
        merged
            .messages
            .iter()
            .filter(|message| message.subject == "Local only")
            .count(),
        1
    );
    let one = merged
        .messages
        .iter()
        .find(|message| message.subject == "Local only")
        .unwrap();
    assert!(one.read && one.starred);
    assert_eq!(one.folder, CachedFolder::Trash);
    assert_eq!(one.labels, ["keep-me"]);
    assert!(
        merged
            .messages
            .iter()
            .any(|message| message.subject == "Written here")
    );
}

/// The person already reads this address in Mail and left the drafts behind: the existing mailbox
/// keeps how it signs in and what else it holds and gains the imported mail, and the folder left
/// out reaches neither the cache nor the kept copy.
#[test]
fn sealed_thunderbird_mail_joins_an_existing_mailbox_and_leaves_out_what_was_not_selected() {
    let home = profile(2, BODY);
    let drafts_file = home
        .path()
        .join(".thunderbird/abc.default/ImapMail/imap.example.test/Drafts");
    let drafts_original = std::fs::read(&drafts_file).unwrap();
    let reviewed = reviewed(&home);
    let mut selection = ThunderbirdSelection::all(&reviewed);
    let drafts = leave_out_drafts(&reviewed, &mut selection);
    let password = key();
    let sealed = seal(&reviewed, &selection, &password);
    let target = tempfile::tempdir().unwrap();
    let existing = existing_mailbox(target.path());

    let receipt = import_sealed(target.path(), &sealed, &password, BINDING, &selection).unwrap();
    let imported = imap(&receipt);
    assert!(!imported.created);
    let store = Accounts::read(&target.path().join(".config/tilo")).unwrap();
    let account = store
        .get(existing.id.as_str())
        .unwrap()
        .expect("the existing mailbox");
    assert_eq!(account.type_id.as_str(), "email");
    assert_eq!(account.config["imap_host"], "mail.example.test");
    assert_eq!(account.config["auth"], "OAuth");
    assert_eq!(account.config["owner_extension"], "preserve-me");
    assert_eq!(
        account.config["source_provenance"]["mail_client"],
        "thunderbird"
    );
    assert_eq!(
        kept(target.path(), imported).messages.len(),
        2,
        "the Inbox's messages still arrive"
    );

    assert!(
        !imported
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
    assert_eq!(
        std::fs::read(&drafts_file).unwrap(),
        drafts_original,
        "the excluded original is untouched"
    );
}

#[test]
fn an_account_with_no_folder_selected_brings_none_of_its_mail() {
    let home = profile(2, BODY);
    let reviewed = reviewed(&home);
    let mut selection = ThunderbirdSelection::all(&reviewed);
    let imap_id = &reviewed
        .accounts
        .iter()
        .find(|account| account.inventory.needs_reconnect)
        .unwrap()
        .inventory
        .source_id;
    selection
        .accounts
        .iter_mut()
        .find(|account| &account.source_id == imap_id)
        .unwrap()
        .folders
        .clear();
    let password = key();
    let sealed = seal(&reviewed, &selection, &password);
    let target = tempfile::tempdir().unwrap();
    let receipt = import_sealed(target.path(), &sealed, &password, BINDING, &selection).unwrap();
    assert!(imap(&receipt).folders.is_empty());
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

/// Kept mail the import cannot merge, an unreadable file or an attachment with other bytes, stops
/// it before any account is published and is left for its owner; once it is gone the same import
/// completes, and a retry that inserts nothing rewrites nothing.
#[test]
fn an_import_that_cannot_merge_publishes_nothing_and_a_retry_completes_it() {
    let home = profile(2, BODY);
    let reviewed = reviewed(&home);
    let selection = ThunderbirdSelection::all(&reviewed);
    let password = key();
    let sealed = seal(&reviewed, &selection, &password);
    // Source IDs are deterministic, so one target teaches the other its file.
    let learned = tempfile::tempdir().unwrap();
    let first = import_sealed(learned.path(), &sealed, &password, BINDING, &selection).unwrap();
    let local_file = cache_of(learned.path(), local(&first))
        .strip_prefix(learned.path())
        .unwrap()
        .to_owned();

    let target = tempfile::tempdir().unwrap();
    let store = accounts::person::staged_path_in(&target.path().join(".config/tilo"));
    let corrupt = target.path().join(&local_file);
    write(&corrupt, b"{not json");
    assert!(
        import_sealed(target.path(), &sealed, &password, BINDING, &selection).is_err(),
        "unreadable kept mail is refused"
    );
    assert!(
        !store.exists(),
        "an account is not published while its mail is only half written"
    );
    assert_eq!(
        std::fs::read(&corrupt).unwrap(),
        b"{not json",
        "an unreadable file is left for its owner instead of being overwritten"
    );
    std::fs::remove_file(&corrupt).unwrap();

    let attachment = target
        .path()
        .join(".cache")
        .join(MAIL_APP)
        .join("mail")
        .join("person@example.test.files")
        .join(format!("{}.0", stable_message_id("one-0@example.test")));
    write(&attachment, b"someone else's bytes");
    assert!(
        import_sealed(target.path(), &sealed, &password, BINDING, &selection).is_err(),
        "an attachment that differs is refused"
    );
    assert!(
        !store.exists(),
        "a conflicting attachment stops the import before any account is published"
    );
    std::fs::remove_file(&attachment).unwrap();

    let receipt = import_sealed(target.path(), &sealed, &password, BINDING, &selection).unwrap();
    assert_eq!(receipt.accounts.len(), 2);
    assert!(imap(&receipt).created);
    assert_eq!(std::fs::read(&attachment).unwrap(), b"%PDF-1.7");
    assert_eq!(kept(target.path(), local(&receipt)).messages.len(), 2);
    assert_eq!(
        Accounts::read(&target.path().join(".config/tilo"))
            .unwrap()
            .all()
            .unwrap()
            .len(),
        2
    );
    let before = std::fs::read(target.path().join(&local_file)).unwrap();
    let third = import_sealed(target.path(), &sealed, &password, BINDING, &selection).unwrap();
    assert!(third.accounts.iter().all(|account| !account.created));
    assert_eq!(
        std::fs::read(target.path().join(&local_file)).unwrap(),
        before,
        "a retry that inserts nothing rewrites nothing"
    );
}
