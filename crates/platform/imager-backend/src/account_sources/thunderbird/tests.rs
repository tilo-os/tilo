use super::*;
use std::path::PathBuf;

const PRIVATE_SENTINEL: &str = "private-account-sentinel@example.test";

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

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

pub(super) fn fixture() -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".thunderbird");
    std::fs::create_dir_all(root.join("abc.default/ImapMail/imap.example.test/Archive/cur"))
        .unwrap();
    std::fs::create_dir_all(root.join("abc.default/ImapMail/imap.example.test/Archive/new"))
        .unwrap();
    std::fs::create_dir_all(root.join("abc.default/ImapMail/imap.example.test/Archive/tmp"))
        .unwrap();
    std::fs::create_dir_all(root.join("abc.default/Mail/Local Folders/Projects.sbd")).unwrap();
    std::fs::create_dir_all(root.join("abc.default/Mail/pop.example.test")).unwrap();
    std::fs::write(
        root.join("profiles.ini"),
        "[Profile0]\nName=Personal\nIsRelative=1\nPath=abc.default\nDefault=1\n",
    )
    .unwrap();
    let prefs = format!(
        r#"user_pref("mail.accountmanager.accounts", "account1,account2");
user_pref("mail.accountmanager.localfoldersserver", "server3");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.account.account2.server", "server2");
user_pref("mail.account.account2.identities", "id2");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.hostname", "imap.example.test");
user_pref("mail.server.server1.port", 993);
user_pref("mail.server.server1.userName", "person@example.test");
user_pref("mail.server.server1.socketType", 3);
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.test");
user_pref("mail.server.server2.type", "pop3");
user_pref("mail.server.server2.hostname", "pop.example.test");
user_pref("mail.server.server2.directory-rel", "[ProfD]Mail/pop.example.test");
user_pref("mail.server.server3.type", "none");
user_pref("mail.server.server3.directory-rel", "[ProfD]Mail/Local Folders");
user_pref("mail.identity.id1.useremail", "{PRIVATE_SENTINEL}");
user_pref("mail.identity.id1.fullName", "Personal");
user_pref("mail.identity.id1.smtpServer", "smtp1");
user_pref("mail.smtpserver.smtp1.hostname", "smtp.example.test");
user_pref("mail.smtpserver.smtp1.port", 465);
user_pref("mail.smtpserver.smtp1.username", "person@example.test");
user_pref("mail.smtpserver.smtp1.try_ssl", 3);
user_pref("ldap_2.servers.personal.description", "Personal address book");
user_pref("ldap_2.servers.personal.filename", "abook.sqlite");
user_pref("calendar.registry.work.name", "Work calendar");
user_pref("calendar.registry.work.type", "storage");
user_pref("calendar.registry.work.filename", "calendar.ics");
user_pref("mail.server.server1.password", launch_a_program());
"#
    );
    std::fs::write(root.join("abc.default/prefs.js"), prefs).unwrap();
    let profile = root.join("abc.default");
    write(
        &profile.join("ImapMail/imap.example.test/Inbox"),
        format!(
            "{}{}",
            message("one", "One", "0001", true),
            message("two", "Two", "0004", false)
        ),
    );
    write(
        &profile.join("Mail/Local Folders/Inbox"),
        message("local", "Local only", "0000", false),
    );
    write(
        &profile.join("Mail/Local Folders/Projects.sbd/2024"),
        message("project", "Project mail", "0001", false),
    );
    write(
        &profile.join("Mail/pop.example.test/Sent"),
        message("sent", "Sent mail", "0001", false),
    );
    write(
        &profile.join("ImapMail/imap.example.test/Archive/cur/one:2,S"),
        "From: Ada <ada@example.test>\r\nSubject: Archived\r\nMessage-ID: <archive-one@example.test>\r\n\r\nArchived body.\r\n",
    );
    write(
        &profile.join("ImapMail/imap.example.test/Archive/new/two"),
        "From: Ada <ada@example.test>\r\nSubject: New archive\r\nMessage-ID: <archive-two@example.test>\r\n\r\nUnread archive.\r\n",
    );
    write(&profile.join("abook.sqlite"), b"fixture address book");
    write(
        &profile.join("calendar.ics"),
        b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n",
    );
    (home, root)
}

#[test]
fn discovers_accounts_local_folders_hierarchy_counts_flags_and_attachments() {
    let (home, _) = fixture();
    let inventory = super::super::discover(&super::super::SourceRoots {
        home: home.path().into(),
        config: home.path().join(".config"),
    });
    assert!(inventory.unreadable_sources.is_empty());
    assert_eq!(inventory.thunderbird.len(), 1);
    let profile = &inventory.thunderbird[0];
    assert_eq!(profile.accounts.len(), 3);
    let imap = profile
        .accounts
        .iter()
        .find(|account| account.kind == "imap")
        .unwrap();
    assert!(imap.local_mail);
    assert!(imap.needs_reconnect);
    let inbox = imap
        .folders
        .iter()
        .find(|folder| folder.hierarchy == ["Inbox"])
        .unwrap();
    assert_eq!(inbox.format, "mbox");
    assert_eq!(inbox.messages, Some(2));
    assert_eq!(inbox.unread, Some(1));
    assert_eq!(inbox.flagged, Some(1));
    assert_eq!(inbox.attachments, Some(1));
    assert!(inbox.counted);
    let archive = imap
        .folders
        .iter()
        .find(|folder| folder.hierarchy == ["Archive"])
        .unwrap();
    assert_eq!(archive.format, "maildir");
    assert_eq!(archive.messages, Some(2));
    assert_eq!(archive.unread, Some(1));
    let local = profile
        .accounts
        .iter()
        .find(|account| account.kind == "local")
        .unwrap();
    assert!(
        local
            .folders
            .iter()
            .any(|folder| folder.hierarchy == ["Projects", "2024"]
                && folder.local_only
                && folder.messages == Some(1))
    );
    assert!(
        profile
            .address_books
            .iter()
            .any(|book| book.present && book.local)
    );
    assert!(
        profile
            .calendars
            .iter()
            .any(|calendar| calendar.present && calendar.local)
    );
    let serialized = serde_json::to_string(&inventory).unwrap();
    assert!(!serialized.contains(&home.path().to_string_lossy().to_string()));
    assert!(!serialized.contains("launch_a_program"));
}

#[cfg(unix)]
#[test]
fn redirected_profile_files_are_refused_without_reading_outside_the_grant() {
    use std::os::unix::fs::symlink;
    let (home, root) = fixture();
    let outside = tempfile::tempdir().unwrap();
    let prefs = root.join("abc.default/prefs.js");
    std::fs::remove_file(&prefs).unwrap();
    symlink(outside.path().join("prefs.js"), &prefs).unwrap();
    std::fs::write(
        outside.path().join("prefs.js"),
        "user_pref(\"mail.accountmanager.accounts\", \"account1\");",
    )
    .unwrap();
    let inventory = super::super::discover(&super::super::SourceRoots {
        home: home.path().into(),
        config: home.path().join(".config"),
    });
    assert!(inventory.thunderbird.is_empty());
    assert!(
        inventory
            .unreadable_sources
            .contains(&"Thunderbird profile")
    );
}

#[cfg(unix)]
#[test]
fn redirected_folder_file_is_not_counted_or_followed() {
    use std::os::unix::fs::symlink;
    let (home, root) = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("Inbox"),
        message("outside", "Outside", "0001", false),
    )
    .unwrap();
    let inbox = root.join("abc.default/Mail/Local Folders/Inbox");
    std::fs::remove_file(&inbox).unwrap();
    symlink(outside.path().join("Inbox"), &inbox).unwrap();
    let inventory = super::super::discover(&super::super::SourceRoots {
        home: home.path().into(),
        config: home.path().join(".config"),
    });
    let profile = &inventory.thunderbird[0];
    let local = profile
        .accounts
        .iter()
        .find(|account| account.kind == "local")
        .unwrap();
    assert!(
        !local
            .folders
            .iter()
            .any(|folder| folder.hierarchy == ["Inbox"])
    );
}
