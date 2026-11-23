use super::*;

fn route(app: &str, record: &str) -> CrdtRoute {
    CrdtRoute {
        app_id: app.into(),
        cluster_id: collab_types::person_workspace(&[7; 32]).to_string(),
        document_id: record.into(),
    }
}

/// An app joins the person's workspace for the records of theirs its surface
/// declares, its own or another app's it reads, and for nothing else.
#[test]
fn an_app_joins_the_persons_workspace_only_for_the_records_it_declares() {
    static SETTINGS: &[u8] =
        b"[[resources]]\nkind = \"tilo.accounts\"\nlabel = \"Accounts\"\nversion = 1\n";
    static MAIL: &[u8] = b"reads = [\"tilo.accounts\"]\n\n[[resources]]\nkind = \"test-mail.Message\"\nlabel = \"Message\"\nversion = 1\n";
    static TERMINAL: &[u8] = b"[[resources]]\nkind = \"test-person.session\"\nlabel = \"Session\"\nversion = 1\n\n[[resources]]\nkind = \"test-person.history\"\nlabel = \"History\"\nperson = true\nversion = 1\n\n[[resources]]\nkind = \"test-person.accounts\"\nlabel = \"Accounts\"\nperson = true\nversion = 1\n";
    collab_types::app_surface::register_package("test.person-terminal", TERMINAL);
    let history = collab_types::person_document_id("test-person.history");
    let accounts = collab_types::person_document_id("test-person.accounts");
    assert_eq!(
        person_document_join(
            "test.person-terminal",
            &route("test.person-terminal", &history)
        ),
        Ok(())
    );
    assert_eq!(
        person_document_join(
            "test.person-terminal",
            &route("test.person-settings", &accounts)
        ),
        Ok(()),
        "another app's record of the person's it declares, to read"
    );
    let undeclared = collab_types::person_document_id("test-person.bookmarks");
    assert!(
        person_document_join(
            "test.person-terminal",
            &route("test.person-terminal", &undeclared)
        )
        .is_err()
    );
    assert!(
        person_document_join(
            "test.person-terminal",
            &route("test.person-terminal", "tab:01")
        )
        .is_err(),
        "nothing but the person's records"
    );
    assert_eq!(
        person_document_join("test.person-terminal", &route("test.person-terminal", "")),
        Ok(()),
        "its own route naming no record, as every app opens on"
    );
    assert!(
        person_document_join("test.person-notes", &route("test.person-terminal", "")).is_err(),
        "not another app's"
    );
    collab_types::app_surface::register_package("so.tilo.settings", SETTINGS);
    collab_types::app_surface::register_package("test.person-mail", MAIL);
    let accounts_list = route(
        "so.tilo.settings",
        &collab_types::workspace_document_id("tilo.accounts"),
    );
    assert_eq!(
        person_document_join("so.tilo.settings", &accounts_list),
        Ok(()),
        "the workspace's account list, by the app it belongs to"
    );
    assert_eq!(
        person_document_join("test.person-mail", &accounts_list),
        Ok(()),
        "and by an app that declares it reads it"
    );
    assert!(
        person_document_join("test.person-terminal", &accounts_list).is_err(),
        "but not by an app that declares no read of it"
    );
    assert!(
        person_document_join("test.person-notes", &route("test.person-notes", &history)).is_err(),
        "but still no record of the person's of a kind it does not declare"
    );
    assert!(
        person_document_join(
            "test.person-notes",
            &route("test.person-terminal", &history)
        )
        .is_err(),
        "an app that declares none of them is refused"
    );
}

#[test]
fn contextual_views_can_join_the_profile_with_their_own_kind() {
    let app = "so.tilo.lang.views";
    let context = uuid::Uuid::from_u128(89);
    let document = collab_types::person_document_for_workspace("tl.so.tilo.lang.views", context);
    let views = route(app, &document);
    assert_eq!(person_document_join(app, &views), Ok(()));
    assert!(person_document_join("so.tilo.notes", &views).is_err());
}
