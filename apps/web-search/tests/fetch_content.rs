//! What `fetch` answers an agent for each kind of body, read from a fixture
//! server on this machine: a GitHub-like JSON listing comes back as indented
//! text, and a PDF is kept whole in the workspace's content store and
//! answered as `tilo.content:<hash>` with its media type, whose bytes the
//! store then holds. Without a content store a file is refused in plain
//! words, never dropped.
#![allow(clippy::unwrap_used, reason = "in a test these are assertions")]

use std::time::Duration;

use tilo_web_search::{FetchLimits, fetch_fixture};
use tilo_web_search_app::actions::{answer_fetched, code};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const PDF: &[u8] =
    b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n%%EOF\n";
const REPOS: &[u8] = br#"[{"name":"engine","description":"An analytical engine","stargazers_count":12,"pushed_at":"2026-10-20T09:00:00Z"}]"#;

/// Answer every request: `/repos` with JSON, `/cv.pdf` with a PDF and
/// `/file` with the PDF and no type at all.
async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => {
                            request.extend_from_slice(buffer.get(..read).unwrap_or_default())
                        }
                    }
                }
                let line = String::from_utf8_lossy(&request);
                let (kind, body) = match line.split_whitespace().nth(1) {
                    Some("/repos") => ("Content-Type: application/json\r\n", REPOS),
                    Some("/cv.pdf") => ("Content-Type: application/pdf\r\n", PDF),
                    _ => ("", PDF),
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\n{kind}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(body).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://{address}")
}

#[tokio::test]
async fn json_is_text_and_a_pdf_is_kept_as_workspace_content() {
    let base = serve().await;
    let client = tilo_web_search::client().unwrap();
    let limits = FetchLimits {
        timeout: Duration::from_secs(10),
        ..FetchLimits::default()
    };
    let store = tilo_content::ContentStore::memory();

    let repos = fetch_fixture(&client, &format!("{base}/repos"), &limits)
        .await
        .unwrap();
    let answer = answer_fetched(repos, Some(&store), 0, 20_000).unwrap();
    assert_eq!(answer["content_type"], "application/json");
    assert!(answer.get("content").is_none(), "{answer}");
    let text = answer["text"].as_str().unwrap();
    assert!(
        text.starts_with("[\n  {\n    \"name\": \"engine\",\n"),
        "JSON is indented: {text}"
    );

    for path in ["/cv.pdf", "/file"] {
        let pdf = fetch_fixture(&client, &format!("{base}{path}"), &limits)
            .await
            .unwrap();
        let answer = answer_fetched(pdf, Some(&store), 0, 20_000).unwrap();
        assert_eq!(
            answer["content_type"], "application/pdf",
            "{path}: {answer}"
        );
        assert_eq!(answer["bytes"], PDF.len());
        assert!(answer.get("text").is_none(), "{answer}");
        let address = answer["content"].as_str().unwrap();
        let hash = address.strip_prefix("tilo.content:").unwrap();
        assert_eq!(
            store.read(hash).as_deref(),
            Some(PDF),
            "{path} is kept whole"
        );
    }

    let unkept = fetch_fixture(&client, &format!("{base}/cv.pdf"), &limits)
        .await
        .unwrap();
    let refused = answer_fetched(unkept, None, 0, 20_000).unwrap_err();
    assert_eq!(refused.code, code::NOT_KEPT);
    assert!(
        refused.message.contains("application/pdf"),
        "{}",
        refused.message
    );
}
