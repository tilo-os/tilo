//! A page's apps read a site without CORS headers through the workspace
//! host it is admitted to: the host says it serves the fetch proxy when the
//! session starts, the page sends its session with each proxied request,
//! and the answer reads like the site's own (tilo-net wire protocol v1).

#![allow(
    clippy::unwrap_used,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host and page live until the test ends"
)]

mod support;

use http_body_util::BodyExt as _;
use test_http::{Reply, Responder};
use tilo_browser_peer::{Host, PeerHandle};
use tilo_net::proxy::{self, Proxy};
use tilo_net::{HeaderMap, HeaderValue, Url};
use tilo_relay::web::FetchProxySetup;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{HostSettings, enrol, start_host_plain, start_host_serving, start_peer, wait_for};

/// What an Invidious instance answers `/api/v1/trending` with, trimmed to
/// what Tube lists.
const TRENDING: &str = r#"[{"type":"video","title":"A trending video","videoId":"dQw4w9WgXcQ","author":"Someone","lengthSeconds":212,"viewCount":1000,"videoThumbnails":[{"quality":"medium","url":"https://i.ytimg.com/vi/dQw4w9WgXcQ/mqdefault.jpg","width":320,"height":180}]}]"#;

/// `GET target` through the proxy at `proxy`, as `tilo_net::Client` sends it
/// from a page: the answer's status, its headers as the app sees them, the
/// URL that answered and the body.
async fn fetch_through(
    plain: &str,
    proxy: &Proxy,
    target: &Url,
) -> Result<(u16, HeaderMap, Url, Vec<u8>), tilo_net::NetError> {
    let mut headers = HeaderMap::new();
    headers.insert("user-agent", HeaderValue::from_static("tube/0.1 (+tilo)"));
    headers.insert("accept", HeaderValue::from_static("application/json"));
    let (url, headers) = proxy::route(proxy, target, &headers)?;
    let stream = tokio::net::TcpStream::connect(plain.trim_start_matches("http://"))
        .await
        .unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .unwrap();
    tokio::spawn(connection);
    let mut request = hyper::Request::get(format!("{}?{}", url.path(), url.query().unwrap()))
        .header(hyper::header::HOST, "localhost");
    for (name, value) in &headers {
        request = request.header(name, value);
    }
    let response = sender
        .send_request(
            request
                .body(http_body_util::Empty::<bytes::Bytes>::new())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let answered = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let (headers, final_url) = proxy::unwrap_response(target, &answered, || {
        String::from_utf8_lossy(&body).into_owned()
    })?;
    Ok((status, headers, final_url, body.to_vec()))
}

async fn session(peer: &PeerHandle) -> String {
    wait_for("the host's fetch proxy session", || async {
        peer.fetch_proxy_session()
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admitted_page_reads_a_site_without_cors_through_its_host() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "room-host", DeviceCapabilities::host());
    let laptop = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host_serving(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            plain_listen: Some("127.0.0.1:0".parse().unwrap()),
            fetch_proxy: Some(FetchProxySetup {
                config: tilo_net::proxy::host::Config {
                    // The video directory stands in on loopback, which a
                    // host's proxy otherwise never reaches.
                    destinations: tilo_net::proxy::host::Destinations::Any,
                    ..Default::default()
                },
                metrics_file: Some(state.path().join("metrics.d/fetch-proxy.prom")),
            }),
            ..Default::default()
        },
    )
    .await;
    let plain = host.web.plain_origin.clone().unwrap();
    let plain = plain.replace("localhost", "127.0.0.1");
    let peer = start_peer(
        Host::Https(host.https()),
        laptop.cert.clone(),
        host_device.cert.clone(),
        laptop.key.clone(),
    );
    let token = session(&peer).await;

    let directory = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "application/json")
            .header("Cache-Control", "public, max-age=300")
            .body(TRENDING),
    ]);
    let target = Url::parse(&format!("{}/api/v1/trending", directory.url())).unwrap();
    let proxy = Proxy {
        endpoint: Url::parse(&format!("{plain}/peer/v1/fetch")).unwrap(),
        session: Some(token),
    };
    let (status, headers, final_url, body) = fetch_through(&plain, &proxy, &target).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(final_url, target);
    // The app reads the site's own caching header; the page's origin got a
    // private one.
    assert_eq!(headers["cache-control"], "public, max-age=300");
    let videos: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(videos[0]["videoId"], "dQw4w9WgXcQ");
    assert_eq!(videos[0]["title"], "A trending video");
    let seen = directory.next_request();
    assert_eq!(seen.target, "/api/v1/trending");
    assert_eq!(seen.header("user-agent"), Some("tube/0.1 (+tilo)"));
    assert_eq!(seen.header("x-tilo-session"), None);

    // Without the session, the proxy is closed to the page.
    let stranger = Proxy {
        endpoint: proxy.endpoint.clone(),
        session: Some(hex::encode([3; 32])),
    };
    let refused = fetch_through(&plain, &stranger, &target).await;
    assert!(
        matches!(refused, Err(tilo_net::NetError::Connect { .. })),
        "{refused:?}"
    );
    // The counters a room host's metrics page appends.
    let metrics = state.path().join("metrics.d/fetch-proxy.prom");
    wait_for("the fetch proxy's counters", || {
        let text = std::fs::read_to_string(&metrics).unwrap_or_default();
        async move {
            (text.contains("tilo_rooms_fetch_proxy_requests_total{outcome=\"answered\"} 1")
                && text
                    .contains("tilo_rooms_fetch_proxy_requests_total{outcome=\"unauthorized\"} 1"))
            .then_some(())
        }
    })
    .await;
    peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_without_the_proxy_names_none() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host_plain(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        None,
    )
    .await;
    let peer = start_peer(
        Host::Https(host.https()),
        laptop.cert.clone(),
        host_device.cert.clone(),
        laptop.key.clone(),
    );
    wait_for("the session", || async {
        peer.status().authenticated.then_some(())
    })
    .await;
    assert_eq!(peer.fetch_proxy_session(), None);
    peer.close();
}
