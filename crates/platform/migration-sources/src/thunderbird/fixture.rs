//! A realistic Thunderbird profile for proofs and tests.
//!
//! It is written the way Thunderbird lays a profile out: `profiles.ini`, a
//! profile directory with `prefs.js`, mbox mail under `ImapMail/<host>`, an
//! `SQLite` address book and an `SQLite` local calendar under `calendar-data`.

use std::path::Path;

pub const WORK_EMAIL: &str = "ada@example.com";
pub const HOME_EMAIL: &str = "ada.home@example.org";
pub const WORK_HOST: &str = "imap.example.com";
pub const HOME_HOST: &str = "imap.example.org";

/// Write one two-account profile below `home`:
///
/// - `ada@example.com` "Work" and `ada.home@example.org` "Home", both IMAP,
///   each with an Inbox and Sent folder (3 and 1 messages, and 2 and 1);
/// - one address book with two contacts;
/// - one local calendar with two events.
///
/// # Panics
/// A file the fixture writes could not be created.
#[expect(
    clippy::too_many_lines,
    reason = "the fixture writes one profile's whole layout in one place"
)]
#[expect(
    clippy::expect_used,
    reason = "a fixture that cannot write its files panics with the path"
)]
pub fn write_sample_profile(home: &Path) {
    let root = home.join(".thunderbird");
    let profile = root.join("fx8r2l9p.default-release");
    std::fs::create_dir_all(&root).expect("create Thunderbird root");
    std::fs::create_dir_all(&profile).expect("create profile directory");
    std::fs::write(
        root.join("profiles.ini"),
        "[Profile0]\nName=Default\nIsRelative=1\nPath=fx8r2l9p.default-release\nDefault=1\n",
    )
    .expect("write profiles.ini");
    std::fs::write(profile.join("prefs.js"), prefs()).expect("write prefs.js");

    write(
        &profile,
        &format!("ImapMail/{WORK_HOST}/INBOX"),
        [
            message(
                "work-1",
                WORK_EMAIL,
                "Welcome to Work",
                "0001",
                "The first message in the Work inbox.\r\n",
            ),
            message(
                "work-2",
                WORK_EMAIL,
                "Design review",
                "0000",
                "Can you review the draft on Thursday?\r\n",
            ),
            message(
                "work-3",
                WORK_EMAIL,
                "Invoice",
                "0005",
                "The invoice is attached.\r\n",
            ),
        ]
        .concat(),
    );
    write(
        &profile,
        &format!("ImapMail/{WORK_HOST}/Sent"),
        message(
            "work-sent-1",
            WORK_EMAIL,
            "Re: Welcome to Work",
            "0001",
            "Thanks, good to be here.\r\n",
        ),
    );
    // Thunderbird keeps an index beside every mbox; it is not mail.
    write(
        &profile,
        &format!("ImapMail/{WORK_HOST}/INBOX.msf"),
        "index",
    );
    write(
        &profile,
        &format!("ImapMail/{HOME_HOST}/INBOX"),
        [
            message(
                "home-1",
                HOME_EMAIL,
                "Weekend plans",
                "0000",
                "Shall we go to the coast?\r\n",
            ),
            message(
                "home-2",
                HOME_EMAIL,
                "Garden club",
                "0001",
                "The meeting moved to Tuesday.\r\n",
            ),
        ]
        .concat(),
    );
    write(
        &profile,
        &format!("ImapMail/{HOME_HOST}/Sent"),
        message(
            "home-sent-1",
            HOME_EMAIL,
            "Re: Weekend plans",
            "0001",
            "Yes, let's.\r\n",
        ),
    );

    let connection =
        rusqlite::Connection::open(profile.join("abook.sqlite")).expect("create address book");
    connection
        .execute_batch(
            "CREATE TABLE cards (uid TEXT PRIMARY KEY, card TEXT);
             INSERT INTO cards VALUES ('ada-lovelace',
                 'BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada-lovelace\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nORG:Analytical Engines\r\nTITLE:Mathematician\r\nTEL;TYPE=cell:+44 20 7946 0101\r\nEMAIL;TYPE=work:ada@example.com\r\nADR;TYPE=home:;;12 St James''s Square;London;Greater London;SW1Y 4JH;United Kingdom\r\nBDAY:1815-12-10\r\nURL:https://example.com/ada\r\nNOTE:Wrote the first algorithm.\r\nCATEGORIES:Friends\r\nEND:VCARD\r\n');
             INSERT INTO cards VALUES ('grace-hopper',
                 'BEGIN:VCARD\r\nVERSION:3.0\r\nUID:grace-hopper\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nORG:United States Navy\r\nTITLE:Rear Admiral\r\nEMAIL;TYPE=work:grace@example.org\r\nEND:VCARD\r\n');",
        )
        .expect("write address book cards");
    drop(connection);

    let calendar = profile.join("calendar-data");
    std::fs::create_dir_all(&calendar).expect("create calendar directory");
    let connection =
        rusqlite::Connection::open(calendar.join("local.sqlite")).expect("create calendar");
    connection
        .execute_batch(
            "CREATE TABLE cal_events (cal_id TEXT, id TEXT, timestamp INTEGER, icalString TEXT, PRIMARY KEY (cal_id, id));
             INSERT INTO cal_events VALUES ('work', 'event-timed', 1760000000,
                 'BEGIN:VEVENT\r\nUID:review@example.com\r\nSUMMARY:Design review\r\nLOCATION:Meeting room\r\nDTSTART;TZID=Europe/Berlin:20261105T090000\r\nDTEND;TZID=Europe/Berlin:20261105T100000\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEND:VEVENT');
             INSERT INTO cal_events VALUES ('work', 'event-all-day', 1760000000,
                 'BEGIN:VEVENT\r\nUID:holiday@example.com\r\nSUMMARY:Company holiday\r\nDTSTART;VALUE=DATE:20261224\r\nDTEND;VALUE=DATE:20261227\r\nEND:VEVENT');",
        )
        .expect("write calendar events");
}

#[expect(
    clippy::expect_used,
    reason = "a fixture that cannot write its files panics with the path"
)]
fn write(profile: &Path, relative: &str, bytes: impl AsRef<[u8]>) {
    let path = profile.join(relative);
    std::fs::create_dir_all(path.parent().expect("fixture file has a parent"))
        .expect("create mail directory");
    std::fs::write(path, bytes).expect("write fixture file");
}

fn prefs() -> String {
    let mut prefs = String::new();
    prefs.push_str("// Mozilla User Preferences\n\n");
    prefs.push_str(
        r##"user_pref("mail.accountmanager.accounts", "account1,account2");
user_pref("mail.accountmanager.defaultaccount", "account1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.account.account2.server", "server2");
user_pref("mail.account.account2.identities", "id2,id3");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.hostname", "imap.example.com");
user_pref("mail.server.server1.port", 993);
user_pref("mail.server.server1.userName", "ada@example.com");
user_pref("mail.server.server1.socketType", 3);
user_pref("mail.server.server1.authMethod", 3);
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.com");
user_pref("mail.server.server1.name", "Work");
user_pref("mail.server.server2.type", "imap");
user_pref("mail.server.server2.hostname", "imap.example.org");
user_pref("mail.server.server2.port", 993);
user_pref("mail.server.server2.userName", "ada.home@example.org");
user_pref("mail.server.server2.socketType", 3);
user_pref("mail.server.server2.authMethod", 3);
user_pref("mail.server.server2.directory-rel", "[ProfD]ImapMail/imap.example.org");
user_pref("mail.server.server2.name", "Home");
user_pref("mail.identity.id1.useremail", "ada@example.com");
user_pref("mail.identity.id1.fullName", "Work");
user_pref("mail.identity.id1.smtpServer", "smtp1");
user_pref("mail.identity.id2.useremail", "ada.home@example.org");
user_pref("mail.identity.id2.fullName", "Home");
user_pref("mail.identity.id2.smtpServer", "smtp2");
user_pref("mail.identity.id3.useremail", "ada+lists@example.org");
user_pref("mail.identity.id3.fullName", "Ada's lists");
user_pref("mail.identity.id3.smtpServer", "smtp2");
user_pref("mail.smtpserver.smtp1.hostname", "smtp.example.com");
user_pref("mail.smtpserver.smtp1.port", 465);
user_pref("mail.smtpserver.smtp1.username", "ada@example.com");
user_pref("mail.smtpserver.smtp1.try_ssl", 3);
user_pref("mail.smtpserver.smtp2.hostname", "smtp.example.org");
user_pref("mail.smtpserver.smtp2.port", 465);
user_pref("mail.smtpserver.smtp2.username", "ada.home@example.org");
user_pref("mail.smtpserver.smtp2.try_ssl", 3);
user_pref("ldap_2.servers.personal.description", "Personal address book");
user_pref("ldap_2.servers.personal.filename", "abook.sqlite");
user_pref("ldap_2.servers.personal.dirType", 2);
user_pref("calendar.registry.work.name", "Work");
user_pref("calendar.registry.work.type", "storage");
user_pref("calendar.registry.work.filename", "local.sqlite");
user_pref("calendar.registry.work.color", "#3a87ad");
"##,
    );
    prefs
}

fn message(id: &str, to: &str, subject: &str, status: &str, body: &str) -> String {
    format!(
        "From - Mon Jan 05 09:00:00 2026\r\nFrom: Ada Lovelace <{to}>\r\nTo: Ada Lovelace <{to}>\r\nSubject: {subject}\r\nDate: Mon, 05 Jan 2026 09:00:00 +0000\r\nMessage-ID: <{id}@example.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nX-Mozilla-Status: {status}\r\nX-Mozilla-Status2: 00000000\r\n\r\n{body}"
    )
}
