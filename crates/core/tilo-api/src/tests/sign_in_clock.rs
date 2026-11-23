//! A person's approval when their computer's clock and this server's differ.
//!
//! The page of the person's Tilo dates what it signs by the browser's clock,
//! and the server reads it by its own. These tests hold both: the server's
//! at one moment, the page's a chosen time ahead of it or behind. The page
//! answers by the calls its sign-in route makes (`tilo-browser-runtime`'s
//! `sign_in_page`), and the server is asked as a browser asks it.

use super::*;
use futures_util::FutureExt as _;
use tilo_server_client::certification::{issue_agent_grant, issue_sign_in_cert};
use tilo_server_client::{grant_label, sign_in_proof};
use tilo_user_identity::certification::CERTIFICATION_CLOCK_ALLOWANCE_NS;
use tilo_user_identity::{CertificationPolicy, RevocationList};

/// The server's clock in these tests, in Unix milliseconds.
const SERVER_MS: u64 = 1_800_000_000_000;
/// How far two clocks may differ, in milliseconds: the one rule, the
/// certification of a paired device's.
const RULE_MS: u64 = CERTIFICATION_CLOCK_ALLOWANCE_NS / 1_000_000;

/// `flow` with this thread's clock, the server's, held at [`SERVER_MS`].
fn at_the_server<T>(flow: impl Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    tilo_clock::with_wall_ms(SERVER_MS, || runtime.block_on(flow))
}

/// What the page of a person's Tilo answers a sign-in with.
struct PageAnswer {
    proof: SignInProof,
    grant: Option<DeviceCert>,
}

impl PageAnswer {
    /// The form the page posts back for the sign-in `pending`.
    fn form(&self, pending: &str) -> String {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("pending", pending)
            .append_pair("proof", &sign_in::encode_proof(&self.proof).unwrap());
        if let Some(grant) = &self.grant {
            form.append_pair("grant", &sign_in::encode_cert(grant).unwrap());
        }
        form.finish()
    }

    /// Why the server's check of the proof refuses it at the server's clock.
    fn proof_refused(&self) -> Option<String> {
        sign_in::verify(
            &self.proof,
            ISSUER,
            &self.proof.challenge,
            &RevocationList::new(),
            SERVER_MS * 1_000_000,
        )
        .err()
        .map(|error| error.to_string())
    }

    /// Why the grant's certificate does not hold at the server's clock.
    fn grant_refused(&self) -> Option<String> {
        let grant = self.grant.as_ref()?;
        tilo_user_identity::verify_device_cert_at(grant, SERVER_MS * 1_000_000)
            .err()
            .map(|error| error.to_string())
    }
}

/// The page of `person`'s Tilo, its clock at `page_ms`, answers `challenge`
/// and certifies the grant `asked` names: the calls of the page's sign-in
/// route, in its order, with the root and the device key the page holds.
fn page_answers(
    person: &Person,
    challenge: [u8; 32],
    asked: Option<&Asked>,
    page_ms: u64,
) -> PageAnswer {
    let answer = async {
        let cert = issue_sign_in_cert(
            &person.root,
            person.device.verifying_key().to_bytes(),
            "Tilo on the web",
        )
        .await
        .unwrap();
        let proof = sign_in_proof(cert, &person.device, ISSUER, challenge)
            .await
            .unwrap();
        let grant = match asked {
            Some(asked) => {
                let capabilities = if asked.server_key {
                    DeviceCapabilities::server_key_grant()
                } else {
                    DeviceCapabilities::agent_grant(asked.scope)
                };
                let grant = issue_agent_grant(
                    &person.root,
                    asked.key,
                    &grant_label(&asked.client, ISSUER),
                    capabilities.with_browser_pages(asked.browser_pages),
                    asked.expires_ms.map(|ms| ms.saturating_mul(1_000_000)),
                )
                .await
                .unwrap();
                Some(grant)
            }
            None => None,
        };
        PageAnswer { proof, grant }
    };
    tilo_clock::with_wall_ms(page_ms, || {
        answer.now_or_never().expect("a held key signs at once")
    })
}

/// A browser nobody is signed in at opens the grants page: the sign-in the
/// server sends it to, and its challenge.
async fn grants_page_asks(server: &Arc<Server>) -> (String, [u8; 32]) {
    let reply = server
        .dispatch(&Method::GET, "/grants", "", &headers(&[]), b"")
        .await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    let asked = query_of(&location(&reply));
    let challenge = hex::decode(&asked["challenge"])
        .unwrap()
        .try_into()
        .unwrap();
    (asked["pending"].clone(), challenge)
}

/// What a refusal's page says, for a failed assertion's words.
async fn page_text(reply: Reply) -> String {
    String::from_utf8(body(reply).await).unwrap()
}

#[test]
fn an_agent_is_approved_from_a_computer_whose_clock_is_ahead_within_the_rule() {
    at_the_server(async {
        let alice = Person::new(1);
        let server = server();
        let client = register(&server).await;
        for ahead_ms in [10, 5_000, RULE_MS] {
            let asked = authorize(&server, &client, ISSUER).await;
            let page = page_answers(&alice, asked.challenge, Some(&asked), SERVER_MS + ahead_ms);
            let reply = answer(&server, &page.form(&asked.pending)).await;
            assert_eq!(
                reply.status(),
                StatusCode::SEE_OTHER,
                "the page's clock {ahead_ms} ms ahead: the proof {:?}, the grant {:?}: {}",
                page.proof_refused(),
                page.grant_refused(),
                page_text(reply).await
            );
            let code = returned(&reply)["code"].clone();
            let tokens = json_body(redeem(&server, &client, &code, VERIFIER).await).await;
            assert!(
                tokens["access_token"].is_string(),
                "{ahead_ms} ms ahead: no token"
            );
        }
        assert_eq!(server.store.grants_of(&alice.id()).len(), 3);
    });
}

#[test]
fn a_key_is_approved_from_a_computer_whose_clock_is_ahead_within_the_rule() {
    at_the_server(async {
        let alice = Person::new(1);
        let server = server();
        for ahead_ms in [10, RULE_MS] {
            let (poll, asked) = ask_for_key(&server, &json!({"name": "Visa backend"})).await;
            let page = page_answers(&alice, asked.challenge, Some(&asked), SERVER_MS + ahead_ms);
            let reply = answer(&server, &page.form(&asked.pending)).await;
            assert!(
                reply.status().is_success(),
                "the page's clock {ahead_ms} ms ahead: the proof {:?}, the grant {:?}: {}",
                page.proof_refused(),
                page.grant_refused(),
                page_text(reply).await
            );
            let (status, polled) = poll_key(&server, &poll).await;
            assert_eq!(status, StatusCode::OK, "{ahead_ms} ms ahead");
            assert!(polled["key"].is_string(), "{ahead_ms} ms ahead: no key");
        }
    });
}

#[test]
fn the_grants_page_signs_in_from_a_computer_whose_clock_is_ahead_within_the_rule() {
    at_the_server(async {
        let alice = Person::new(3);
        let server = server();
        for ahead_ms in [10, RULE_MS] {
            let (pending, challenge) = grants_page_asks(&server).await;
            let page = page_answers(&alice, challenge, None, SERVER_MS + ahead_ms);
            let reply = answer(&server, &page.form(&pending)).await;
            assert_eq!(
                reply.status(),
                StatusCode::SEE_OTHER,
                "the page's clock {ahead_ms} ms ahead: the proof {:?}: {}",
                page.proof_refused(),
                page_text(reply).await
            );
            assert_eq!(location(&reply), "/grants");
            assert!(reply.headers().contains_key(header::SET_COOKIE));
        }
    });
}

#[test]
fn an_approval_dated_further_ahead_than_the_rule_stays_refused() {
    at_the_server(async {
        let alice = Person::new(1);
        let server = server();
        let client = register(&server).await;
        let beyond_ms = SERVER_MS + RULE_MS + 1;
        let not_yet = "device cert is not yet valid";

        let asked = authorize(&server, &client, ISSUER).await;
        let page = page_answers(&alice, asked.challenge, Some(&asked), beyond_ms);
        assert_eq!(
            page.proof_refused(),
            Some(format!("the device's certificate does not hold: {not_yet}"))
        );
        assert_eq!(page.grant_refused().as_deref(), Some(not_yet));
        let reply = answer(&server, &page.form(&asked.pending)).await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN);
        assert!(server.store.grants_of(&alice.id()).is_empty());

        let (poll, asked) = ask_for_key(&server, &json!({"name": "Visa backend"})).await;
        let page = page_answers(&alice, asked.challenge, Some(&asked), beyond_ms);
        let reply = answer(&server, &page.form(&asked.pending)).await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN);
        assert_ne!(poll_key(&server, &poll).await.0, StatusCode::OK);
        assert!(server.store.grants_of(&alice.id()).is_empty());

        let (pending, challenge) = grants_page_asks(&server).await;
        let page = page_answers(&alice, challenge, None, beyond_ms);
        let reply = answer(&server, &page.form(&pending)).await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN);
        assert!(!reply.headers().contains_key(header::SET_COOKIE));
    });
}

#[test]
fn a_sign_in_whose_certificate_ran_out_stays_refused_and_a_clock_behind_is_no_fault() {
    at_the_server(async {
        let alice = Person::new(3);
        let server = server();
        let lifetime_ms = CertificationPolicy::browser().lifetime_ns / 1_000_000;

        // A computer an hour behind the server: what it signs is an hour
        // old here, and holds.
        let (pending, challenge) = grants_page_asks(&server).await;
        let page = page_answers(&alice, challenge, None, SERVER_MS - 60 * 60 * 1000);
        assert_eq!(page.proof_refused(), None);
        let reply = answer(&server, &page.form(&pending)).await;
        assert_eq!(reply.status(), StatusCode::SEE_OTHER);
        assert!(reply.headers().contains_key(header::SET_COOKIE));

        // One as far behind as a browser's certificate lasts: the
        // certificate has ended by the server's clock.
        let (pending, challenge) = grants_page_asks(&server).await;
        let page = page_answers(&alice, challenge, None, SERVER_MS - lifetime_ms);
        assert_eq!(
            page.proof_refused().as_deref(),
            Some("the device's certificate does not hold: device cert has expired")
        );
        let reply = answer(&server, &page.form(&pending)).await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN);
        assert!(!reply.headers().contains_key(header::SET_COOKIE));
    });
}
