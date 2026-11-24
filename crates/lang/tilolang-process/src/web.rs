//! The network `web.get` and `web.post` reach from an app this process runs: this device's,
//! once per request, through the general network client (`tilo_network_client`) the app's other
//! requests use too. The runtime keeps the answer in the invocation's journal, so a replay
//! answers from it instead of asking again.
//!
//! A timeout, no network and a refused connection answer with status 0 and say why: the app
//! sees a value, never a trap.

use tilolang_tilo::runtime::run::WebRequest;
use tilolang_tilo::runtime::web::{MAX_BODY, TIMEOUT, WebReply};

/// Fetch `request` over this device's network.
pub fn fetch(request: &WebRequest) -> WebReply {
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .filter_map(|line| {
            line.split_once(':')
                .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect();
    if let Some(tag) = &request.if_none_match {
        headers.push(("If-None-Match".to_owned(), tag.clone()));
    }
    let body = request.body_bytes();
    match tilo_network_client::fetch(
        &request.method,
        &request.url,
        &headers,
        body.as_deref(),
        MAX_BODY,
        TIMEOUT,
    ) {
        Ok(reply) => WebReply {
            status: reply.status,
            status_text: reply.status_text,
            headers: reply.headers,
            body: reply.body,
        },
        Err(reason) => WebReply::failed(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_http::{Reply as Served, Responder};

    /// The runtime's request reads through the general network client: the method, the headers
    /// sent, and the status, headers and body the language gets back.
    #[test]
    fn a_web_request_reads_through_the_general_client() {
        let served = Responder::start(vec![
            Served::status(200)
                .header("content-type", "text/plain")
                .body("from the client"),
        ]);
        let answer = fetch(&WebRequest {
            method: "GET".to_owned(),
            url: served.url(),
            headers: vec!["Accept: text/plain".to_owned()],
            body: None,
            bytes: None,
            if_none_match: None,
        });
        assert_eq!(answer.status, 200);
        assert_eq!(answer.body, b"from the client");
        assert!(
            answer
                .headers
                .iter()
                .any(|line| line.eq_ignore_ascii_case("content-type: text/plain")),
            "{:?}",
            answer.headers
        );
        let seen = served.next_request();
        assert_eq!(seen.method, "GET");
        assert_eq!(seen.header("accept"), Some("text/plain"));
    }
}
