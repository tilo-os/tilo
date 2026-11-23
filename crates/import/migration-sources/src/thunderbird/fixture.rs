//! A realistic Thunderbird profile for proofs and tests.
//!
//! It is written the way Thunderbird lays a profile out: `profiles.ini`, a
//! profile directory with `prefs.js`, mbox mail under `ImapMail/<host>`, an
//! `SQLite` address book, an `SQLite` local calendar under `calendar-data`, and
//! the accounts' saved passwords in `logins.json` under the key `key4.db` keeps.

use std::path::Path;

pub const WORK_EMAIL: &str = "ada@example.com";
pub const HOME_EMAIL: &str = "ada.home@example.org";
pub const WORK_SERVER: &str = "imap.example.com";
pub const HOME_SERVER: &str = "imap.example.org";
/// The password Thunderbird saved for the work account's servers.
pub const WORK_PASSWORD: &str = "work-fixture-password";
/// The password Thunderbird saved for the home account's servers.
pub const HOME_PASSWORD: &str = "home-fixture-password";
/// The file the work Inbox's invoice carries.
pub const INVOICE_FILE: &str = "invoice-2026-01.pdf";
/// What that file holds.
pub const INVOICE_BYTES: &[u8] =
    b"%PDF-1.4\n% Sample invoice 2026-01: 1 analytical engine, 1 200 GBP\n%%EOF\n";

/// Write one two-account profile below `home`:
///
/// - `ada@example.com` "Work" and `ada.home@example.org` "Home", both IMAP,
///   each with an Inbox and Sent folder (3 and 1 messages, and 2 and 1), the
///   work Inbox's invoice an HTML message with a PDF attached;
/// - the passwords Thunderbird saved for both accounts' servers;
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
        &format!("ImapMail/{WORK_SERVER}/INBOX"),
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
            invoice(),
        ]
        .concat(),
    );
    write(
        &profile,
        &format!("ImapMail/{WORK_SERVER}/Sent"),
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
        &format!("ImapMail/{WORK_SERVER}/INBOX.msf"),
        "index",
    );
    write(
        &profile,
        &format!("ImapMail/{HOME_SERVER}/INBOX"),
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
        &format!("ImapMail/{HOME_SERVER}/Sent"),
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

    write_logins(
        &profile,
        &[
            (format!("imap://{WORK_SERVER}"), WORK_EMAIL, WORK_PASSWORD),
            (
                "smtp://smtp.example.com".to_owned(),
                WORK_EMAIL,
                WORK_PASSWORD,
            ),
            (format!("imap://{HOME_SERVER}"), HOME_EMAIL, HOME_PASSWORD),
            (
                "smtp://smtp.example.org".to_owned(),
                HOME_EMAIL,
                HOME_PASSWORD,
            ),
        ],
    );

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

/// The sample profile where Flatpak keeps Thunderbird: under the
/// application's `.var/app/org.mozilla.Thunderbird`, which is a link to the
/// package's own directory, as Flatpak leaves it when it names an app
/// directory after a package rather than its application id.
///
/// # Panics
/// The fixture's application directory or its link could not be created.
#[cfg(unix)]
#[expect(
    clippy::expect_used,
    reason = "a fixture that cannot write its files panics with the path"
)]
pub fn write_flatpak_profile(home: &Path) {
    let app = home.join(".var/app");
    let package = app.join("org.mozilla.thunderbird_esr");
    std::fs::create_dir_all(&package).expect("create the Flatpak application directory");
    std::os::unix::fs::symlink(
        "org.mozilla.thunderbird_esr",
        app.join("org.mozilla.Thunderbird"),
    )
    .expect("link the Flatpak application directory");
    write_sample_profile(&app.join("org.mozilla.Thunderbird"));
}

/// The sample profile as Thunderbird leaves it when the profile was copied in
/// from another Thunderbird location: `prefs.js` still names the old mail
/// directories, both absolutely and relative to that place, where nothing is
/// any more, while the mail itself lies in `ImapMail/` where Thunderbird lays
/// it out. The old paths stay stale, not wrong: reading the profile must still
/// find its mail, and must not follow a path out of the profile.
///
/// # Panics
/// The sample profile's `prefs.js` could not be read or written.
#[expect(
    clippy::expect_used,
    reason = "a fixture that cannot write its files panics with the path"
)]
pub fn write_stale_mail_directories(home: &Path) {
    use std::fmt::Write as _;

    let profile = home.join(".thunderbird/fx8r2l9p.default-release");
    let old = home.join("thunderbird-before-flatpak/fx8r2l9p.default-release");
    let prefs = profile.join("prefs.js");
    let mut text = std::fs::read_to_string(&prefs).expect("read the fixture's prefs.js");
    for (server, name) in [("server1", WORK_SERVER), ("server2", HOME_SERVER)] {
        let _ = writeln!(
            text,
            "user_pref(\"mail.server.{server}.directory\", \"{}/ImapMail/{name}\");",
            old.display()
        );
        let _ = writeln!(
            text,
            "user_pref(\"mail.server.{server}.directory-rel\", \"[ProfD]../../thunderbird-before-flatpak/fx8r2l9p.default-release/ImapMail/{name}\");"
        );
    }
    std::fs::write(&prefs, text).expect("write the fixture's prefs.js");
}

/// One IMAP account of a made-up profile whose servers are wherever a test starts them: what
/// [`write_live_profile`] writes.
#[derive(Clone, Debug)]
pub struct LiveAccount {
    pub name: String,
    pub email: String,
    /// Another address the account sends as, `Name <address>`'s parts.
    pub also_sends_as: Option<(String, String)>,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub password: String,
}

/// A profile with `account` alone, as Thunderbird keeps one that reads its mail from a server:
/// its servers and identities in `prefs.js`, its password in `logins.json` sealed by
/// `key4.db`, and one message of its Inbox in Thunderbird's cache. IMAP is TLS from the first
/// byte on every port but 143, and SMTP on 465 alone; the others upgrade with `STARTTLS`.
#[expect(
    clippy::expect_used,
    reason = "a fixture that cannot write its files panics with the path"
)]
pub fn write_live_profile(home: &Path, account: &LiveAccount) {
    let root = home.join(".thunderbird");
    let profile = root.join("l1v3l1v3.default-release");
    std::fs::create_dir_all(&profile).expect("create profile directory");
    std::fs::write(
        root.join("profiles.ini"),
        "[Profile0]\nName=Default\nIsRelative=1\nPath=l1v3l1v3.default-release\nDefault=1\n",
    )
    .expect("write profiles.ini");
    // 3 is TLS from the first byte, 2 is STARTTLS: IMAP is STARTTLS on 143 alone, SMTP is TLS
    // on 465 alone, as Mail reads an account's ports.
    let socket = |starttls: bool| if starttls { 2 } else { 3 };
    let mut prefs = format!(
        r#"// Mozilla User Preferences

user_pref("mail.accountmanager.accounts", "account1");
user_pref("mail.accountmanager.defaultaccount", "account1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account1.identities", "{identities}");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.hostname", "{imap_host}");
user_pref("mail.server.server1.port", {imap_port});
user_pref("mail.server.server1.userName", "{email}");
user_pref("mail.server.server1.socketType", {imap_socket});
user_pref("mail.server.server1.authMethod", 3);
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/{imap_host}");
user_pref("mail.server.server1.name", "{name}");
user_pref("mail.identity.id1.useremail", "{email}");
user_pref("mail.identity.id1.fullName", "{name}");
user_pref("mail.identity.id1.smtpServer", "smtp1");
user_pref("mail.smtpserver.smtp1.hostname", "{smtp_host}");
user_pref("mail.smtpserver.smtp1.port", {smtp_port});
user_pref("mail.smtpserver.smtp1.username", "{email}");
user_pref("mail.smtpserver.smtp1.try_ssl", {smtp_socket});
"#,
        identities = if account.also_sends_as.is_some() {
            "id1,id2"
        } else {
            "id1"
        },
        imap_host = account.imap_host,
        imap_port = account.imap_port,
        imap_socket = socket(account.imap_port == 143),
        smtp_host = account.smtp_host,
        smtp_port = account.smtp_port,
        smtp_socket = socket(account.smtp_port != 465),
        email = account.email,
        name = account.name,
    );
    if let Some((name, email)) = &account.also_sends_as {
        use std::fmt::Write as _;
        let _ = write!(
            prefs,
            "user_pref(\"mail.identity.id2.useremail\", \"{email}\");\n\
             user_pref(\"mail.identity.id2.fullName\", \"{name}\");\n\
             user_pref(\"mail.identity.id2.smtpServer\", \"smtp1\");\n"
        );
    }
    std::fs::write(profile.join("prefs.js"), prefs).expect("write prefs.js");
    write(
        &profile,
        &format!("ImapMail/{}/INBOX", account.imap_host),
        message(
            "cached-1",
            &account.email,
            "Kept on this computer",
            "0001",
            "A message Thunderbird keeps in its cache.\r\n",
        ),
    );
    write_logins(
        &profile,
        &[
            (
                format!("imap://{}", account.imap_host),
                &account.email,
                &account.password,
            ),
            (
                format!("smtp://{}", account.smtp_host),
                &account.email,
                &account.password,
            ),
        ],
    );
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

/// The work Inbox's invoice: text and HTML alternatives and a PDF attached,
/// as a mail program sends one.
fn invoice() -> String {
    use base64::Engine as _;
    let attachment = base64::engine::general_purpose::STANDARD.encode(INVOICE_BYTES);
    format!(
        "From - Mon Jan 05 09:00:00 2026\r\nFrom: Ada Lovelace <{WORK_EMAIL}>\r\nTo: Ada Lovelace <{WORK_EMAIL}>\r\nSubject: Invoice\r\nDate: Mon, 05 Jan 2026 09:00:00 +0000\r\nMessage-ID: <work-3@example.test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"outer\"\r\nX-Mozilla-Status: 0005\r\nX-Mozilla-Status2: 00000000\r\n\r\n--outer\r\nContent-Type: multipart/alternative; boundary=\"inner\"\r\n\r\n--inner\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe invoice is attached.\r\nTotal: 1 200 GBP\r\n--inner\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>The invoice is <b>attached</b>.</p><table><tr><td>Total</td><td><strong>1 200 GBP</strong></td></tr></table></body></html>\r\n--inner--\r\n--outer\r\nContent-Type: application/pdf; name=\"{INVOICE_FILE}\"\r\nContent-Disposition: attachment; filename=\"{INVOICE_FILE}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{attachment}\r\n--outer--\r\n"
    )
}

/// One PBES2 entry of `key4.db` sealing `plaintext` under `global_salt`
/// with no primary password, its entry salt filled with `salt`.
#[expect(
    clippy::expect_used,
    reason = "a fixture that cannot seal its entry panics"
)]
fn key4_entry(global_salt: &[u8], plaintext: &[u8], salt: u8) -> Vec<u8> {
    use tilo_seal::compat::nss_pbes2_ciphertext;
    let entry_salt = [salt; 32];
    let iv = [0x04, 0x0e, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3];
    let ciphertext = nss_pbes2_ciphertext(global_salt, b"", &entry_salt, 10_000, &iv, plaintext)
        .expect("seal a key4 entry");
    let hmac_sha256 = der(
        0x30,
        &der(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x02, 0x09]),
    );
    let pbkdf2 = der(
        0x30,
        &[
            der(
                0x06,
                &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x05, 0x0c],
            ),
            der(
                0x30,
                &[
                    der(0x04, &entry_salt),
                    der(0x02, &[0x27, 0x10]),
                    der(0x02, &[0x20]),
                    hmac_sha256,
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let cipher = der(
        0x30,
        &[
            der(
                0x06,
                &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x2a],
            ),
            der(0x04, iv.get(2..).unwrap_or_default()),
        ]
        .concat(),
    );
    der(
        0x30,
        &[
            der(
                0x30,
                &[
                    der(
                        0x06,
                        &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x05, 0x0d],
                    ),
                    der(0x30, &[pbkdf2, cipher].concat()),
                ]
                .concat(),
            ),
            der(0x04, &ciphertext),
        ]
        .concat(),
    )
}

/// One `logins.json` field sealing `plaintext` under the logins `key`.
#[expect(
    clippy::expect_used,
    reason = "a fixture that cannot seal its login panics"
)]
fn login_field(key: &[u8], plaintext: &str) -> String {
    use base64::Engine as _;
    use tilo_seal::compat::{NssCipher, nss_cbc_ciphertext};
    let iv = [7_u8; 8];
    let ciphertext = nss_cbc_ciphertext(NssCipher::TripleDesCbc, key, &iv, plaintext.as_bytes())
        .expect("seal a login");
    base64::engine::general_purpose::STANDARD.encode(der(
        0x30,
        &[
            der(0x04, &super::logins::LOGINS_KEY_ID),
            der(
                0x30,
                &[
                    der(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x03, 0x07]),
                    der(0x04, &iv),
                ]
                .concat(),
            ),
            der(0x04, &ciphertext),
        ]
        .concat(),
    ))
}

/// `logins.json` holding `logins` (origin, user name, password) under a key
/// `key4.db` keeps with no primary password, as Thunderbird writes them.
#[expect(
    clippy::expect_used,
    reason = "a fixture that cannot write its files panics with the path"
)]
fn write_logins(profile: &Path, logins: &[(String, &str, &str)]) {
    let global_salt = [0x11_u8; 20];
    let key = [0x5a_u8; 24];
    let entry = |plaintext: &[u8], salt: u8| key4_entry(&global_salt, plaintext, salt);
    write_key4(profile, &global_salt, &key, &entry);
    let field = |plaintext: &str| login_field(&key, plaintext);
    let entries: Vec<serde_json::Value> = logins
        .iter()
        .enumerate()
        .map(|(index, (origin, username, password))| {
            serde_json::json!({
                "id": index.saturating_add(1),
                "hostname": origin,
                "httpRealm": origin,
                "formSubmitURL": null,
                "usernameField": "",
                "passwordField": "",
                "encryptedUsername": field(username),
                "encryptedPassword": field(password),
                "encType": 1,
                "timesUsed": 1,
            })
        })
        .collect();
    std::fs::write(
        profile.join("logins.json"),
        serde_json::to_vec(
            &serde_json::json!({"nextId": logins.len().saturating_add(1), "logins": entries}),
        )
        .expect("encode logins.json"),
    )
    .expect("write logins.json");
}

/// `key4.db` keeping `key` under the password check, with no primary
/// password; `entry` seals one of its PBES2 entries.
#[expect(
    clippy::expect_used,
    reason = "a fixture that cannot write its files panics with the path"
)]
fn write_key4(
    profile: &Path,
    global_salt: &[u8; 20],
    key: &[u8; 24],
    entry: &dyn Fn(&[u8], u8) -> Vec<u8>,
) {
    let connection = rusqlite::Connection::open(profile.join("key4.db")).expect("create key4.db");
    connection
        .execute_batch(
            "CREATE TABLE metaData (id PRIMARY KEY UNIQUE ON CONFLICT REPLACE, item1, item2);
             CREATE TABLE nssPrivate (id PRIMARY KEY UNIQUE ON CONFLICT ABORT, a11, a102);",
        )
        .expect("create key4.db tables");
    connection
        .execute(
            "INSERT INTO metaData VALUES ('password', ?1, ?2)",
            rusqlite::params![global_salt.to_vec(), entry(b"password-check\x02\x02", 1)],
        )
        .expect("write the password check");
    connection
        .execute(
            "INSERT INTO nssPrivate VALUES (1, ?1, ?2)",
            rusqlite::params![entry(key, 2), super::logins::LOGINS_KEY_ID.to_vec()],
        )
        .expect("write the logins key");
    drop(connection);
}

/// One DER element.
fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let length = content.len();
    let mut out = vec![tag];
    if length < 0x80 {
        out.push(u8::try_from(length).unwrap_or_default());
    } else {
        let digits: Vec<u8> = length
            .to_be_bytes()
            .into_iter()
            .skip_while(|byte| *byte == 0)
            .collect();
        out.push(0x80 | u8::try_from(digits.len()).unwrap_or_default());
        out.extend(digits);
    }
    out.extend_from_slice(content);
    out
}
