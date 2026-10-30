//! What a fetch makes of each kind of body, read from a fixture server on
//! this machine: JSON comes back as indented text, HTML as its readable
//! text, and a PDF (labelled, or sent with no type at all) whole, as bytes
//! for the caller to keep. A binary body larger than the limit is refused in
//! plain words rather than cut.
#![allow(clippy::unwrap_used, reason = "in a test these are assertions")]

use std::time::Duration;

use tilo_web_search::{FetchError, FetchLimits, FetchedBody, fetch, fetch_fixture};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// A small but real PDF header and body: what matters is that it is binary.
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
