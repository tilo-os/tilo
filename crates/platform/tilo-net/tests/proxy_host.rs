#![cfg(not(target_family = "wasm"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use test_http::{Reply, Responder};
use tilo_net::proxy::host::{
    Budgets, Caller, Config, Destinations, FetchProxy, Lookup, MEDIA_PIECE, Rate, RateLimits, Room,
};
use tilo_net::proxy::{ERROR_HEADER, FINAL_URL_HEADER, MARKER_HEADER, Proxy};

const DEVICE: Caller = Caller([7; 32]);

fn request(method: &str, target: &str, body: &'static [u8]) -> http::Request<Full<Bytes>> {
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", target)
        .finish();
    http::Request::builder()
        .method(method)
        .uri(format!("/_tilo/net/fetch?{query}"))
        .header(MARKER_HEADER, "1")
        .body(Full::new(Bytes::from_static(body)))
        .unwrap()
}

/// A proxy that may reach loopback, as the development server's does.
fn local(mut config: Config) -> FetchProxy {
    config.destinations = Destinations::Any;
    FetchProxy::new(config).unwrap()
}

async fn body_of(response: http::Response<tilo_net::proxy::host::ProxyBody>) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn a_get_answers_the_target_with_headers_the_page_origin_cannot_act_on_renamed() {
    let target = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "application/json")
            .header("Set-Cookie", "session=target")
            .header("WWW-Authenticate", "Basic realm=\"api\"")
            .header("Cache-Control", "public, max-age=600")
            .header("Clear-Site-Data", "\"storage\"")
            .header("Access-Control-Allow-Origin", "*")
            .header("X-Tilo-Proxy-Error", "blocked")
            .header("X-Answer", "42")
            .body("[{\"title\":\"trending\"}]"),
    ]);
    let proxy = local(Config::default());
    let mut sent = request(
        "GET",
        &format!("{}/api/v1/trending?region=NL", target.url()),
        b"",
    );
    let headers = sent.headers_mut();
    headers.insert("x-tilo-session", "page-session".parse().unwrap());
    headers.insert("cookie", "host-cookie=1".parse().unwrap());
    headers.insert("user-agent", "Browser".parse().unwrap());
    headers.insert("x-tilo-forward-user-agent", "tube/0.1".parse().unwrap());
    headers.insert("x-forwarded-for", "203.0.113.5".parse().unwrap());
    let response = proxy.serve(DEVICE, sent).await;

    assert_eq!(response.status(), 200);
    let headers = response.headers();
    assert!(
        !headers.contains_key(ERROR_HEADER),
        "the target cannot fake a proxy failure"
    );
    assert_eq!(headers["x-tilo-forward-x-tilo-proxy-error"], "blocked");
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(headers["x-answer"], "42");
    assert_eq!(headers["cache-control"], "private, no-store");
    assert_eq!(
        headers["x-tilo-forward-cache-control"],
        "public, max-age=600"
    );
    for name in [
        "set-cookie",
        "www-authenticate",
        "clear-site-data",
        "access-control-allow-origin",
    ] {
        assert!(
            !headers.contains_key(name),
            "{name} must not reach the page's origin"
        );
        assert!(headers.contains_key(format!("x-tilo-forward-{name}").as_str()));
    }
    assert_eq!(
        headers[FINAL_URL_HEADER],
        format!("{}/api/v1/trending?region=NL", target.url())
    );
    assert_eq!(body_of(response).await, "[{\"title\":\"trending\"}]");

    let seen = target.next_request();
    assert_eq!(seen.method, "GET");
    assert_eq!(seen.target, "/api/v1/trending?region=NL");
    assert_eq!(seen.header("user-agent"), Some("tube/0.1"));
    assert_eq!(seen.header("x-tilo-session"), None);
    assert_eq!(seen.header("cookie"), None);
    assert_eq!(seen.header("x-forwarded-for"), None);
    assert_eq!(proxy.metrics().answered, 1);
}

#[tokio::test]
async fn redirects_are_followed_and_drop_credentials_across_origins() {
    let other = Responder::start(vec![Reply::status(200).body("moved here")]);
    let first = Responder::start(vec![
        Reply::status(302).header("Location", &format!("{}/landing", other.url())),
    ]);
    let proxy = local(Config::default());
    let mut sent = request("GET", &format!("{}/start", first.url()), b"");
    sent.headers_mut()
        .insert("authorization", "Bearer app-key".parse().unwrap());
    let response = proxy.serve(DEVICE, sent).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()[FINAL_URL_HEADER],
        format!("{}/landing", other.url())
    );
    assert_eq!(body_of(response).await, "moved here");
    assert_eq!(
        first.next_request().header("authorization"),
        Some("Bearer app-key")
    );
    assert_eq!(other.next_request().header("authorization"), None);
}

#[tokio::test]
async fn the_host_network_is_refused_by_address_and_by_what_a_name_resolves_to() {
    let target = Responder::silent();
    // Every lookup is counted: the connection must use the checked answer,
    // not a second lookup a rebinding name could answer differently.
    let lookups = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&lookups);
    let lookup: Lookup = Arc::new(move |name: String| {
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(match name.as_str() {
                "rebind.test" => vec![
                    "93.184.216.34".parse::<IpAddr>().unwrap(),
                    "127.0.0.1".parse().unwrap(),
                ],
                "metadata.test" => vec!["169.254.169.254".parse().unwrap()],
                _ => vec!["10.0.0.7".parse().unwrap()],
            })
        })
    });
    let proxy = FetchProxy::with_lookup(Config::default(), lookup).unwrap();
    let port = target.port();
    for url in [
        format!("http://127.0.0.1:{port}/"),
        format!("http://[::1]:{port}/"),
        format!("http://[::ffff:127.0.0.1]:{port}/"),
        "http://10.1.2.3/".to_owned(),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
        format!("http://rebind.test:{port}/"),
        "http://metadata.test/".to_owned(),
        "https://internal.test/".to_owned(),
    ] {
        let response = proxy.serve(DEVICE, request("GET", &url, b"")).await;
        assert_eq!(response.status(), 403, "{url}");
        assert_eq!(response.headers()[ERROR_HEADER], "blocked", "{url}");
    }
    assert_eq!(lookups.load(Ordering::SeqCst), 3, "one lookup per name");
    target.assert_untouched();
}

#[tokio::test]
async fn malformed_requests_ports_and_methods_are_refused() {
    let proxy = local(Config::default());
    let mut unmarked = request("GET", "https://example.com/", b"");
    unmarked.headers_mut().remove(MARKER_HEADER);
    let cases = [
        (unmarked, 400, "bad-request"),
        (
            request("GET", "ftp://example.com/", b""),
            400,
            "bad-request",
        ),
        (request("GET", "not a url", b""), 400, "bad-request"),
        (
            request("CONNECT", "https://example.com/", b""),
            400,
            "bad-request",
        ),
        (
            request("TRACE", "https://example.com/", b""),
            400,
            "bad-request",
        ),
        (
            request("GET", "http://example.com:25/", b""),
            403,
            "blocked",
        ),
        (
            request("GET", "http://example.com:22/", b""),
            403,
            "blocked",
        ),
    ];
    for (sent, status, code) in cases {
        let response = proxy.serve(DEVICE, sent).await;
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[ERROR_HEADER], code);
    }
}

#[tokio::test]
async fn bodies_beyond_the_limits_are_refused() {
    let target = Responder::start(vec![
        Reply::status(200).body(vec![b'x'; 4096]),
        Reply::status(200)
            .body(vec![b'y'; 4096])
            .without_content_length(),
    ]);
    let proxy = local(Config {
        max_request_bytes: 1024,
        max_response_bytes: 1024,
        ..Config::default()
    });

    let response = proxy
        .serve(
            DEVICE,
            request("POST", &format!("{}/upload", target.url()), &[0; 2048]),
        )
        .await;
    assert_eq!(response.status(), 413);
    assert_eq!(response.headers()[ERROR_HEADER], "too-large");

    // A declared length over the limit is refused before the body.
    let response = proxy
        .serve(
            DEVICE,
            request("GET", &format!("{}/big", target.url()), b""),
        )
        .await;
    assert_eq!(response.status(), 413);
    assert_eq!(response.headers()[ERROR_HEADER], "too-large");

    // An undeclared one breaks off at the limit, never reading as whole.
    let response = proxy
        .serve(
            DEVICE,
            request("GET", &format!("{}/stream", target.url()), b""),
        )
        .await;
    assert_eq!(response.status(), 200);
    assert!(response.into_body().collect().await.is_err());
}

#[tokio::test]
async fn a_target_slower_than_the_deadline_times_out() {
    let target = Responder::handle(|_| {
        std::thread::sleep(Duration::from_secs(3));
        Some(Reply::status(200).body("late"))
    });
    let proxy = local(Config {
        deadline: Duration::from_millis(500),
        ..Config::default()
    });
    let started = std::time::Instant::now();
    let response = proxy
        .serve(
            DEVICE,
            request("GET", &format!("{}/slow", target.url()), b""),
        )
        .await;
    assert_eq!(response.status(), 504);
    assert_eq!(response.headers()[ERROR_HEADER], "timeout");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn each_device_and_each_destination_has_a_budget() {
    let target = Responder::handle(|_| Some(Reply::status(204)));
    let tight = Rate {
        burst: 2.0,
        per_minute: 1.0,
    };
    let proxy = local(Config {
        limits: RateLimits {
            device_requests: tight,
            ..RateLimits::default()
        },
        ..Config::default()
    });
    let url = format!("{}/ping", target.url());
    for _ in 0..2 {
        assert_eq!(
            proxy
                .serve(DEVICE, request("GET", &url, b""))
                .await
                .status(),
            204
        );
    }
    let refused = proxy.serve(DEVICE, request("GET", &url, b"")).await;
    assert_eq!(refused.status(), 429);
    assert_eq!(refused.headers()[ERROR_HEADER], "rate-limited");
    assert!(
        refused.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            >= 1
    );
    // Another device still has its own budget.
    let other = Caller([8; 32]);
    assert_eq!(
        proxy.serve(other, request("GET", &url, b"")).await.status(),
        204
    );

    // One destination, from every device together.
    let proxy = local(Config {
        limits: RateLimits {
            destination_requests: tight,
            ..RateLimits::default()
        },
        ..Config::default()
    });
    for device in 0..2u8 {
        let response = proxy
            .serve(Caller([device; 32]), request("GET", &url, b""))
            .await;
        assert_eq!(response.status(), 204);
    }
    let refused = proxy
        .serve(Caller([9; 32]), request("GET", &url, b""))
        .await;
    assert_eq!(refused.status(), 429);
    let metrics = proxy.metrics();
    assert_eq!(metrics.answered, 2);
    assert!(metrics.failed.contains(&("rate-limited", 1)));
    assert!(
        proxy
            .prometheus("tilo_fetch_proxy")
            .contains("tilo_fetch_proxy_requests_total{outcome=\"rate-limited\"} 1")
    );
}

#[tokio::test]
async fn a_device_that_fetched_too_much_waits() {
    let target = Responder::handle(|_| Some(Reply::status(200).body(vec![b'z'; 2048])));
    let proxy = local(Config {
        limits: RateLimits {
            device_bytes: Rate {
                burst: 1024.0,
                per_minute: 1.0,
            },
            ..RateLimits::default()
        },
        ..Config::default()
    });
    let url = format!("{}/blob", target.url());
    let response = proxy.serve(DEVICE, request("GET", &url, b"")).await;
    assert_eq!(response.status(), 200);
    assert_eq!(body_of(response).await.len(), 2048);
    let refused = proxy.serve(DEVICE, request("GET", &url, b"")).await;
    assert_eq!(refused.status(), 429);
    assert_eq!(refused.headers()[ERROR_HEADER], "rate-limited");
}

/// `method` `target` as a media element sends it: the marker and the session
/// in the query, `range` the element's own.
fn media_request(method: &str, target: &str, range: &str) -> http::Request<Full<Bytes>> {
    let proxy = Proxy {
        endpoint: url::Url::parse("http://host.example/_tilo/net/fetch").unwrap(),
        session: Some("ab12".into()),
    };
    let url = tilo_net::proxy::media_url(&proxy, &url::Url::parse(target).unwrap());
    http::Request::builder()
        .method(method)
        .uri(format!("{}?{}", url.path(), url.query().unwrap()))
        .header("range", range)
        .body(Full::new(Bytes::new()))
        .unwrap()
}

#[tokio::test]
async fn a_media_element_streams_through_the_query_form_a_piece_at_a_time() {
    let target = Responder::start(vec![
        Reply::status(206)
            .header("Content-Type", "video/mp4")
            .header("Content-Range", "bytes 0-3/11829048")
            .header("Accept-Ranges", "bytes")
            .body("mp4!"),
        Reply::status(206)
            .header("Content-Type", "video/mp4")
            .header("Content-Range", "bytes 9000000-9000003/11829048")
            .body("more"),
    ]);
    let proxy = local(Config::default());
    let video = format!("{}/videoplayback?itag=18", target.url());

    // An open-ended range is asked of the target one piece at a time.
    let response = proxy
        .serve(DEVICE, media_request("GET", &video, "bytes=0-"))
        .await;
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["content-range"], "bytes 0-3/11829048");
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert_eq!(body_of(response).await, "mp4!");
    let seen = target.next_request();
    assert_eq!(seen.target, "/videoplayback?itag=18");
    let piece_end = MEDIA_PIECE - 1;
    assert_eq!(
        seen.header("range"),
        Some(format!("bytes=0-{piece_end}").as_str())
    );

    // A bounded range goes as the element asked.
    let response = proxy
        .serve(
            DEVICE,
            media_request("GET", &video, "bytes=9000000-9000003"),
        )
        .await;
    assert_eq!(response.status(), 206);
    assert_eq!(
        target.next_request().header("range"),
        Some("bytes=9000000-9000003")
    );

    // Only a GET or HEAD may name itself in the query: a form a cross-site
    // page submits sends a POST.
    let response = proxy
        .serve(DEVICE, media_request("POST", &video, "bytes=0-"))
        .await;
    assert_eq!(response.status(), 400);
    assert_eq!(response.headers()[ERROR_HEADER], "bad-request");
    let post = media_request("POST", &video, "bytes=0-");
    assert_eq!(
        tilo_net::proxy::host::query_session(post.method(), post.uri()),
        None
    );
    let get = media_request("GET", &video, "bytes=0-");
    assert_eq!(
        tilo_net::proxy::host::query_session(get.method(), get.uri()).as_deref(),
        Some("ab12")
    );
}

/// A proxy that relays `visitor_bytes` to each visitor and `room_bytes` to
/// each room per `window`.
fn budgeted(visitor_bytes: Option<u64>, room_bytes: Option<u64>, window: Duration) -> FetchProxy {
    local(Config {
        budgets: Budgets {
            window,
            visitor_bytes,
            room_bytes,
        },
        ..Config::default()
    })
}

#[tokio::test]
async fn a_visitor_that_received_its_budget_is_refused_until_the_window_turns_over() {
    let target = Responder::handle(|_| Some(Reply::status(200).body(vec![b'v'; 2048])));
    let window = Duration::from_millis(600);
    let proxy = budgeted(Some(2048), None, window);
    let url = format!("{}/piece", target.url());
    let response = proxy.serve(DEVICE, request("GET", &url, b"")).await;
    assert_eq!(response.status(), 200);
    assert_eq!(body_of(response).await.len(), 2048);

    let refused = proxy.serve(DEVICE, request("GET", &url, b"")).await;
    assert_eq!(refused.status(), 429);
    assert_eq!(refused.headers()[ERROR_HEADER], "over-budget");
    assert_eq!(refused.headers()["retry-after"], "1");
    assert!(body_of(refused).await.contains("for now"));
    // Another visitor has its own budget.
    let other = proxy
        .serve(Caller([8; 32]), request("GET", &url, b""))
        .await;
    assert_eq!(other.status(), 200);
    assert_eq!(body_of(other).await.len(), 2048);
    let metrics = proxy.metrics();
    assert!(metrics.failed.contains(&("over-budget", 1)));
    assert!(
        proxy
            .prometheus("tilo_fetch_proxy")
            .contains("tilo_fetch_proxy_requests_total{outcome=\"over-budget\"} 1")
    );

    tokio::time::sleep(window).await;
    let renewed = proxy.serve(DEVICE, request("GET", &url, b"")).await;
    assert_eq!(renewed.status(), 200, "a new window, a new budget");
}

#[tokio::test]
async fn everyone_in_a_room_shares_its_budget() {
    let target = Responder::handle(|_| Some(Reply::status(200).body(vec![b'r'; 1500])));
    let proxy = budgeted(None, Some(3000), Duration::from_secs(3600));
    let url = format!("{}/piece", target.url());
    let lounge = [Room(1)];
    for device in 1..=2u8 {
        let response = proxy
            .serve_in(Caller([device; 32]), &lounge, request("GET", &url, b""))
            .await;
        assert_eq!(response.status(), 200);
        body_of(response).await;
    }
    // A third person in the room is refused, though they received nothing.
    let refused = proxy
        .serve_in(Caller([3; 32]), &lounge, request("GET", &url, b""))
        .await;
    assert_eq!(refused.status(), 429);
    assert_eq!(refused.headers()[ERROR_HEADER], "over-budget");
    let retry: u64 = refused.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (3500..=3600).contains(&retry),
        "until the window turns over: {retry}"
    );
    // So is one who is also in another room: every room they are in counts.
    let refused = proxy
        .serve_in(
            Caller([4; 32]),
            &[Room(2), Room(1)],
            request("GET", &url, b""),
        )
        .await;
    assert_eq!(refused.headers()[ERROR_HEADER], "over-budget");
    // Another room has its own.
    let response = proxy
        .serve_in(Caller([3; 32]), &[Room(2)], request("GET", &url, b""))
        .await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn a_response_that_spends_the_budget_ends_in_an_error_not_a_short_body() {
    let target = Responder::start(vec![Reply::status(200).body(vec![b'm'; 1024 * 1024])]);
    let proxy = budgeted(Some(1024), None, Duration::from_secs(3600));
    let url = format!("{}/video", target.url());
    let response = proxy.serve(DEVICE, request("GET", &url, b"")).await;
    assert_eq!(response.status(), 200);
    assert!(
        response.into_body().collect().await.is_err(),
        "the page sees the body break off, never a whole short one"
    );
    let refused = proxy
        .serve(DEVICE, media_request("GET", &url, "bytes=1024-"))
        .await;
    assert_eq!(refused.status(), 429);
    assert_eq!(refused.headers()[ERROR_HEADER], "over-budget");
}

/// A peer request: the target is this machine's own path beside
/// [`tilo_net::proxy::PEER_PARAM`].
fn peer_request(path: &str) -> http::Request<Full<Bytes>> {
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", &format!("file://{path}"))
        .append_pair(tilo_net::proxy::PEER_PARAM, tilo_net::proxy::PEER_LOCAL)
        .finish();
    http::Request::builder()
        .method("GET")
        .uri(format!("/peer/v1/fetch?{query}"))
        .header(MARKER_HEADER, "1")
        .body(Full::new(Bytes::new()))
        .unwrap()
}

#[tokio::test]
async fn a_host_that_offers_its_own_files_reads_a_machine_path_as_the_path_it_is() {
    let mut config = Config {
        peer: true,
        ..Config::default()
    };
    // Loopback is this machine there, and a peer request checks no
    // destination: it goes nowhere.
    config.destinations = Destinations::Any;
    let proxy = FetchProxy::new(config).unwrap();
    let directory = std::env::temp_dir().join("tilo-peer-route");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("notes.md");
    std::fs::write(&file, "# Notes\n").unwrap();

    let response = proxy
        .serve(DEVICE, peer_request(file.to_str().unwrap()))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(body_of(response).await, "# Notes\n");

    // A path that is not there is a file that is not there, as the desktop
    // session's own reader would find it.
    let response = proxy
        .serve(DEVICE, peer_request("/nonexistent/tilo/notes.md"))
        .await;
    assert_eq!(response.status(), 404);
    assert_eq!(
        response.headers()[http::header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    std::fs::remove_file(&file).unwrap();
}

#[tokio::test]
async fn a_host_that_does_not_offer_its_files_refuses_a_peer_request() {
    let proxy = local(Config::default());
    let response = proxy.serve(DEVICE, peer_request("/etc/hostname")).await;
    assert_eq!(response.status(), 400);
    assert_eq!(response.headers()[ERROR_HEADER], "bad-request");
    // The client's word for a peer request does not make it one: it is the
    // host's configuration that does.
    let mut made_up = peer_request("/etc/hostname");
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", "http://127.0.0.1:1/")
        .append_pair(tilo_net::proxy::PEER_PARAM, "elsewhere")
        .finish();
    *made_up.uri_mut() = format!("/peer/v1/fetch?{query}").parse().unwrap();
    let response = proxy.serve(DEVICE, made_up).await;
    assert_eq!(response.status(), 400);
    assert_eq!(response.headers()[ERROR_HEADER], "bad-request");
}
