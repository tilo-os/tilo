//! Your data's real tree on a real discovered inventory: every checkbox is
//! applied through the production effect path and asserted on the selection
//! the capture reads. Only source discovery is a fixture; the folders, browser
//! profiles, sign-ins and mail accounts come from actual read-only discovery.
use super::super::belongings::Belongings;
use super::super::*;
use crate::setup::{account_transfer, home_transfer};
use std::path::Path;

/// A home with two documents folders, one loose file, a Firefox profile with a
/// Slack cookie and a Thunderbird profile with two accounts.
fn fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path();
    std::fs::create_dir_all(home.join(".config")).unwrap();
    std::fs::create_dir_all(home.join("Documents/Work")).unwrap();
    std::fs::create_dir_all(home.join("Documents/Private")).unwrap();
    std::fs::write(home.join("Documents/Work/plan.txt"), b"work").unwrap();
    std::fs::write(home.join("Documents/Private/taxes.txt"), b"private").unwrap();
    std::fs::write(home.join("Documents/loose.txt"), b"loose").unwrap();

    let firefox = home.join(".mozilla/firefox");
    std::fs::create_dir_all(firefox.join("personal.default")).unwrap();
    std::fs::write(
        firefox.join("profiles.ini"),
        "[Profile0]\nName=Personal\nIsRelative=1\nPath=personal.default\n",
    )
    .unwrap();
    let profile = firefox.join("personal.default");
    let places = rusqlite::Connection::open(profile.join("places.sqlite")).unwrap();
    places
        .execute_batch(
            "CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT,last_visit_date INTEGER,visit_count INTEGER);
             CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY,parent INTEGER,position INTEGER,title TEXT,type INTEGER,fk INTEGER);
             INSERT INTO moz_places VALUES(1,'https://example.invalid/read','Read later',1700000000000000,2);
             INSERT INTO moz_bookmarks VALUES(1,0,0,'Root',2,NULL);
             INSERT INTO moz_bookmarks VALUES(2,1,0,'Saved',1,1);",
        )
        .unwrap();
    drop(places);
    let cookies = rusqlite::Connection::open(profile.join("cookies.sqlite")).unwrap();
    cookies
        .execute_batch(
            "PRAGMA user_version=17;
             CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY,host TEXT,name TEXT,value TEXT,path TEXT,
             expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,
             originAttributes TEXT,isPartitionedAttributeSet INTEGER,inBrowserElement INTEGER);
             INSERT INTO moz_cookies VALUES(1,'.slack.com','d','private-cookie','/',2000000000000,1,1,256,'',0,0);",
        )
        .unwrap();
    drop(cookies);
    std::fs::write(
        profile.join("logins.json"),
        r#"{"logins":[{"hostname":"https://example.invalid","username":"person@example.invalid","encryptedPassword":"private-password"}]}"#,
    )
    .unwrap();

    let thunderbird = home.join(".thunderbird/abc.default");
    std::fs::create_dir_all(thunderbird.join("ImapMail/imap.example.test")).unwrap();
    std::fs::create_dir_all(thunderbird.join("Mail/pop.example.test")).unwrap();
    std::fs::write(
        home.join(".thunderbird/profiles.ini"),
        "[Profile0]\nName=Personal\nIsRelative=1\nPath=abc.default\nDefault=1\n",
    )
    .unwrap();
    std::fs::write(
        thunderbird.join("prefs.js"),
        r#"user_pref("mail.accountmanager.accounts", "account1,account2");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.account.account2.server", "server2");
user_pref("mail.account.account2.identities", "id2");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.hostname", "imap.example.test");
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.test");
user_pref("mail.server.server2.type", "pop3");
user_pref("mail.server.server2.hostname", "pop.example.test");
user_pref("mail.server.server2.directory-rel", "[ProfD]Mail/pop.example.test");
user_pref("mail.identity.id1.fullName", "Personal");
user_pref("mail.identity.id2.fullName", "Work");
"#,
    )
    .unwrap();
    let message = |subject: &str| {
        format!(
            "From - Mon Jan 01 00:00:00 2024\r\nFrom: Ada <ada@example.test>\r\nSubject: {subject}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{subject}@example.test>\r\n\r\nBody.\r\n"
        )
    };
    std::fs::write(
        thunderbird.join("ImapMail/imap.example.test/Inbox"),
        message("Inbox mail"),
    )
    .unwrap();
    std::fs::write(
        thunderbird.join("ImapMail/imap.example.test/Drafts"),
        message("Draft mail"),
    )
    .unwrap();
    std::fs::write(
        thunderbird.join("Mail/pop.example.test/Inbox"),
        message("Pop mail"),
    )
    .unwrap();
    fixture
}

fn preview(home: &Path) -> InstallerPreview {
    let belongings = Belongings::from_roots(account_transfer::SourceRoots {
        home: home.to_path_buf(),
        config: home_transfer::granted_root(&home.join(".config")).unwrap(),
    });
    let mut preview = InstallerPreview {
        live: true,
        ..Default::default()
    };
    preview.journey.page = Page::Belongings;
    preview.journey.destination = Some(Destination::Another);
    preview.journey.migrate = Some(true);
    preview.journey.categories = [true, true, true];
    preview.belongings = belongings;
    preview
}

fn apply(
    preview: &mut InstallerPreview,
    effect: InstallerPreviewEffect,
) -> Result<(), &'static str> {
    preview.apply(effect)
}

fn node(preview: &InstallerPreview, label: &str) -> String {
    let data = preview.your_data();
    let found = data
        .source("files")
        .unwrap()
        .groups
        .iter()
        .find(|group| group.label == label);
    assert!(found.is_some(), "missing discovered folder {label}");
    found.unwrap().id.clone()
}

fn checkbox(preview: &InstallerPreview, test_id: &str) -> (Option<bool>, bool) {
    let composed = preview.composition(&Theme::dark());
    let widget = visual_tests::control(composed.as_ref(), test_id);
    assert!(widget.is_some(), "missing control {test_id}");
    let widget = widget.unwrap();
    let state = widget.accessibility().expect("checkbox state").state;
    (state.checked, state.mixed)
}

#[test]
fn folder_toggle_changes_the_capture_exclusions_and_the_painted_tree() {
    let fixture = fixture();
    let mut preview = preview(fixture.path());
    let documents = node(&preview, &t("installer-your-data-folder-documents"));
    assert_eq!(
        node(&preview, &t("installer-your-data-folder-documents")),
        documents
    );
    let overview = preview.your_data().report();
    assert_eq!(overview["state"], "all");
    assert!(
        preview
            .belongings
            .source_selection
            .exclusions()
            .unwrap()
            .is_empty()
    );
    apply(
        &mut preview,
        InstallerPreviewEffect::ExpandYourDataSource {
            source: "files".into(),
            expanded: true,
        },
    )
    .unwrap();
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataGroup {
            group: documents.clone(),
            included: false,
        },
    )
    .unwrap();
    let exclusions = preview.belongings.source_selection.exclusions().unwrap();
    assert!(
        exclusions
            .home()
            .iter()
            .any(|path| path == std::path::Path::new("Documents")),
        "the folder decision is the exclusion the copy enforces: {exclusions:?}"
    );
    assert_eq!(
        preview.your_data().group(&documents).unwrap().toggle(),
        super::Toggle::None
    );
    // The person's decision reaches the checkbox they clicked, and the parent
    // reports the exact mixed state instead of one total.
    let (checked, mixed) = checkbox(
        &preview,
        &preview.your_data().group(&documents).unwrap().test_id,
    );
    assert_eq!((checked, mixed), (Some(false), false));
    assert_eq!(preview.your_data().toggle(), super::Toggle::Mixed);
    // The category still says files come over, and the other folders stay.
    assert!(preview.journey.categories[0]);
    assert!(
        preview
            .belongings
            .source_selection
            .node_state(documents.strip_prefix("tree|").unwrap())
            .is_some_and(
                |state| state == imager_backend::setup::source_tree::SelectionState::Excluded
            )
    );
    let work = preview
        .your_data()
        .group(&documents)
        .unwrap()
        .groups
        .iter()
        .map(|group| group.label.clone())
        .collect::<Vec<_>>();
    assert_eq!(work.len(), 2);
    assert!(
        work.iter()
            .all(|label| label.starts_with(&t("installer-your-data-folder-other"))),
        "an unpublished folder name still needs a distinguishable row: {work:?}"
    );
    assert_ne!(
        work[0], work[1],
        "two sibling folders must not render as the same row"
    );
    // An identity the inventory no longer has changes nothing.
    assert_eq!(
        apply(
            &mut preview,
            InstallerPreviewEffect::SetYourDataItem {
                item: "tree|invented".into(),
                included: true,
            },
        ),
        Err("installer-your-data-changed")
    );
    assert_eq!(
        preview
            .belongings
            .source_selection
            .exclusions()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn selecting_the_files_source_again_restores_every_folder() {
    let fixture = fixture();
    let mut preview = preview(fixture.path());
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataSource {
            source: "files".into(),
            included: false,
        },
    )
    .unwrap();
    assert!(!preview.journey.categories[0]);
    assert!(
        !preview
            .belongings
            .source_selection
            .exclusions()
            .unwrap()
            .is_empty()
    );
    // Every child is now excluded, and the top checkbox says so.
    assert_eq!(
        preview.your_data().source("files").unwrap().toggle(),
        super::Toggle::None
    );
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataSource {
            source: "files".into(),
            included: true,
        },
    )
    .unwrap();
    assert!(preview.journey.categories[0]);
    assert!(
        preview
            .belongings
            .source_selection
            .exclusions()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        preview.your_data().source("files").unwrap().toggle(),
        super::Toggle::All
    );
}

#[test]
fn browser_profile_domain_and_session_choices_are_owned_per_profile() {
    let fixture = fixture();
    let mut preview = preview(fixture.path());
    let browser = preview
        .your_data()
        .sources
        .iter()
        .find(|source| {
            source.id
                == format!(
                    "browser|{}",
                    imager_backend::browser_inventory::BrowserKind::Firefox.label()
                )
        })
        .expect("reviewed Firefox source")
        .id
        .clone();
    let profile_group = preview
        .your_data()
        .source(&browser)
        .unwrap()
        .groups
        .iter()
        .map(|group| group.id.clone())
        .find(|id| id.starts_with("profile|"))
        .expect("one reviewed profile group");
    let profile = profile_group.strip_prefix("profile|").unwrap().to_owned();
    let domain = |preview: &InstallerPreview, kind: &str| {
        preview
            .your_data()
            .group(&profile_group)
            .unwrap()
            .items
            .iter()
            .find(|item| item.id == format!("profiledomain|{profile}|{kind}"))
            .expect("the profile's own domain item")
            .id
            .clone()
    };
    let bookmarks = domain(&preview, "bookmarks");
    let history = domain(&preview, "history");
    let before = preview.belongings.browser.profile_state(&profile).unwrap();
    assert!(before.included && before.bookmarks && before.history);

    // Excluding this profile clears its saved sign-in as well as both domains.
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataGroup {
            group: profile_group.clone(),
            included: false,
        },
    )
    .unwrap();
    let excluded = preview.belongings.browser.profile_state(&profile).unwrap();
    assert!(!excluded.included && !excluded.bookmarks && !excluded.history);
    assert!(
        preview.belongings.browser.selected().profiles().is_empty(),
        "the browser plan no longer carries the excluded profile"
    );
    assert!(
        preview.belongings.sessions.report()["selected"]
            .as_array()
            .unwrap()
            .is_empty(),
        "an excluded profile leaves no website session behind"
    );

    // Re-including the profile restores its own data only.
    let remaining = preview.your_data();
    let remaining_group = remaining
        .group(&profile_group)
        .expect("an excluded profile stays visible so it can be restored");
    assert!(
        remaining_group.selectable,
        "an excluded profile's own data can be selected again: {:?}",
        remaining_group
            .items
            .iter()
            .map(|item| (item.id.as_str(), item.eligible, item.included))
            .collect::<Vec<_>>()
    );
    drop(remaining);
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataGroup {
            group: profile_group.clone(),
            included: true,
        },
    )
    .unwrap_or_else(|error| {
        panic!(
            "re-including profile failed: {error}; state={:?}",
            preview.belongings.browser.profile_state(&profile)
        )
    });
    let restored = preview.belongings.browser.profile_state(&profile).unwrap();
    assert!(restored.included && restored.bookmarks && restored.history);
    assert_eq!(preview.belongings.browser.selected().profiles().len(), 1);

    // One profile's history is independent of its bookmarks, and the total
    // shown by the parent is the exact selection the capture reads.
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataItem {
            item: history.clone(),
            included: false,
        },
    )
    .unwrap();
    let history_off = preview.belongings.browser.profile_state(&profile).unwrap();
    assert!(history_off.included && history_off.bookmarks && !history_off.history);
    assert_eq!(preview.belongings.browser.selected().profiles().len(), 1);
    assert_eq!(
        preview.your_data().group(&profile_group).unwrap().toggle(),
        super::Toggle::Mixed
    );
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataItem {
            item: history,
            included: true,
        },
    )
    .unwrap();
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataItem {
            item: bookmarks,
            included: false,
        },
    )
    .unwrap();
    let bookmarks_off = preview.belongings.browser.profile_state(&profile).unwrap();
    assert!(bookmarks_off.included && !bookmarks_off.bookmarks && bookmarks_off.history);
    assert!(!preview.journey.browser_domains[0]);
    assert!(preview.journey.browser_domains[1]);

    // Real login metadata names a saved account without exposing a password,
    // and says plainly that the password itself is not carried.
    let (accounts_eligible, accounts_included, accounts_detail, accounts_note) = {
        let inventory = preview.your_data();
        let accounts = inventory
            .group(&profile_group)
            .unwrap()
            .items
            .iter()
            .find(|item| item.id.starts_with("browseraccounts|"))
            .expect("the profile's login metadata is visible");
        (
            accounts.eligible,
            accounts.included,
            accounts.detail.clone(),
            accounts.note.clone(),
        )
    };
    assert!(!accounts_eligible);
    assert!(!accounts_included);
    assert!(
        accounts_detail
            .as_deref()
            .is_some_and(|detail| detail.contains('1'))
    );
    assert!(accounts_note.is_some());

    // The saved sign-in is its own selected source/service pair inside the
    // profile group, and turning it off leaves the domains alone.
    let sign_in = preview
        .your_data()
        .group(&profile_group)
        .unwrap()
        .items
        .iter()
        .find(|item| item.id.starts_with("session|"))
        .expect("the discovered Slack cookie is a sign-in child")
        .id
        .clone();
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataItem {
            item: sign_in,
            included: false,
        },
    )
    .unwrap();
    assert!(
        preview.belongings.sessions.report()["selected"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        preview
            .belongings
            .browser
            .profile_state(&profile)
            .unwrap()
            .history
    );
}

#[test]
fn browser_source_master_toggle_is_one_atomic_edit_over_profiles_and_sessions() {
    let fixture = fixture();
    let mut preview = preview(fixture.path());
    let browser = preview
        .your_data()
        .sources
        .iter()
        .find(|source| source.id.starts_with("browser|"))
        .expect("a discovered browser source")
        .id
        .clone();
    let profile = preview
        .your_data()
        .source(&browser)
        .unwrap()
        .groups
        .iter()
        .find_map(|group| group.id.strip_prefix("profile|").map(str::to_owned))
        .expect("a reviewed browser profile");
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataSource {
            source: browser.clone(),
            included: false,
        },
    )
    .unwrap();
    assert!(
        !preview
            .belongings
            .browser
            .profile_state(&profile)
            .unwrap()
            .included
    );
    assert!(preview.belongings.browser.selected().profiles().is_empty());
    assert!(
        preview.belongings.sessions.report()["selected"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataSource {
            source: browser,
            included: true,
        },
    )
    .unwrap();
    let state = preview.belongings.browser.profile_state(&profile).unwrap();
    assert!(state.included && state.bookmarks && state.history);
    assert_eq!(preview.belongings.browser.selected().profiles().len(), 1);
}

#[test]
fn thunderbird_accounts_and_folders_are_selected_independently_and_validated() {
    let fixture = fixture();
    let mut preview = preview(fixture.path());
    let profile = preview
        .belongings
        .thunderbird_profiles()
        .next()
        .expect("the discovered Thunderbird profile")
        .source_id
        .clone();
    let mail = preview
        .your_data()
        .sources
        .iter()
        .find(|source| source.id == format!("mail|{profile}"))
        .expect("a mail source per reviewed profile")
        .id
        .clone();
    assert_eq!(mail, format!("mail|{profile}"));
    let accounts = preview
        .your_data()
        .source(&mail)
        .unwrap()
        .groups
        .iter()
        .map(|group| group.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(accounts.len(), 2, "two reviewed accounts, not one total");
    let selected = preview.belongings.thunderbird_selection(&profile).unwrap();
    assert_eq!(selected.accounts.len(), 2);
    let first_folder = preview.your_data().source(&mail).unwrap().groups[0].items[0]
        .id
        .clone();
    // Excluding one account leaves the other account and its folders intact.
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataGroup {
            group: accounts[1].clone(),
            included: false,
        },
    )
    .unwrap();
    let selected = preview.belongings.thunderbird_selection(&profile).unwrap();
    assert_eq!(selected.accounts.len(), 1);
    assert_eq!(
        selected.accounts[0].source_id,
        accounts[0]
            .strip_prefix("mailaccount|")
            .unwrap()
            .split_once('|')
            .unwrap()
            .1
    );
    assert!(
        selected
            .validate(
                preview
                    .belongings
                    .accounts
                    .thunderbird_reviewed
                    .iter()
                    .find(|reviewed| reviewed.profile.source_id == profile)
                    .unwrap()
            )
            .is_ok()
    );
    // One folder inside the remaining account is an independent decision.
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataItem {
            item: first_folder.clone(),
            included: false,
        },
    )
    .unwrap();
    let selected = preview.belongings.thunderbird_selection(&profile).unwrap();
    assert_eq!(selected.accounts.len(), 1, "the account stays selected");
    assert!(
        !selected.accounts[0]
            .folders
            .iter()
            .any(|folder| first_folder.ends_with(folder)),
        "the excluded folder is gone from the import selection"
    );
    assert!(
        !selected.accounts[0].folders.is_empty(),
        "the sibling folders stay selected"
    );
    // The painted row reports the exact state of the account, and the source
    // keeps its mixed mark: nothing was widened by the folder decision.
    assert_eq!(
        preview.your_data().source(&mail).unwrap().toggle(),
        super::Toggle::Mixed
    );
    apply(
        &mut preview,
        InstallerPreviewEffect::ExpandYourDataSource {
            source: mail,
            expanded: true,
        },
    )
    .unwrap();
    let (checked, mixed) = checkbox(
        &preview,
        &preview.your_data().group(&accounts[1]).unwrap().test_id,
    );
    assert_eq!((checked, mixed), (Some(false), false));
    let partially = preview
        .your_data()
        .group(&accounts[0])
        .unwrap()
        .test_id
        .clone();
    assert_eq!(
        checkbox(&preview, &partially),
        (Some(false), true),
        "the account whose folder was removed is the shared mixed state"
    );
    // An account identity that no longer exists is refused and changes nothing.
    assert_eq!(
        apply(
            &mut preview,
            InstallerPreviewEffect::SetYourDataGroup {
                group: format!("mailaccount|{profile}|invented"),
                included: true,
            },
        ),
        Err("installer-your-data-changed")
    );
    assert_eq!(
        preview
            .belongings
            .thunderbird_selection(&profile)
            .unwrap()
            .accounts
            .len(),
        1
    );
    let (reviewed, selection) = preview
        .belongings
        .thunderbird_export(&profile)
        .expect("the boundary refuses only an empty or stale selection");
    assert!(selection.validate(reviewed).is_ok());
}

/// A real GOA configuration with two accounts: the exact per-account and
/// per-service rows are the selection, and only the checked services become
/// pending destination rows. A mailbox the person already has is reused.
fn goa_fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path();
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("goa-1.0")).unwrap();
    std::fs::write(
        config.join("goa-1.0/accounts.conf"),
        concat!(
            "[Account account_1]\n",
            "Provider=google\n",
            "Identity=person@example.test\n",
            "PresentationIdentity=person@example.test\n",
            "FilesEnabled=true\n",
            "MailEnabled=true\n",
            "CalendarEnabled=true\n",
            "ContactsEnabled=false\n",
            "[Account account_2]\n",
            "Provider=google\n",
            "Identity=other@example.test\n",
            "PresentationIdentity=other@example.test\n",
            "FilesEnabled=true\n",
            "MailEnabled=true\n",
            "CalendarEnabled=false\n",
            "ContactsEnabled=false\n",
        ),
    )
    .unwrap();
    fixture
}

#[test]
fn goa_accounts_and_services_are_individually_selectable_and_only_selected_become_pending() {
    use crate::setup::account_transfer::Origin;
    use imager_backend::account_sources::Service;

    let fixture = goa_fixture();
    let mut preview = preview(fixture.path());
    let source = preview
        .your_data()
        .source("goa")
        .expect("the exact GNOME Online Accounts source, not a generic account form")
        .id
        .clone();
    assert_eq!(source, "goa");
    let gnome = preview
        .belongings
        .accounts
        .candidates
        .iter()
        .filter(|candidate| candidate.origin == Origin::Gnome)
        .collect::<Vec<_>>();
    assert_eq!(gnome.len(), 2);
    let person = gnome
        .iter()
        .find(|candidate| candidate.identity.as_deref() == Some("person@example.test"))
        .unwrap();
    let other = gnome
        .iter()
        .find(|candidate| candidate.identity.as_deref() == Some("other@example.test"))
        .unwrap();
    let person_id = person.id.clone();
    let other_id = other.id.clone();
    let inventory = preview.your_data();
    let groups = &inventory.source("goa").unwrap().groups;
    assert_eq!(groups.len(), 2, "one row per configured account");
    let person_group = groups
        .iter()
        .find(|group| group.id == format!("account|{person_id}"))
        .expect("the person's exact account row");
    assert_eq!(person_group.items.len(), 3, "Files, Mail and Calendar");
    assert!(
        person_group
            .items
            .iter()
            .all(|item| item.eligible && item.included)
    );
    let calendar = person_group
        .items
        .iter()
        .find(|item| item.id == format!("accountservice|{person_id}|calendar"))
        .expect("Calendar is its own service row")
        .id
        .clone();
    drop(inventory);

    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataItem {
            item: calendar.clone(),
            included: false,
        },
    )
    .unwrap();
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataGroup {
            group: format!("account|{other_id}"),
            included: false,
        },
    )
    .unwrap();
    apply(
        &mut preview,
        InstallerPreviewEffect::ExpandYourDataSource {
            source: "goa".into(),
            expanded: true,
        },
    )
    .unwrap();
    apply(
        &mut preview,
        InstallerPreviewEffect::ExpandYourDataSource {
            source: format!("account|{person_id}"),
            expanded: true,
        },
    )
    .unwrap();

    let source_state = preview.your_data().source("goa").unwrap().toggle();
    assert_eq!(source_state, super::Toggle::Mixed);
    assert_eq!(
        checkbox(
            &preview,
            &preview
                .your_data()
                .group(&format!("account|{other_id}"))
                .unwrap()
                .test_id,
        ),
        (Some(false), false)
    );
    assert_eq!(
        checkbox(
            &preview,
            &preview.your_data().item(&calendar).unwrap().test_id,
        ),
        (Some(false), false)
    );

    // The aggregate identity still skips the optional Accounts step without
    // owning the exact account rows: their selection survives the alias.
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataSource {
            source: "accounts".into(),
            included: false,
        },
    )
    .unwrap();
    assert!(!preview.journey.categories[2]);
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataSource {
            source: "accounts".into(),
            included: true,
        },
    )
    .unwrap();
    assert!(preview.journey.categories[2]);
    assert_eq!(
        preview.your_data().source("goa").unwrap().toggle(),
        super::Toggle::Mixed,
        "the alias changed only the step, not the exact service decisions"
    );

    let selected = preview
        .accounts
        .selected_source_accounts(&preview.belongings.accounts);
    assert_eq!(selected.len(), 1, "the excluded account is not selected");
    assert_eq!(selected[0].identity, "person@example.test");
    assert_eq!(
        selected[0].services,
        vec![Service::Files, Service::Mail],
        "Calendar was turned off"
    );

    let target = tempfile::tempdir().unwrap();
    let target_root = target.path().join(".config/tilo");
    let mut store = ::accounts::AccountStore::new();
    let mut existing = ::accounts::Account::new(
        ::accounts::AccountId::new("person@example.test"),
        ::accounts::thunderbird::PENDING_TYPE,
    );
    existing.config = serde_json::json!({
        "display_name": "Personal",
        "email": "person@example.test",
        "import_pending": true,
        "source_provenance": {"mail_client": "thunderbird"},
    });
    store.upsert(existing);
    store.save_in(&target_root).unwrap();
    let receipts = imager_backend::account_transfer::import_selected_goa(
        &target_root,
        &preview.belongings.accounts,
        &selected,
    )
    .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].services, vec![Service::Files, Service::Mail]);
    assert!(receipts[0].pending);
    assert!(
        !serde_json::to_string(&receipts)
            .unwrap()
            .contains("person@example.test")
    );

    let after = ::accounts::AccountStore::load_in(&target_root);
    assert!(after.all().iter().all(|account| {
        account
            .config
            .get("source_goa_identity")
            .and_then(serde_json::Value::as_str)
            != Some("other@example.test")
            && account
                .config
                .get("email")
                .and_then(serde_json::Value::as_str)
                != Some("other@example.test")
    }));
    assert!(after.all().iter().any(|account| {
        account.type_id.as_str() == ::accounts::cloud_types::GOOGLE_DRIVE
            && account.config["source_goa_identity"] == "person@example.test"
    }));
    let mail = after
        .all()
        .iter()
        .filter(|account| {
            account
                .config
                .get("email")
                .and_then(serde_json::Value::as_str)
                == Some("person@example.test")
        })
        .collect::<Vec<_>>();
    assert_eq!(mail.len(), 1, "the existing Thunderbird mailbox is reused");
    assert_eq!(
        mail[0].config["source_provenance"]["mail_client"],
        "thunderbird"
    );
    assert!(
        after
            .all()
            .iter()
            .all(|account| { account.type_id.as_str() != "caldav" })
    );
}

/// Nextcloud and rclone are app sources with real account identities and the
/// services they turn on. They start selected like every other discovered
/// source, and the master decision covers all of them in one edit.
#[test]
fn nextcloud_and_rclone_are_product_sources_selected_by_default() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path();
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("Nextcloud")).unwrap();
    std::fs::write(
        config.join("Nextcloud/nextcloud.cfg"),
        concat!(
            "[Accounts]\n",
            "0\\url=https://cloud.example.test/base\n",
            "0\\webflow_user=ada\n",
            "0\\password=DO_NOT_LEAK\n",
        ),
    )
    .unwrap();
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    std::fs::write(
        config.join("rclone/rclone.conf"),
        concat!(
            "[Team files]\n",
            "type=webdav\n",
            "vendor=nextcloud\n",
            "url=https://team.example.test/\n",
            "user=ada\n",
            "pass=DO_NOT_LEAK\n",
        ),
    )
    .unwrap();
    let mut preview = preview(home);
    let sources = preview.your_data();
    let nextcloud = sources
        .source("accounts|nextcloud")
        .expect("Nextcloud is its own product source");
    assert_eq!(nextcloud.groups.len(), 1);
    assert_eq!(nextcloud.groups[0].items.len(), 1);
    assert!(nextcloud.groups[0].items[0].eligible);
    assert!(nextcloud.groups[0].items[0].included);
    assert_eq!(nextcloud.toggle(), super::Toggle::All);
    let rclone = sources
        .source("accounts|rclone")
        .expect("rclone is its own product source");
    assert_eq!(rclone.groups.len(), 1);
    assert_eq!(rclone.toggle(), super::Toggle::All);
    let rclone_group = rclone.groups[0].id.clone();
    let nextcloud_group = nextcloud.groups[0].id.clone();
    assert!(nextcloud_group.starts_with("account|"));
    let report = serde_json::to_string(&sources.report()).unwrap();
    assert!(
        !report.contains("https://cloud.example.test") && !report.contains("DO_NOT_LEAK"),
        "a source row never publishes its server or credential"
    );

    // One rclone remote is its own decision; the Nextcloud account stays.
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataGroup {
            group: rclone_group.clone(),
            included: false,
        },
    )
    .unwrap();
    let sources = preview.your_data();
    assert_eq!(
        sources.source("accounts|rclone").unwrap().toggle(),
        super::Toggle::None
    );
    assert_eq!(
        sources.source("accounts|nextcloud").unwrap().toggle(),
        super::Toggle::All
    );
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataGroup {
            group: rclone_group,
            included: true,
        },
    )
    .unwrap();

    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataAll { included: false },
    )
    .unwrap();
    let sources = preview.your_data();
    assert_eq!(
        sources.source("accounts|nextcloud").unwrap().toggle(),
        super::Toggle::None
    );
    assert_eq!(
        sources.source("accounts|rclone").unwrap().toggle(),
        super::Toggle::None
    );

    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataAll { included: true },
    )
    .unwrap();
    let sources = preview.your_data();
    assert_eq!(
        sources.source("accounts|nextcloud").unwrap().toggle(),
        super::Toggle::All
    );
    assert_eq!(
        sources.source("accounts|rclone").unwrap().toggle(),
        super::Toggle::All
    );
}

/// A granted Windows OneDrive root carries account metadata even though its
/// configuration names no service list. It is recognized as a file account and
/// keeps its real identity instead of disappearing behind a generic form.
#[test]
fn granted_windows_onedrive_root_is_an_account_source() {
    use crate::setup::windows_inventory::{RootRole, SelectedRoot};

    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("OneDrive");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("notes.txt"), b"notes").unwrap();
    let mut preview = InstallerPreview {
        live: true,
        ..Default::default()
    };
    preview.journey.page = Page::Belongings;
    preview.journey.destination = Some(Destination::Another);
    preview.journey.migrate = Some(true);
    preview.journey.categories = [true, true, true];
    preview.belongings = Belongings::from_windows_roots(&[SelectedRoot {
        path: root,
        role: RootRole::OneDrive,
    }])
    .unwrap();

    let sources = preview.your_data();
    let source = sources
        .sources
        .iter()
        .find(|source| source.id.starts_with("accounts|application|"))
        .expect("the granted Windows account is its own app source");
    assert_eq!(source.groups.len(), 1);
    assert_eq!(source.groups[0].items.len(), 1);
    assert_eq!(
        source.groups[0].items[0].label,
        t("installer-your-data-service-files")
    );
    assert!(source.groups[0].items[0].eligible);
    assert!(source.groups[0].items[0].included);
    assert_eq!(source.toggle(), super::Toggle::All);
}

#[test]
fn one_tree_edit_restores_every_selection_authority_when_it_fails() {
    let fixture = fixture();
    let mut preview = preview(fixture.path());
    let documents = node(&preview, &t("installer-your-data-folder-documents"));
    let accounts = preview
        .your_data()
        .sources
        .iter()
        .find(|source| source.id.starts_with("mail|"))
        .unwrap()
        .groups[0]
        .id
        .clone();
    let before_tree = preview.belongings.source_selection.exclusions().unwrap();
    let before_profiles = preview.belongings.browser.profile_choices();
    let before_mail = preview.belongings.thunderbird.clone();
    let result = preview.change_your_data(|this| {
        this.apply_your_data_group(&documents, false)?;
        this.apply_your_data_group(&accounts, false)?;
        this.belongings.browser.set_profile("invented", false)?;
        Ok(true)
    });
    assert_eq!(result, Err("installer-browser-source-changed"));
    assert_eq!(
        preview.belongings.source_selection.exclusions().unwrap(),
        before_tree,
        "a failed batch leaves the folder selection exactly as it was"
    );
    assert_eq!(
        preview.belongings.browser.profile_choices(),
        before_profiles
    );
    assert_eq!(
        preview
            .belongings
            .thunderbird_selection(
                preview
                    .belongings
                    .thunderbird_profiles()
                    .next()
                    .unwrap()
                    .source_id
                    .as_str()
            )
            .unwrap()
            .accounts
            .len(),
        before_mail.values().next().unwrap().accounts.len()
    );
    assert_eq!(preview.belongings.thunderbird, before_mail);
}

#[test]
fn excluded_folder_reaches_the_canonical_home_capture_review() {
    let fixture = fixture();
    let mut preview = preview(fixture.path());
    let documents = node(&preview, &t("installer-your-data-folder-documents"));
    apply(
        &mut preview,
        InstallerPreviewEffect::SetYourDataGroup {
            group: documents,
            included: false,
        },
    )
    .unwrap();
    let exclusions = preview
        .migration_exclusions()
        .expect("the reviewed tree is the capture selection");
    assert_eq!(exclusions, vec![std::path::PathBuf::from("Documents")]);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let full = imager_backend::setup::portable_home_media::HomeSource::review_with_cancel(
        fixture.path(),
        &cancel,
    )
    .unwrap();
    let selected =
        imager_backend::setup::portable_home_media::HomeSource::review_with_cancel_selected(
            fixture.path(),
            &exclusions,
            &cancel,
        )
        .unwrap();
    assert_eq!(selected.exclusions(), exclusions.as_slice());
    assert_eq!(
        selected.summary().unwrap().bytes,
        full.summary().unwrap().bytes - (b"work".len() + b"private".len() + b"loose".len()) as u64,
        "the actual reviewed producer omits every file below the excluded folder"
    );
    selected.validate().unwrap();
    // Media preparation takes its own route on a filesystem whose original
    // reader retains the source; either route carries the same exclusions.
    let media = imager_backend::setup::portable_home_media::HomeSource::review_selected_for_media_preparation(
        fixture.path(),
        &exclusions,
        &cancel,
    )
    .unwrap();
    assert_eq!(media.exclusions(), exclusions.as_slice());
    // The excluded original is only read, never deleted or rewritten.
    assert_eq!(
        std::fs::read(fixture.path().join("Documents/Private/taxes.txt")).unwrap(),
        b"private"
    );
}
