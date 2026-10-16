use super::*;

fn pending() -> AccountConfig {
    AccountConfig {
        email: "person@outlook.de".into(),
        imap_host: "outlook.office365.com".into(),
        auth: AuthMethod::OAuth,
        import_pending: true,
        ..Default::default()
    }
}

#[test]
fn imported_outlook_uses_its_host_and_requires_matching_fresh_oauth_completion() {
    let fields = SetupFields::of(&pending());
    assert_eq!(fields.provider(), Some(OAuthProvider::Microsoft));
    assert!(fields.config().import_pending);
    fields.complete_oauth("person@outlook.de");
    assert!(
        fields.config().import_pending,
        "an unrequested completion cannot activate imported metadata"
    );
    fields.expect_oauth();
    fields.set_value(IMAP_HOST, "different.example.org");
    fields.complete_oauth("person@outlook.de");
    assert!(
        fields.config().import_pending,
        "a changed endpoint fences the old handoff"
    );
    fields.set_value(IMAP_HOST, "outlook.office365.com");
    fields.expect_oauth();
    fields.complete_oauth("other@outlook.de");
    assert!(
        fields.config().import_pending,
        "a different account cannot complete this handoff"
    );
    fields.complete_oauth("person@outlook.de");
    assert!(!fields.config().import_pending);
}

#[test]
fn imported_microsoft_account_cannot_clear_pending_by_switching_to_password() {
    let fields = SetupFields::of(&pending());
    fields.set_auth(AuthMethod::Password);
    fields.set_value(PASSWORD, "fixture-new-password");
    assert!(fields.config().import_pending);
}
