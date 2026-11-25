//! What an app reaches of a record by what its signed manifest declares: its own records and the
//! workspace's to change, another app's only to read where it declares that read, and in the
//! person's own workspace its own records and the records of theirs it declares.

use super::{AppReach, app_reach};
use tilo_actions::remote::ResourceAddress;

/// Whether `app` reaches `address` in the person's own workspace, to read or to change.
fn persons_record(app: &str, address: &ResourceAddress) -> Result<(), String> {
    app_reach(app, address, true).map(drop)
}

fn workspace_record(app: &str) -> ResourceAddress {
    ResourceAddress {
        workspace: "4f2c1a64-0d7e-4b8e-9b55-2a6f3c9d1e10".into(),
        app: app.into(),
        record: "record-1".into(),
    }
}

/// `record` of `app` in the person's own workspace.
fn person_record(app: &str, record: &str) -> ResourceAddress {
    ResourceAddress {
        workspace: collab_types::person_workspace(&[7; 32]).to_string(),
        app: app.into(),
        record: record.into(),
    }
}

/// The manifests these cases register: what each app may read, and the person's own record one
/// of them declares.
static NOTES: &[u8] =
    b"{\"resources\":[{\"kind\":\"tl.test.lang-notes.Note\",\"label\":\"Note\",\"version\":1}]}";
static BOARD: &[u8] = b"{\"reads\":[\"tl.test.lang-notes.Note\"],\"resources\":[{\"kind\":\"tl.test.lang-board.Card\",\"label\":\"Card\",\"version\":1}]}";
static CHAT: &[u8] =
    b"{\"resources\":[{\"kind\":\"tl.test.lang-chat.Message\",\"label\":\"Message\",\"version\":1}]}";
static READER: &[u8] = b"{\"resources\":[{\"kind\":\"test-person.accounts\",\"label\":\"Accounts\",\"person\":true,\"version\":1}]}";

/// An app changes only its own records. Another app's record it reads only when its manifest
/// declares that it reads that app's kinds; it changes such a record only through the owning
/// app's actions.
#[test]
fn an_app_changes_only_its_own_records_and_reads_another_apps_only_as_declared() {
    collab_types::app_surface::register_package("test.lang-notes", NOTES);
    collab_types::app_surface::register_package("test.lang-board", BOARD);
    collab_types::app_surface::register_package("test.lang-chat", CHAT);
    workspace_records_are_the_apps_own_or_declared_reads();
    person_records_of_another_app_are_read_never_changed();
}

/// Its own record, whatever it reads; another app's only to read, and only when it declares that
/// read.
fn workspace_records_are_the_apps_own_or_declared_reads() {
    assert_eq!(
        app_reach(
            "test.lang-board",
            &workspace_record("test.lang-board"),
            false
        ),
        Ok(AppReach::Changes),
        "its own record, whatever it reads"
    );
    assert_eq!(
        app_reach("test.lang-board", &workspace_record(""), false),
        Ok(AppReach::Changes),
        "a record of no app is the workspace's"
    );
    assert_eq!(
        app_reach(
            "test.lang-board",
            &workspace_record("test.lang-notes"),
            false
        ),
        Ok(AppReach::Reads),
        "Board reads the notes it declares, read-only"
    );
    assert!(
        app_reach(
            "test.lang-board",
            &workspace_record("test.lang-chat"),
            false
        )
        .is_err(),
        "nor another app's record it declares no read of"
    );
    let emoji = ResourceAddress {
        record: collab_types::workspace_document_id("tilo.workspace.emoji"),
        ..workspace_record("so.tilo.chat")
    };
    assert_eq!(
        app_reach("test.lang-board", &emoji, false),
        Ok(AppReach::Changes),
        "a workspace record is any app's of the workspace, as its grants decide"
    );
    assert!(
        app_reach(
            "test.lang-chat",
            &workspace_record("test.lang-notes"),
            false
        )
        .is_err(),
        "an app that reads nothing reaches only its own records"
    );
    assert!(
        app_reach("test.unknown", &workspace_record("test.lang-notes"), false).is_err(),
        "an app whose manifest the device does not hold reads nothing"
    );
}

/// Another app's record of the person's is read, never changed.
fn person_records_of_another_app_are_read_never_changed() {
    let accounts = collab_types::person_document_id("test-person.accounts");
    collab_types::app_surface::register_package("test.person-reader", READER);
    assert_eq!(
        app_reach(
            "test.person-reader",
            &person_record("test.person-settings", &accounts),
            true
        ),
        Ok(AppReach::Reads),
        "another app's record of the person's is read, never changed"
    );
}

/// An app reaches the person's workspace for the records of theirs its manifest declares, its own
/// or another app's it reads, and for nothing else.
#[test]
fn an_app_reaches_the_persons_workspace_only_for_the_records_it_declares() {
    static SETTINGS: &[u8] =
        b"{\"resources\":[{\"kind\":\"tilo.accounts\",\"label\":\"Accounts\",\"version\":1}]}";
    static MAIL: &[u8] = b"{\"reads\":[\"tilo.accounts\"],\"resources\":[{\"kind\":\"test-mail.Message\",\"label\":\"Message\",\"version\":1}]}";
    static TERMINAL: &[u8] = b"{\"resources\":[{\"kind\":\"test-person.session\",\"label\":\"Session\",\"version\":1},{\"kind\":\"test-person.history\",\"label\":\"History\",\"person\":true,\"version\":1},{\"kind\":\"test-person.accounts\",\"label\":\"Accounts\",\"person\":true,\"version\":1}]}";
    collab_types::app_surface::register_package("test.person-terminal", TERMINAL);
    let history = collab_types::person_document_id("test-person.history");
    let accounts = collab_types::person_document_id("test-person.accounts");
    assert_eq!(
        persons_record(
            "test.person-terminal",
            &person_record("test.person-terminal", &history)
        ),
        Ok(())
    );
    assert_eq!(
        persons_record(
            "test.person-terminal",
            &person_record("test.person-settings", &accounts)
        ),
        Ok(()),
        "another app's record of the person's it declares, to read"
    );
    let undeclared = collab_types::person_document_id("test-person.bookmarks");
    assert!(
        persons_record(
            "test.person-terminal",
            &person_record("test.person-terminal", &undeclared)
        )
        .is_err()
    );
    assert!(
        persons_record(
            "test.person-notes",
            &person_record("test.person-terminal", "tab:01")
        )
        .is_err(),
        "nor another app's record"
    );
    assert_eq!(
        persons_record(
            "test.person-terminal",
            &person_record("test.person-terminal", "")
        ),
        Ok(()),
        "its own record naming no record, as every app opens"
    );
    assert!(
        persons_record(
            "test.person-notes",
            &person_record("test.person-terminal", "")
        )
        .is_err(),
        "not another app's"
    );
    collab_types::app_surface::register_package("so.tilo.settings", SETTINGS);
    collab_types::app_surface::register_package("test.person-mail", MAIL);
    let accounts_list = person_record(
        "so.tilo.settings",
        &collab_types::workspace_document_id("tilo.accounts"),
    );
    assert_eq!(
        persons_record("so.tilo.settings", &accounts_list),
        Ok(()),
        "the workspace's account list, by the app it belongs to"
    );
    assert_eq!(
        persons_record("test.person-mail", &accounts_list),
        Ok(()),
        "and by an app that declares it reads it"
    );
    assert!(
        persons_record("test.person-terminal", &accounts_list).is_err(),
        "but not by an app that declares no read of it"
    );
    assert!(
        persons_record(
            "test.person-notes",
            &person_record("test.person-notes", &history)
        )
        .is_err(),
        "but still no record of the person's of a kind it does not declare"
    );
    assert!(
        persons_record(
            "test.person-notes",
            &person_record("test.person-terminal", &history)
        )
        .is_err(),
        "an app that declares none of them is refused"
    );
}

/// Messages opens its own primary record in the person's workspace, as in any workspace it is
/// installed in, and reads the people Contacts keeps there, a workspace record of Contacts' its
/// surface need not list, because it declares that it reads them; it changes them only through
/// Contacts' actions. An app that declares no such read, another app's record it declares no read
/// of, and a record of the person's it declares no kind of, stay refused.
#[test]
fn an_app_opens_its_own_records_and_reads_those_it_declares_in_the_persons_workspace() {
    static CONTACTS: &[u8] =
        b"{\"resources\":[{\"kind\":\"tl.so.tilo.contacts.Card\",\"label\":\"Card\",\"version\":1}]}";
    static MESSAGES: &[u8] = b"{\"reads\":[\"tilo.contacts.identities\"],\"resources\":[{\"kind\":\"tilo.messages\",\"label\":\"Messages\",\"version\":1}]}";
    static UNDECLARED: &[u8] =
        b"{\"resources\":[{\"kind\":\"test.undeclared.Note\",\"label\":\"Note\",\"version\":1}]}";
    collab_types::app_surface::register_package("so.tilo.contacts", CONTACTS);
    let messages = "so.tilo.messages";
    collab_types::app_surface::register_package(messages, MESSAGES);
    collab_types::app_surface::register_package("test.undeclared", UNDECLARED);
    let people = person_record("so.tilo.contacts", "");
    assert!(
        app_reach("test.undeclared", &people, true).is_err(),
        "an app that declares no read of Contacts' people"
    );
    let primary = collab_types::document::DocumentId::package_primary();
    assert_eq!(
        app_reach(messages, &person_record(messages, primary.as_str()), true),
        Ok(AppReach::Changes)
    );
    assert_eq!(
        app_reach(messages, &people, true),
        Ok(AppReach::Reads),
        "Contacts' people, read, never changed"
    );
    assert!(
        app_reach(
            messages,
            &person_record("so.tilo.notes", primary.as_str()),
            true
        )
        .is_err(),
        "another app's record it declares no read of"
    );
    let identity = collab_types::person_document_id("tilo.identity");
    assert!(
        app_reach(messages, &person_record(messages, &identity), true).is_err(),
        "a record of the person's it declares no kind of"
    );
}

#[test]
fn contextual_views_reach_the_profile_with_their_own_kind() {
    let app = "so.tilo.lang.views";
    let context = uuid::Uuid::from_u128(89);
    let document = collab_types::person_document_for_workspace("tl.so.tilo.lang.views", context);
    let views = person_record(app, &document);
    assert_eq!(persons_record(app, &views), Ok(()));
    assert!(persons_record("so.tilo.notes", &views).is_err());
}

/// The records the framework keeps for an app's windows, the default document a window opens and
/// the view a device keeps of it, are the app's own in every workspace it runs in, the person's
/// own among them. Another app reaches none of them; one that declares it reads the app's kinds
/// reads a window's document as any of its records, but never the view a device keeps of it.
#[test]
fn an_apps_windows_records_are_its_own_in_every_workspace() {
    collab_types::app_surface::register_package("test.lang-notes", NOTES);
    collab_types::app_surface::register_package("test.lang-board", BOARD);
    collab_types::app_surface::register_package("test.lang-chat", CHAT);
    let window = collab_types::document::DocumentId::window_default(0x2a);
    let first = collab_types::document::DocumentId::workspace_default();
    let view = format!("derived:view.d0a1b@{}", window.as_str());
    let records = [window.as_str(), first.as_str(), view.as_str()];
    let shared = |app: &str, record: &str| ResourceAddress {
        record: record.into(),
        ..workspace_record(app)
    };
    for record in records {
        for (own_workspace, address) in [
            (true, person_record("test.lang-notes", record)),
            (false, shared("test.lang-notes", record)),
        ] {
            assert_eq!(
                app_reach("test.lang-notes", &address, own_workspace),
                Ok(AppReach::Changes),
                "its own window's {record}, own workspace {own_workspace}"
            );
            assert!(
                app_reach("test.lang-chat", &address, own_workspace).is_err(),
                "not another app's window's {record}, own workspace {own_workspace}"
            );
        }
    }
    assert!(
        app_reach("test.lang-board", &shared("test.lang-notes", &view), false).is_err(),
        "a declared read of an app's kinds reaches no view of its windows"
    );
    let tab_view = "derived:view.d0a1b@tab:000000000000002a";
    assert!(
        !collab_types::document::DocumentId::is_window_record(tab_view),
        "a view of anything but a window's default document is no window's record"
    );
    assert!(
        app_reach(
            "test.lang-chat",
            &person_record("test.lang-notes", tab_view),
            true
        )
        .is_err(),
        "nor another app's to reach"
    );
}
