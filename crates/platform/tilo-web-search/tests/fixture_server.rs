//! What a fetch makes of each kind of body, read from a fixture server on
//! this machine: JSON comes back as indented text, HTML as its readable
//! text, and a PDF (labelled, or sent with no type at all) whole, as bytes
//! for the caller to keep. A binary body larger than the limit is refused in
//! plain words rather than cut.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tilo_web_search::fixtures::{FixtureServer, Route};
use tilo_web_search::{
    FetchError, FetchLimits, FetchedBody, Refusal, fetch, fetch_fixture, fetch_looking_up,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const PDF: &[u8] = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<< /Type /Catalog >>\nendobj\n\x00\x01\x02trailer\n%%EOF\n";

/// Serve each request for one of `routes` (path, content type, body) until
/// the test ends; the address it listens on.
async fn serve(routes: Vec<(&'static str, &'static str, Vec<u8>)>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(read) = stream.read(&mut buffer).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(buffer.get(..read).unwrap_or_default());
                }
                let line = String::from_utf8_lossy(&request);
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (head, body) = match routes.iter().find(|(route, _, _)| *route == path) {
                    Some((_, kind, body)) => {
                        let kind = if kind.is_empty() {
                            String::new()
                        } else {
                            format!("Content-Type: {kind}\r\n")
                        };
                        (format!("HTTP/1.1 200 OK\r\n{kind}"), body.clone())
                    }
                    None => ("HTTP/1.1 404 Not Found\r\n".to_owned(), Vec::new()),
                };
                let head = format!(
                    "{head}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://{address}")
}

fn limits() -> FetchLimits {
    FetchLimits {
        timeout: Duration::from_secs(10),
        ..FetchLimits::default()
    }
}

#[tokio::test]
async fn json_html_and_pdf_bodies_from_a_fixture_server() {
    let base = serve(vec![
        (
            "/users/ada/repos",
            "application/json; charset=utf-8",
            br#"[{"name":"engine","stargazers_count":12,"fork":false}]"#.to_vec(),
        ),
        (
            "/",
            "text/html",
            b"<!doctype html><html><head><title>Ada Lovelace</title></head><body><main><h1>Ada</h1><p>I build analytical engines for a living, and write about them.</p></main></body></html>".to_vec(),
        ),
        ("/cv.pdf", "application/pdf", PDF.to_vec()),
        ("/unlabelled", "", PDF.to_vec()),
    ])
    .await;
    let client = tilo_web_search::client().unwrap();

    let json = fetch_fixture(&client, &format!("{base}/users/ada/repos"), &limits())
        .await
        .unwrap();
    assert_eq!(json.content_type, "application/json");
    assert_eq!(
        json.text(),
        Some(
            "[\n  {\n    \"name\": \"engine\",\n    \"stargazers_count\": 12,\n    \"fork\": false\n  }\n]"
        ),
        "JSON is indented so its structure reads"
    );

    let html = fetch_fixture(&client, &format!("{base}/"), &limits())
        .await
        .unwrap();
    assert_eq!(html.title(), Some("Ada Lovelace"));
    assert!(
        html.text().unwrap().contains("analytical engines"),
        "{html:?}"
    );

    for path in ["/cv.pdf", "/unlabelled"] {
        let pdf = fetch_fixture(&client, &format!("{base}{path}"), &limits())
            .await
            .unwrap();
        assert!(pdf.text().is_none(), "{path} is not text");
        assert_eq!(
            pdf.body,
            FetchedBody::Binary {
                bytes: PDF.to_vec(),
                len: PDF.len(),
            },
            "{path} comes back whole"
        );
        assert!(!pdf.truncated);
    }

    let small = FetchLimits {
        max_bytes: 16,
        ..limits()
    };
    let refused = fetch_fixture(&client, &format!("{base}/cv.pdf"), &small)
        .await
        .unwrap_err();
    assert_eq!(refused, FetchError::TooLarge(16));
    assert!(!refused.retryable());

    // The service's own fetch still never reaches this machine.
    assert!(matches!(
        fetch(&client, &format!("{base}/cv.pdf"), &limits()).await,
        Err(FetchError::Refused(_))
    ));
}

/// A resolver the test stands in for a name's own DNS: each lookup takes the
/// next of `answers`, `None` for a lookup that fails, and the last of them
/// from then on. It counts its lookups.
fn stand_in(answers: Vec<Option<Vec<IpAddr>>>) -> (tilo_net::Lookup, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let lookup: tilo_net::Lookup = Arc::new(move |_name: String| {
        let at = counted.fetch_add(1, Ordering::SeqCst);
        let answer = answers
            .get(at.min(answers.len().saturating_sub(1)))
            .cloned()
            .flatten();
        Box::pin(
            async move { answer.ok_or_else(|| std::io::Error::other("the name does not resolve")) },
        )
    });
    (lookup, asked)
}

/// A client that dials whatever `lookup` answers, as every client did
/// before the fetch's own judged its lookup.
fn unjudged(lookup: &tilo_net::Lookup) -> tilo_net::Client {
    tilo_net::Client::builder()
        .redirects(tilo_net::Redirects::None)
        .lookup(Arc::clone(lookup))
        .build()
        .unwrap()
}

/// A page this machine keeps, and the address a name no resolver knows
/// would reach it by.
fn kept_here() -> (FixtureServer, String) {
    let server = FixtureServer::serve(vec![Route::new(
        "/kept",
        "text/plain",
        "this machine's own",
    )]);
    let port = url::Url::parse(&server.at("/kept"))
        .unwrap()
        .port()
        .unwrap();
    let named = format!("http://rebound.example.test:{port}/kept");
    (server, named)
}

const HERE: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// An address on the public internet, which no test dials.
const PUBLIC: IpAddr = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));

#[tokio::test]
async fn a_name_answering_a_public_address_and_then_this_machine_is_not_fetched() {
    let (server, named) = kept_here();
    let rebinding = || stand_in(vec![Some(vec![PUBLIC]), Some(vec![HERE])]);

    // As the fetch stood: the look before the request finds the name
    // public, the client resolves it again unjudged, and what this machine
    // keeps is read.
    let (lookup, _) = rebinding();
    let read = fetch_looking_up(&unjudged(&lookup), &named, &limits(), lookup)
        .await
        .unwrap();
    assert_eq!(read.text(), Some("this machine's own"));
    assert_eq!(server.requests().len(), 1);

    // The fetch's own client judges the lookup its connection uses: the
    // same two answers, and nothing is sent.
    let (lookup, asked) = rebinding();
    let client = tilo_web_search::client_looking_up(Arc::clone(&lookup)).unwrap();
    let refused = fetch_looking_up(&client, &named, &limits(), lookup)
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        FetchError::Refused(Refusal::Local("rebound.example.test".into()))
    );
    assert_eq!(server.requests().len(), 1, "nothing more was sent");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        2,
        "the look before the request and the connection's own lookup, and no third behind them"
    );

    // A name that answers a public address beside a local one is refused
    // whole: the connection might take either.
    let (lookup, _) = stand_in(vec![Some(vec![PUBLIC]), Some(vec![PUBLIC, HERE])]);
    let client = tilo_web_search::client_looking_up(Arc::clone(&lookup)).unwrap();
    let refused = fetch_looking_up(&client, &named, &limits(), lookup)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, FetchError::Refused(Refusal::Local(_))),
        "{refused:?}"
    );
    assert_eq!(server.requests().len(), 1, "nothing more was sent");
}

#[tokio::test]
async fn a_name_whose_first_lookup_fails_is_not_dialled_unjudged() {
    let (server, named) = kept_here();
    let failing_first = || stand_in(vec![None, Some(vec![HERE])]);

    // As the fetch stood: a lookup that fails is no refusal, the client
    // resolves the name itself, and what this machine keeps is read.
    let (lookup, _) = failing_first();
    let read = fetch_looking_up(&unjudged(&lookup), &named, &limits(), lookup)
        .await
        .unwrap();
    assert_eq!(read.text(), Some("this machine's own"));
    assert_eq!(server.requests().len(), 1);

    // A lookup that fails is still no refusal, as a sandbox with no
    // resolver needs it. What the connection then resolves is judged.
    let (lookup, _) = failing_first();
    let client = tilo_web_search::client_looking_up(Arc::clone(&lookup)).unwrap();
    let refused = fetch_looking_up(&client, &named, &limits(), lookup)
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        FetchError::Refused(Refusal::Local("rebound.example.test".into()))
    );
    assert_eq!(server.requests().len(), 1, "nothing more was sent");

    // A name that never resolves is a fetch that failed, and no refusal.
    let (lookup, _) = stand_in(vec![None]);
    let client = tilo_web_search::client_looking_up(Arc::clone(&lookup)).unwrap();
    let failed = fetch_looking_up(&client, &named, &limits(), lookup)
        .await
        .unwrap_err();
    assert!(matches!(failed, FetchError::Network(_)), "{failed:?}");
    assert_eq!(server.requests().len(), 1, "nothing more was sent");
}
