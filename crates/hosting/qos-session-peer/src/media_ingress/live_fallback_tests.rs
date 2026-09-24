//! LIVE_V1 HTTPS fallback carrier on the installed ingress: real TLS listener,
//! a real attached owner, grants from the proof-required grant route, and the
//! bridge into the shared relay observed by a real WebTransport MoQ client.
use super::{
    data_tests::Harness,
    live_tests::{grant, live_connect, request},
    *,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::StatusCode;
use hyper_util::rt::TokioIo;
use qos_moq::live::fallback::*;
use rustls::pki_types::ServerName;
use tilo_relay::delivery_v3::live_wire::*;
use tokio::time::timeout;

/// An open fallback response stream over HTTPS (HTTP/1.1, chunked).
struct Stream {
    body: hyper::body::Incoming,
    pending: Vec<u8>,
    _driver: tokio::task::JoinHandle<()>,
}
impl Stream {
    async fn frame(&mut self) -> (u8, Vec<u8>) {
        loop {
            if self.pending.len() >= 5 {
                let len = u32::from_be_bytes(self.pending[1..5].try_into().unwrap()) as usize;
                if self.pending.len() >= 5 + len {
                    let kind = self.pending[0];
                    let payload = self.pending[5..5 + len].to_vec();
                    self.pending.drain(..5 + len);
                    return (kind, payload);
                }
            }
            let frame = timeout(Duration::from_secs(5), self.body.frame())
                .await
                .expect("stream deadline")
                .expect("stream open")
                .unwrap();
            if let Ok(data) = frame.into_data() {
                self.pending.extend_from_slice(&data);
            }
        }
    }
}

async fn post(
    h: &Harness,
    path: &str,
    body: Vec<u8>,
    session: Option<[u8; 16]>,
) -> (StatusCode, Option<Stream>, Vec<u8>) {
    let tcp = tokio::net::TcpStream::connect(h.client.address)
        .await
        .unwrap();
    let tls = tokio_rustls::TlsConnector::from(h.client.tls.clone())
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .unwrap();
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut builder = hyper::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", h.client.address.to_string())
        .header("origin", h.ingress.origin());
    if let Some(id) = session {
        builder = builder.header(FALLBACK_SESSION_HEADER, hex::encode(id));
    }
    let response = sender
        .send_request(builder.body(Full::new(Bytes::from(body))).unwrap())
        .await
        .unwrap();
    let status = response.status();
    if status == StatusCode::OK {
        assert_eq!(response.headers()["cache-control"], "no-store");
        return (
            status,
            Some(Stream {
                body: response.into_body(),
                pending: Vec::new(),
                _driver: driver,
            }),
            Vec::new(),
        );
    }
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, None, bytes)
}

fn presence(seq: u64) -> Vec<u8> {
    PresenceFrame {
        seq,
        sent_at_ms: 1,
        focus: None,
        pointer: Some([seq as f32, 7.0]),
        selection: None,
        drag: None,
    }
    .encode()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fallback_carrier_admits_grants_streams_and_bridges_into_the_relay() {
    let mut h = Harness::start().await;
    // Refusals: a forged grant, an unknown session, malformed frames.
    let (status, _, _) = post(&h, FALLBACK_OPEN_PATH, b"forged".to_vec(), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = post(&h, FALLBACK_SEND_PATH, Vec::new(), Some([3; 16])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A WebTransport member subscribed to the room.
    let (status, moq_token) = {
        let wanted = request(&h, 31, true);
        grant(&mut h, &wanted).await
    };
    assert_eq!(status, StatusCode::OK);
    let client = live_connect(&h, moq_token).await.unwrap();
    let path = format!(
        "tilo-live/{}/{}",
        hex::encode(room_digest(h.f.owner.peer().room())),
        hex::encode(h.f.owner.identity.native_cert.device_public_key)
    );
    let own = timeout(
        Duration::from_secs(5),
        client.incoming.consume().announced_broadcast(path.as_str()),
    )
    .await
    .unwrap()
    .unwrap();
    let mut from_relay = own
        .track(PRESENCE_TRACK)
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();

    // The same device on the fallback carrier (newest session owns the namespace).
    let (status, token) = {
        let wanted = request(&h, 32, true);
        grant(&mut h, &wanted).await
    };
    assert_eq!(status, StatusCode::OK);
    let (status, stream, body) = post(&h, FALLBACK_OPEN_PATH, token, None).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let mut stream = stream.unwrap();
    let (kind, hello) = stream.frame().await;
    assert_eq!(kind, FRAME_HELLO);
    assert_eq!(hello[16], 0b11, "publish and subscribe granted");
    let id: [u8; 16] = hello[..16].try_into().unwrap();

    let (status, _, _) = post(&h, FALLBACK_SEND_PATH, vec![0x11, 0, 0], Some(id)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Codec-valid presence over HTTPS reaches the WebTransport subscriber.
    let received = timeout(Duration::from_secs(5), async {
        let mut seq = 0;
        loop {
            seq += 1;
            let batch = encode_frame(FRAME_SEND_PRESENCE, &[&presence(seq)]);
            let (status, _, _) = post(&h, FALLBACK_SEND_PATH, batch, Some(id)).await;
            assert_eq!(status, StatusCode::NO_CONTENT);
            if let Ok(Ok(Some(datagram))) =
                timeout(Duration::from_millis(50), from_relay.recv_datagram()).await
            {
                return datagram.payload.to_vec();
            }
        }
    })
    .await
    .expect("fallback presence relayed to the MoQ subscriber");
    assert_eq!(
        PresenceFrame::decode(&received).unwrap().pointer.unwrap()[1],
        7.0
    );

    // A dropped stream resumes the same session.
    drop(stream);
    let (status, resumed, _) = post(&h, FALLBACK_RESUME_PATH, Vec::new(), Some(id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resumed.unwrap().frame().await.0, FRAME_HELLO);
    client.session.abort(moq_net::Error::Cancel);
}
