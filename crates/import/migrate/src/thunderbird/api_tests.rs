//! The laptop-facing reads over the sample profile.

use super::fixture::{
    HOME_EMAIL, WORK_EMAIL, WORK_SERVER, write_flatpak_profile, write_sample_profile,
    write_stale_mail_directories,
};
use super::*;

fn sample() -> (tempfile::TempDir, ThunderbirdProfile) {
    let home = tempfile::tempdir().unwrap();
    write_sample_profile(home.path());
    let profiles = discover(home.path()).unwrap();
    assert_eq!(profiles.len(), 1);
    (home, profiles.into_iter().next().unwrap())
}

#[test]
fn discovery_lists_both_accounts_their_folders_books_and_calendars() {
    let (_home, profile) = sample();
    assert_eq!(profile.accounts.len(), 2);
    let work = profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(WORK_EMAIL))
        .expect("the Work account");
    assert_eq!(work.kind, "imap");
    assert_eq!(work.display_name, "Work");
    assert_eq!(work.identities.len(), 1);
    assert_eq!(work.identities[0].email, WORK_EMAIL);
    let inbox = work
        .folders
        .iter()
        .find(|folder| folder.hierarchy == ["INBOX"])
        .expect("the Work inbox");
    assert_eq!(inbox.kind, FolderKind::Inbox);
    assert_eq!(inbox.format, "mbox");
    assert_eq!(inbox.messages, Some(3));
    let sent = work
        .folders
        .iter()
        .find(|folder| folder.hierarchy == ["Sent"])
        .expect("the Work sent folder");
    assert_eq!(sent.kind, FolderKind::Sent);

    let home = profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(HOME_EMAIL))
        .expect("the Home account");
    assert_eq!(home.identities.len(), 2, "both of Home's identities");
    assert_eq!(
        home.folders
            .iter()
            .find(|folder| folder.hierarchy == ["INBOX"])
            .and_then(|folder| folder.messages),
        Some(2)
    );

    assert_eq!(profile.address_books.len(), 1);
    assert_eq!(profile.address_books[0].name, "Personal address book");
    assert_eq!(profile.address_books[0].contacts, Some(2));
    assert_eq!(profile.calendars.len(), 1);
    assert_eq!(profile.calendars[0].events, Some(2));
}

#[test]
fn account_settings_carry_the_imap_and_smtp_endpoints_without_a_password() {
    let (_home, profile) = sample();
    let work = profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(WORK_EMAIL))
        .unwrap();
    let settings = account_settings(&profile, &work.source_id).expect("settings");
    assert_eq!(settings.hostname.as_deref(), Some(WORK_SERVER));
    let candidate = settings.candidate.expect("an IMAP candidate");
    assert_eq!(candidate.incoming.host, WORK_SERVER);
    assert_eq!(candidate.incoming.port, 993);
    assert_eq!(candidate.incoming.username, WORK_EMAIL);
    assert_eq!(candidate.outgoing.host, "smtp.example.com");
    assert_eq!(candidate.outgoing.port, 465);
    let serialized = serde_json::to_string(&profile).unwrap();
    assert!(
        !serialized.contains("ImapMail"),
        "the pinned source paths stay out of a status record"
    );
    assert!(!serialized.contains("\"settings\""));
}

#[test]
fn an_address_book_is_read_as_the_vcard_text_contacts_imports() {
    let (home, profile) = sample();
    let book = &profile.address_books[0];
    let cards = super::address_book(home.path(), &profile.source_id, &book.source_id).unwrap();
    assert_eq!(cards.matches("BEGIN:VCARD").count(), 2, "{cards}");
    let ada = cards
        .split("BEGIN:VCARD")
        .find(|card| card.contains("Ada Lovelace"))
        .expect("Ada's card");
    assert!(ada.contains("ORG:Analytical Engines"), "{ada}");
    assert!(ada.contains("1815"), "her birthday is kept: {ada}");
    assert!(ada.contains("ada@"), "her address is kept: {ada}");
    let again = super::address_book(home.path(), &profile.source_id, &book.source_id).unwrap();
    assert_eq!(cards, again, "the same book reads the same cards");
}

#[test]
fn a_calendar_goes_over_as_the_file_every_calendar_reads() {
    let (home, profile) = sample();
    let calendar = &profile.calendars[0];
    let text = super::calendar_file(home.path(), &profile.source_id, &calendar.source_id).unwrap();
    assert_eq!(text.matches("BEGIN:VEVENT").count(), 2, "{text}");
    assert!(text.contains("SUMMARY:Design review"), "{text}");
    assert!(text.contains("SUMMARY:Company holiday"), "{text}");
}

/// A home reached through a link, as Fedora Atomic's `/home` is a link to `/var/home`, is read
/// as the home it is; a link inside the profile is still never followed.
#[cfg(unix)]
#[test]
fn a_home_reached_through_a_link_is_read() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("var-home");
    std::fs::create_dir(&real).unwrap();
    write_sample_profile(&real);
    let linked = root.path().join("home");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let profiles = discover(&linked).unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].accounts.len(), 2);
}

/// A profile root reached through the application's link, as Flatpak keeps
/// it, is the root it points at.
#[cfg(unix)]
#[test]
fn a_profile_root_behind_the_packaged_application_link_is_read() {
    let home = tempfile::tempdir().unwrap();
    write_flatpak_profile(home.path());

    let scan = scan(home.path());
    assert!(scan.problems.is_empty(), "{:?}", scan.problems);
    assert_eq!(scan.reviewed.len(), 1);
    let profile = scan.reviewed[0].profile();
    assert_eq!(profile.accounts.len(), 2);
}

/// A Windows home keeps its profiles under its roaming application data, and they are read there,
/// as on a Windows computer or from a Windows disk.
#[test]
fn a_profile_where_windows_keeps_it_is_read() {
    let staging = tempfile::tempdir().unwrap();
    write_sample_profile(staging.path());
    let home = tempfile::tempdir().unwrap();
    let roaming = home.path().join("AppData/Roaming");
    std::fs::create_dir_all(&roaming).unwrap();
    std::fs::rename(
        staging.path().join(".thunderbird"),
        roaming.join("Thunderbird"),
    )
    .unwrap();

    let scan = scan(home.path());
    assert!(scan.problems.is_empty(), "{:?}", scan.problems);
    assert_eq!(scan.reviewed.len(), 1);
    let profile = scan.reviewed[0].profile();
    assert_eq!(profile.accounts.len(), 2);
    assert!(
        profile
            .accounts
            .iter()
            .any(|account| account.email.as_deref() == Some(WORK_EMAIL)
                && account
                    .folders
                    .iter()
                    .any(|folder| folder.hierarchy == ["INBOX"])),
        "the Work account and its inbox are read: {:?}",
        profile.accounts
    );
}

/// A profile copied in from another Thunderbird location still names that
/// place in `prefs.js`; its mail is in the profile itself, and is read.
#[test]
fn mail_named_at_another_location_is_found_where_thunderbird_lays_it_out() {
    let home = tempfile::tempdir().unwrap();
    write_sample_profile(home.path());
    write_stale_mail_directories(home.path());

    let scan = scan(home.path());
    assert!(scan.problems.is_empty(), "{:?}", scan.problems);
    assert_eq!(scan.reviewed.len(), 1);
    let profile = scan.reviewed[0].profile();
    let work = profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(WORK_EMAIL))
        .expect("the Work account");
    assert_eq!(
        work.folders
            .iter()
            .find(|folder| folder.hierarchy == ["INBOX"])
            .and_then(|folder| folder.messages),
        Some(3)
    );
    assert_eq!(
        work.folders
            .iter()
            .find(|folder| folder.hierarchy == ["Sent"])
            .and_then(|folder| folder.messages),
        Some(1)
    );
}

/// One folder that cannot be read is named with its path and reason, and the
/// folders beside it are still listed.
#[cfg(unix)]
#[test]
fn an_unreadable_folder_is_named_and_the_rest_is_still_listed() {
    use std::os::unix::ffi::OsStrExt as _;

    let home = tempfile::tempdir().unwrap();
    write_sample_profile(home.path());
    // A subfolder tree whose entry name is not UTF-8: the folder cannot be
    // read, while its siblings in the account are fine.
    let subfolder = home.path().join(format!(
        ".thunderbird/fx8r2l9p.default-release/ImapMail/{WORK_SERVER}/Projects.sbd"
    ));
    std::fs::create_dir(&subfolder).unwrap();
    std::fs::write(
        subfolder.join(std::ffi::OsStr::from_bytes(b"2026-\xff")),
        b"",
    )
    .unwrap();

    let scan = scan(home.path());
    let reviewed = scan.reviewed.first().expect("the profile is still read");
    let work = reviewed
        .profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(WORK_EMAIL))
        .expect("the Work account");
    assert!(
        work.folders
            .iter()
            .any(|folder| folder.hierarchy == ["INBOX"]),
        "the folders beside it are still listed"
    );
    assert!(
        work.folders
            .iter()
            .any(|folder| folder.hierarchy == ["Sent"]),
        "the folders beside it are still listed"
    );
    let relative = format!("ImapMail/{WORK_SERVER}/Projects.sbd");
    assert_eq!(reviewed.problems.len(), 1, "{:?}", reviewed.problems);
    let problem = &reviewed.problems[0];
    assert_eq!(problem.relative.as_deref(), Some(relative.as_str()));
    assert_eq!(problem.kind, std::io::ErrorKind::InvalidData);
    assert!(
        problem.to_string().contains(&relative) && problem.to_string().contains("unreadable data"),
        "{problem}"
    );
}
