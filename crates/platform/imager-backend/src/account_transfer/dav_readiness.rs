//! DAV readiness runs in the native backend before custody is published.
#[cfg(test)]
use super::ConnectionError;
pub(super) use crate::account_dav_readiness::verify;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
pub(crate) fn test_server() -> test_http::Responder {
    test_http::Responder::handle(|request| {
        assert_eq!(request.method, "PROPFIND");
        assert_eq!(request.header("depth"), Some("0"));
        assert!(request.header("authorization").is_some());
        Some(test_http::Reply::status(207).body(collection(&request.target)))
    })
}
#[cfg(test)]
pub(crate) fn collection(href: &str) -> String {
    format!(
        "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>{href}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_transfer::{AccountActions, AccountJobState, Connector, DavConnection};
    use std::sync::{Arc, Mutex};
    use test_http::{Reply, Responder};
    use zeroize::Zeroizing;
    const PASSWORD: &str = "PRIVATE_DAV_CREDENTIAL_SENTINEL";
    fn connect(
        root: &std::path::Path,
        server: &str,
    ) -> Result<accounts::AccountId, ConnectionError> {
        Connector::new(root.to_owned()).connect_webdav(
            "Files",
            server,
            "ada",
            Zeroizing::new(PASSWORD.into()),
        )
    }
    #[test]
    fn explicit_dav_readiness_checks_actual_root_before_sealed_custody() {
        let targets = Arc::new(Mutex::new(Vec::new()));
        let seen = targets.clone();
        let server = Responder::handle(move |request| {
            assert_eq!(request.method, "PROPFIND");
            assert_eq!(request.header("depth"), Some("0"));
            let expected = if request.target.starts_with("/base/") {
                "Basic YWRhIHNwYWNlOlBSSVZBVEVfREFWX0NSRURFTlRJQUxfU0VOVElORUw="
            } else {
                "Basic YWRhOlBSSVZBVEVfREFWX0NSRURFTlRJQUxfU0VOVElORUw="
            };
            assert_eq!(request.header("authorization"), Some(expected));
            seen.lock().unwrap().push(request.target.clone());
            Some(Reply::status(207).body(collection(&request.target)))
        });
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("webdav");
        let id = connect(&root, &format!("{}/files", server.url())).unwrap();
        let vault = accounts::SecretVault::open_in(&root);
        assert_eq!(
            account_custody::password_credential(&vault.get(&id).unwrap()).as_str(),
            PASSWORD
        );
        let next = temporary.path().join("nextcloud");
        Connector::new(next)
            .connect_nextcloud_app_password(
                "Files",
                account_custody::nextcloud::NextcloudLogin {
                    server: format!("{}/base", server.url()),
                    login_name: "ada space".into(),
                    app_password: Zeroizing::new(PASSWORD.into()),
                },
            )
            .unwrap();
        assert_eq!(
            *targets.lock().unwrap(),
            ["/files/", "/base/remote.php/dav/files/ada%20space/"]
        );
        assert!(
            !std::fs::read_to_string(accounts::store_path_in(&root))
                .unwrap()
                .contains(PASSWORD)
        );
        assert!(
            !String::from_utf8_lossy(
                &std::fs::read(accounts::vault_dir_in(&root).join("vault.enc")).unwrap()
            )
            .contains(PASSWORD)
        );
    }
    #[test]
    fn explicit_dav_refuses_auth_offline_and_unusable_root_without_persistence() {
        let cases = [
            (
                401,
                "credential rejection sentinel",
                ConnectionError::DavAuthenticationRefused,
            ),
            (403, "no access", ConnectionError::DavFilesUnavailable),
            (
                503,
                "temporary unavailable",
                ConnectionError::DavEndpointUnavailable,
            ),
            (
                207,
                "<html>PRIVATE_DAV_CREDENTIAL_SENTINEL</html>",
                ConnectionError::DavFilesUnavailable,
            ),
            (
                207,
                "<d:multistatus xmlns:d=\"DAV:\"/>",
                ConnectionError::DavFilesUnavailable,
            ),
            (
                207,
                "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/other/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>",
                ConnectionError::DavFilesUnavailable,
            ),
            (
                302,
                "redirect refused",
                ConnectionError::DavFilesUnavailable,
            ),
        ];
        let temporary = tempfile::tempdir().unwrap();
        for (index, (status, body, reason)) in cases.into_iter().enumerate() {
            let server = Responder::handle(move |request| {
                assert_eq!(request.method, "PROPFIND");
                Some(Reply::status(status).body(body))
            });
            let root = temporary.path().join(index.to_string());
            assert_eq!(connect(&root, &server.url()), Err(reason));
            assert!(!root.exists());
            assert!(!format!("{reason:?}").contains(PASSWORD));
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let root = temporary.path().join("offline");
        assert_eq!(
            connect(&root, &endpoint),
            Err(ConnectionError::DavEndpointUnavailable)
        );
        assert!(!root.exists());
    }
    #[test]
    fn explicit_dav_worker_reports_auth_refusal_and_preserves_non_cancellable_write_contract() {
        let server = Responder::handle(|request| {
            assert_eq!(request.method, "PROPFIND");
            Some(Reply::status(401))
        });
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        std::fs::create_dir(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut actions = AccountActions::default();
        let job = actions
            .connect_dav(
                &home,
                DavConnection {
                    label: "Files".into(),
                    server: server.url(),
                    username: "ada".into(),
                    password: Zeroizing::new(PASSWORD.into()),
                    nextcloud: false,
                },
            )
            .unwrap();
        assert!(!actions.cancel_job(&job));
        let until = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            match actions
                .status()
                .into_iter()
                .find(|receipt| receipt.id == job)
                .unwrap()
                .state
            {
                AccountJobState::Running => {
                    assert!(std::time::Instant::now() < until);
                    std::thread::sleep(Duration::from_millis(5));
                }
                AccountJobState::Failed { reason: error } => {
                    assert_eq!(error, ConnectionError::DavAuthenticationRefused);
                    break;
                }
                other => panic!("unexpected non-secret job state: {other:?}"),
            }
        }
        assert!(!accounts::store_path_in(&home.join(".config/tilo")).exists());
    }
}
