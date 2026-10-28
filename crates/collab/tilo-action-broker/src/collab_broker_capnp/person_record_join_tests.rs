use super::*;

fn route(app: &str, record: &str) -> CrdtRoute {
    CrdtRoute {
        app_id: app.into(),
        cluster_id: collab_types::person_room(&[7; 32]).to_string(),
        record_id: record.into(),
    }
}

/// An app joins the person's room for the records of theirs its surface
/// declares, its own or another app's it reads, and for nothing else.
#[test]
fn an_app_joins_the_persons_room_only_for_the_records_it_declares() {
    static TERMINAL: &[u8] = b"[[resources]]\nkind = \"test-person.session\"\nlabel = \"Session\"\nversion = 1\n\n[[resources]]\nkind = \"test-person.history\"\nlabel = \"History\"\nperson = true\nversion = 1\n\n[[resources]]\nkind = \"test-person.accounts\"\nlabel = \"Accounts\"\nperson = true\nversion = 1\n";
    ui::app_surface::register_package("test.person-terminal", TERMINAL);
    let history = collab_types::person_record_id("test-person.history");
    let accounts = collab_types::person_record_id("test-person.accounts");
    assert_eq!(
        person_record_join(
            "test.person-terminal",
            &route("test.person-terminal", &history)
        ),
        Ok(())
    );
    assert_eq!(
        person_record_join(
            "test.person-terminal",
            &route("test.person-settings", &accounts)
        ),
        Ok(()),
        "another app's record of the person's it declares, to read"
    );
    let undeclared = collab_types::person_record_id("test-person.bookmarks");
    assert!(
        person_record_join(
            "test.person-terminal",
            &route("test.person-terminal", &undeclared)
        )
        .is_err()
    );
    assert!(
        person_record_join(
            "test.person-terminal",
            &route("test.person-terminal", "tab:01")
        )
        .is_err(),
        "nothing but the person's records"
    );
    assert!(
        person_record_join(
            "test.person-notes",
            &route("test.person-terminal", &history)
        )
        .is_err(),
        "an app that declares none of them is refused"
    );
}
