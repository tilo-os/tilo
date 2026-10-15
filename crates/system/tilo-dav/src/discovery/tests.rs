use super::*;
use crate::{Credentials, Options, Redirects};
use test_http::{Reply, Responder};

fn xml(href: &str, properties: &str) -> String {
    format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>{href}</d:href><d:propstat><d:prop>{properties}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
    )
}

fn client() -> DavClient {
    DavClient::new(&Options {
        redirects: Redirects::Refuse,
        ..Options::default()
    })
    .unwrap()
    .with_credentials(Credentials::new("login@example.test", "app-password"))
}

#[test]
fn subpath_well_known_and_different_principal_discover_multiple_calendars() {
    let principal = xml(
        "/cloud/dav/",
        "<d:current-user-principal>\n<d:href>/cloud/principals/internal-id/</d:href></d:current-user-principal>",
    );
    let home = xml(
        "/cloud/principals/internal-id/",
        "<c:calendar-home-set><d:href>/cloud/calendars/internal-id/</d:href></c:calendar-home-set>",
    );
    let writable = xml(
        "/cloud/calendars/internal-id/work/",
        "<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Work &amp; Team</d:displayname><d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>",
    );
    let readonly = xml(
        "/cloud/calendars/internal-id/holidays/",
        "<d:resourcetype><c:calendar/></d:resourcetype><d:displayname>Holidays</d:displayname><d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set>",
    );
    let listing = writable.replace(
        "</d:multistatus>",
        &format!(
            "{}</d:multistatus>",
            readonly
                .split("<d:response>")
                .nth(1)
                .unwrap()
                .replace("</d:multistatus>", "")
                .replacen("<d:href>", "<d:response><d:href>", 1)
        ),
    );
    let server = Responder::start(vec![
        Reply::status(404),
        Reply::status(301).header("Location", "/cloud/dav/"),
        Reply::status(207).body(principal),
        Reply::status(207).body(home),
        Reply::status(207).body(listing),
    ]);
    let found = calendars(
        &client(),
        &Url::parse(&format!("{}/cloud", server.url())).unwrap(),
    )
    .unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].name, "Work & Team");
    assert!(!found[0].read_only);
    assert!(found[1].read_only);
    assert!(
        found[1]
            .href
            .ends_with("/cloud/calendars/internal-id/holidays/")
    );
    for (path, depth) in [
        ("/cloud", "0"),
        ("/cloud/.well-known/caldav", "0"),
        ("/cloud/dav/", "0"),
        ("/cloud/principals/internal-id/", "0"),
        ("/cloud/calendars/internal-id/", "1"),
    ] {
        let request = server.next_request();
        assert_eq!(request.target, path);
        assert_eq!(request.method, "PROPFIND");
        assert_eq!(request.header("depth"), Some(depth));
        assert!(request.header("authorization").is_some());
    }
}

#[test]
fn discovery_refuses_clients_that_could_follow_unchecked_redirects() {
    let server = Responder::silent();
    let client = DavClient::new(&Options::default())
        .unwrap()
        .with_credentials(Credentials::new("ada", "secret"));
    assert!(
        calendars(&client, &Url::parse(&server.url()).unwrap())
            .unwrap_err()
            .contains("automatic redirects disabled")
    );
    server.assert_untouched();
}

#[test]
fn hostile_discovery_fails_without_contacting_unsafe_targets() {
    let target = Responder::silent();
    for href in [
        format!("{}/steal", target.url()),
        "https://other.example/principal".into(),
        "/cloud/%2e%2e/private".into(),
        "/cloud/bad%2fpath".into(),
        "/cloud/principal?secret=x".into(),
    ] {
        let server = Responder::start(vec![Reply::status(207).body(xml(
            "/cloud/",
            &format!(
                "<d:current-user-principal><d:href>{href}</d:href></d:current-user-principal>"
            ),
        ))]);
        assert!(
            calendars(
                &client(),
                &Url::parse(&format!("{}/cloud/", server.url())).unwrap()
            )
            .is_err()
        );
    }
    let server = Responder::start(vec![Reply::status(302).header("Location", &target.url())]);
    assert!(calendars(&client(), &Url::parse(&server.url()).unwrap()).is_err());
    target.assert_untouched();
}

#[test]
fn auth_empty_malformed_oversized_and_redirect_loops_are_not_connected() {
    for reply in [
        Reply::status(401),
        Reply::status(403),
        Reply::status(207).body("<broken"),
        Reply::status(207).body(vec![b'x'; 1024 * 1024 + 1]),
    ] {
        let server = Responder::start(vec![reply]);
        assert!(calendars(&client(), &Url::parse(&server.url()).unwrap()).is_err());
    }
    let empty_home = xml("/cal/", "<d:resourcetype><d:collection/></d:resourcetype>");
    let server = Responder::start(vec![
        Reply::status(207).body(empty_home.clone()),
        Reply::status(207).body(empty_home),
    ]);
    assert!(
        calendars(
            &client(),
            &Url::parse(&format!("{}/cal/", server.url())).unwrap()
        )
        .unwrap_err()
        .contains("No calendars")
    );
    let server = Responder::start(
        (0..4)
            .map(|_| Reply::status(302).header("Location", "/loop"))
            .collect(),
    );
    assert!(
        calendars(&client(), &Url::parse(&server.url()).unwrap())
            .unwrap_err()
            .contains("redirect limit")
    );
}
