use super::*;
use test_http::{Reply, Responder};

fn run<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn every_operation_rejects_another_origin_before_sending_credentials() {
    let trusted = Responder::start(vec![Reply::status(204)]);
    let attacker = Responder::silent();
    let client = Service::live()
        .authorised(false, &trusted.url(), "ada", "secret", "/cal/")
        .unwrap();
    run(async {
        assert!(
            client
                .report(&attacker.url(), Depth::One, "<report/>".into())
                .await
                .is_err()
        );
        assert!(
            client
                .propfind(&attacker.url(), Depth::Zero, &[])
                .await
                .is_err()
        );
        assert!(
            client
                .put(&attacker.url(), vec![], None, Precondition::None)
                .await
                .is_err()
        );
        assert!(
            client
                .delete(&attacker.url(), Precondition::None)
                .await
                .is_err()
        );
        assert_eq!(
            client
                .delete("/cal/one.ics", Precondition::IfMatch("\"v1\""))
                .await
                .unwrap()
                .status()
                .as_u16(),
            204
        );
    });
    let request = trusted.next_request();
    assert_eq!(request.target, "/cal/one.ics");
    assert_eq!(
        request.header("authorization"),
        Some("Basic YWRhOnNlY3JldA==")
    );
    assert_eq!(request.header("if-match"), Some("\"v1\""));
    assert_eq!(request.header("user-agent"), Some("Tilo-Calendar/0.1"));
    attacker.assert_untouched();
}

/// A Google calendar signs in with the access token the session released from its grant, at
/// its own server only; without one released it sends nothing at all.
#[cfg(feature = "test-support")]
#[test]
fn a_google_calendar_signs_in_with_its_released_grant_at_its_server_alone() {
    let trusted = Responder::start(vec![Reply::status(204)]);
    let attacker = Responder::silent();
    let service = Service::for_test_origin(&trusted.url()).unwrap();
    assert!(
        service
            .authorised(true, &attacker.url(), "", "calendar-access", "/")
            .is_err()
    );
    assert_eq!(
        service
            .authorised(true, &trusted.url(), "", "", "/")
            .err()
            .unwrap(),
        NO_GRANT
    );
    let client = service
        .authorised(true, &trusted.url(), "", "calendar-access", "/")
        .unwrap();
    run(async {
        assert!(
            client
                .report(&attacker.url(), Depth::One, "<report/>".into())
                .await
                .is_err()
        );
        client
            .report("/cal/", Depth::One, "<report/>".into())
            .await
            .unwrap();
    });
    assert_eq!(
        trusted.next_request().header("authorization"),
        Some("Bearer calendar-access")
    );
    attacker.assert_untouched();
}

#[test]
fn calendar_transport_keeps_its_deadline_and_refuses_redirects() {
    let options = options();
    assert_eq!(options.timeout, std::time::Duration::from_secs(20));
    assert_eq!(options.user_agent, Some("Tilo-Calendar/0.1"));
    assert_eq!(options.redirects, Redirects::Refuse);
}

#[test]
fn production_google_never_accepts_alternate_origin_or_endpoint() {
    let service = Service::live();
    let attacker = Responder::silent();
    for server in [
        attacker.url(),
        "https://apidata.googleusercontent.com/not-caldav".into(),
        "http://apidata.googleusercontent.com/caldav/v2/".into(),
    ] {
        let error = service
            .authorised(true, &server, "", "calendar-access", &server)
            .err()
            .unwrap();
        assert!(error.contains("Google CalDAV v2"), "{error}");
    }
    assert!(
        service
            .authorised(true, GOOGLE_SERVER, "", "calendar-access", &attacker.url())
            .is_err()
    );
    attacker.assert_untouched();
}

#[test]
fn redirects_never_forward_the_password() {
    let attacker = Responder::silent();
    let trusted = Responder::start(vec![Reply::status(302).header("Location", &attacker.url())]);
    let client = Service::live()
        .authorised(false, &trusted.url(), "ada", "secret", "/")
        .unwrap();
    run(async {
        assert_eq!(
            client
                .report("/", Depth::One, "<report/>".into())
                .await
                .unwrap()
                .status()
                .as_u16(),
            302
        );
    });
    assert!(trusted.next_request().header("authorization").is_some());
    attacker.assert_untouched();
}
