use super::*;
use std::fmt::Write;

fn fixture() -> String {
    let mut text = String::new();
    let mut add = |key: String, value: Value| {
        writeln!(
            text,
            "user_pref({}, {});",
            serde_json::to_string(&key).unwrap(),
            value
        )
        .unwrap();
    };
    add(
        "mail.accountmanager.accounts".into(),
        serde_json::json!("account1,account2,account3,account4"),
    );
    for index in 1..=4 {
        let account = format!("account{index}");
        let server = format!("server{index}");
        let identity = format!("id{index}");
        let smtp = format!("smtp{index}");
        let email = format!("person{index}@example.org");
        let host = if index == 3 {
            "outlook.office365.com"
        } else {
            "imap.example.org"
        };
        let username = if index == 2 {
            "mailbox-login".into()
        } else {
            email.clone()
        };
        add(
            format!("mail.account.{account}.server"),
            serde_json::json!(server),
        );
        add(
            format!("mail.account.{account}.identities"),
            serde_json::json!(identity),
        );
        for (key, value) in [
            ("type", serde_json::json!("imap")),
            ("hostname", serde_json::json!(host)),
            ("port", serde_json::json!(993)),
            ("userName", serde_json::json!(username)),
            ("socketType", serde_json::json!(3)),
        ] {
            add(format!("mail.server.{server}.{key}"), value);
        }
        for (key, value) in [
            ("useremail", serde_json::json!(email)),
            ("fullName", serde_json::json!("Example Person")),
            ("smtpServer", serde_json::json!(smtp)),
        ] {
            add(format!("mail.identity.{identity}.{key}"), value);
        }
        for (key, value) in [
            ("hostname", serde_json::json!("smtp.example.org")),
            (
                "port",
                serde_json::json!(if index == 3 { 587 } else { 465 }),
            ),
            ("username", serde_json::json!(username)),
            ("try_ssl", serde_json::json!(if index == 3 { 2 } else { 3 })),
        ] {
            add(format!("mail.smtpserver.{smtp}.{key}"), value);
        }
        if index == 3 {
            add(
                format!("mail.server.{server}.authMethod"),
                serde_json::json!(10),
            );
        }
    }
    text
}

#[test]
fn discovers_exact_endpoints_and_keeps_omitted_authentication_unspecified() {
    let accounts = parse(&fixture()).unwrap();
    assert_eq!(accounts.len(), 4);
    let custom = accounts
        .iter()
        .find(|item| item.source_account == "account2")
        .unwrap();
    assert_ne!(custom.email, custom.incoming.username);
    assert_eq!(custom.incoming.username, "mailbox-login");
    assert_eq!(custom.outgoing.username, "mailbox-login");
    assert_eq!(custom.incoming.authentication, None);
    assert_eq!(custom.outgoing.authentication, None);
    let config = custom.reviewed_config(AuthMethod::Password).unwrap();
    assert_eq!(config.username, "mailbox-login");
    assert!(config.password.is_empty());
    let microsoft = accounts
        .iter()
        .find(|item| item.source_account == "account3")
        .unwrap();
    assert_eq!(microsoft.provider(), Some("microsoft"));
    assert_eq!(microsoft.incoming.authentication, Some(10));
    assert_eq!(microsoft.outgoing.transport, Transport::StartTls);
    assert_eq!(microsoft.outgoing.port, 587);
    assert_eq!(
        microsoft.reviewed_config(AuthMethod::Password),
        Err(DiscoveryError::Unsupported)
    );
    assert_eq!(
        microsoft.reviewed_config(AuthMethod::OAuth).unwrap().auth,
        AuthMethod::OAuth
    );
}

#[test]
fn ignores_unrelated_preferences_before_parsing_their_values_and_never_executes_js() {
    let text = format!(
        "{}user_pref(\"mail.server.server1.password\", launch_a_program());\nuser_pref(\"extension.secret\", \"do-not-import\");\n",
        fixture()
    );
    let discovered = parse(&text).unwrap();
    assert_eq!(discovered, parse(&fixture()).unwrap());
    assert!(
        !serde_json::to_string(&discovered)
            .unwrap()
            .contains("do-not-import")
    );
}

#[test]
fn malformed_or_oversized_metadata_fails_without_echoing_values() {
    assert_eq!(
        parse("user_pref(\"mail.accountmanager.accounts\", execute());"),
        Err(DiscoveryError::Malformed)
    );
    assert_eq!(
        parse(&"x".repeat(2 * 1024 * 1024 + 1)),
        Err(DiscoveryError::TooLarge)
    );
}

#[test]
fn preserves_distinct_smtp_login_and_refuses_silent_native_conflation() {
    let text = fixture().replace(
        "user_pref(\"mail.smtpserver.smtp2.username\", \"mailbox-login\");",
        "user_pref(\"mail.smtpserver.smtp2.username\", \"outgoing-login\");",
    );
    let candidate = parse(&text)
        .unwrap()
        .into_iter()
        .find(|item| item.source_account == "account2")
        .unwrap();
    assert_eq!(candidate.incoming.username, "mailbox-login");
    assert_eq!(candidate.outgoing.username, "outgoing-login");
    assert_eq!(
        candidate.reviewed_config(AuthMethod::Password),
        Err(DiscoveryError::Unsupported)
    );
}

#[test]
fn unsupported_socket_types_do_not_become_plaintext_or_tls_by_default() {
    let text = fixture().replace(
        "user_pref(\"mail.smtpserver.smtp2.try_ssl\", 3);",
        "user_pref(\"mail.smtpserver.smtp2.try_ssl\", 1);",
    );
    assert_eq!(parse(&text), Err(DiscoveryError::Unsupported));
    let text = fixture().replace(
        "user_pref(\"mail.smtpserver.smtp2.try_ssl\", 3);",
        "user_pref(\"mail.smtpserver.smtp2.try_ssl\", 0);",
    );
    let candidate = parse(&text)
        .unwrap()
        .into_iter()
        .find(|item| item.source_account == "account2")
        .unwrap();
    assert_eq!(candidate.outgoing.transport, Transport::Plain);
    assert_eq!(
        candidate.reviewed_config(AuthMethod::Password),
        Err(DiscoveryError::Unsupported)
    );
}

#[test]
fn reading_metadata_never_opens_another_profile_file() {
    let root = std::env::temp_dir().join(format!("tilo-thunderbird-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let prefs = root.join("prefs.js");
    std::fs::write(&prefs, fixture()).unwrap();
    let other = root.join("logins.json");
    std::fs::write(&other, "not credential data: denied file fixture").unwrap();
    assert_eq!(discover(&other), Err(DiscoveryError::NotPreferencesFile));
    assert_eq!(discover(&prefs).unwrap(), parse(&fixture()).unwrap());
    #[cfg(unix)]
    {
        std::fs::remove_file(&prefs).unwrap();
        std::os::unix::fs::symlink(&other, &prefs).unwrap();
        assert_eq!(discover(&prefs), Err(DiscoveryError::NotPreferencesFile));
    }
    std::fs::remove_dir_all(root).unwrap();
}
