//! What a person's page does with a company node's legacy applications,
//! from a native peer, against a running node: certify a browser device with
//! the person's root, create and enter a workspace, find the session beside
//! the node (`/node`), list the legacy applications it offers in the
//! workspace, open one, and receive its window's frames on the live plane.
//! Then close the window and see its stream end.
//!
//! ```text
//! node_apps_probe --addr 127.0.0.1:8443 --cert-sha256 <hex> --root-key <file> --app chromium
//! ```
//!
//! `--cert-sha256` pins the node's TLS certificate (SHA-256 of its DER),
//! `--root-key` is the person's root signing key, `--app` a case-insensitive
//! part of the application's name. Prints one line per step; exits non-zero
//! when a step fails or takes longer than `--seconds` (default 180).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::too_many_lines,
    reason = "a probe reports on stdout and fails loudly"
)]

#[path = "../tests/support/https.rs"]
#[allow(
    dead_code,
    reason = "the probe uses the tests' pinned HTTPS client alone"
)]
mod https;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_actions::remote::{
    CatalogueQuery, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_browser_peer::{
    Host, IssuedRole, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle, issue_device_cert,
};
use tilo_relay_protocol::legacy_window as wire;
use tilo_relay_protocol::rooms::{RoomsReply, RoomsRequest};
use tilo_user_identity::RevocationList;

struct Args {
    addr: std::net::SocketAddr,
    cert_sha256: String,
    root_key: std::path::PathBuf,
    app: String,
    deadline: Instant,
}

fn args() -> Args {
    let mut args = std::env::args().skip(1);
    let (mut addr, mut cert, mut root, mut app, mut seconds) = (None, None, None, None, 180);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--addr" => addr = Some(value.parse().expect("--addr is an address")),
            "--cert-sha256" => cert = Some(value),
            "--root-key" => root = Some(value.into()),
            "--app" => app = Some(value.to_lowercase()),
            "--seconds" => seconds = value.parse().expect("--seconds is a number"),
            other => panic!("unknown flag {other}"),
        }
    }
    Args {
        addr: addr.expect("--addr"),
        cert_sha256: cert.expect("--cert-sha256"),
        root_key: root.expect("--root-key"),
        app: app.expect("--app"),
        deadline: Instant::now()
            .checked_add(Duration::from_secs(seconds))
            .expect("--seconds fits"),
    }
}

/// Poll `probe` every 100 ms until it answers or the deadline passes.
async fn until<T, F, Fut>(what: &str, deadline: Instant, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    loop {
        if let Some(value) = probe().await {
            println!("probe: {what}");
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn rooms(peer: &PeerHandle, request: RoomsRequest) -> Result<RoomsReply, String> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    peer.rooms_then(
        request,
        Box::new(move |result| {
            let _ = reply.send(result);
        }),
    )?;
    answer.await.map_err(|_| "peer closed".to_string())?
}

/// Send `message` to `to` and wait for the answer `matches` picks.
async fn ask<T>(
    peer: &PeerHandle,
    workspace_id: u64,
    to: [u8; 32],
    message: RemoteActionMessage,
    matches: impl Fn(RemoteActionMessage) -> Option<T>,
) -> Option<T> {
    peer.send_remote_action(
        workspace_id,
        to,
        RemoteActionFrame::new(message).encode().unwrap(),
    )
    .ok()?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        for event in peer.drain() {
            if let PeerEvent::RemoteAction { from, frame, .. } = event
                && from == to
                && let Ok(frame) = RemoteActionFrame::decode(&frame)
                && let Some(answer) = matches(frame.message)
            {
                return Some(answer);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

#[tokio::main]
async fn main() {
    let args = args();
    let deadline = args.deadline;
    let root =
        tilo_user_identity::persist::read_owned_signing_key(&args.root_key).expect("the root key");
    let http = https::DevHttps::new(args.addr, &args.cert_sha256);
    let host_cert = tilo_browser_peer::carrier::host_info(&*http)
        .await
        .expect("the node answers /host")
        .host_cert;
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cert = issue_device_cert(
        &root,
        key.verifying_key().to_bytes(),
        "Probe browser",
        IssuedRole::Browser,
    )
    .await
    .unwrap();
    let peer = PeerHandle::start(
        PeerConfig {
            host: Host::Https(http),
            cert,
            host_cert,
            revocations: RevocationList::new(),
        },
        Arc::new(key),
        Arc::new(MemoryPeerStore::default()),
    );

    // The company workspace (the one a rerun made before), and the page in
    // it.
    let acme = until("the workspace is there", deadline, || async {
        if let Ok(RoomsReply::Rooms(existing)) = rooms(&peer, RoomsRequest::List).await
            && let Some(known) = existing.into_iter().find(|known| known.name == "Acme")
        {
            return Some(known);
        }
        match rooms(
            &peer,
            RoomsRequest::Create {
                name: "Acme".into(),
            },
        )
        .await
        {
            Ok(RoomsReply::Room(created)) => Some(created),
            _ => None,
        }
    })
    .await;
    let entered = rooms(
        &peer,
        RoomsRequest::Enter {
            room: acme.room.clone(),
        },
    )
    .await;
    assert!(matches!(entered, Ok(RoomsReply::Room(_))), "{entered:?}");
    peer.join(acme.workspace_id).unwrap();

    // The session beside the node.
    let session = until("the node names its session", deadline, || async {
        let (reply, answer) = tokio::sync::oneshot::channel();
        peer.node_session_then(Box::new(move |result| {
            let _ = reply.send(result);
        }))
        .ok()?;
        answer.await.ok()?.ok().flatten()
    })
    .await;
    println!("probe: session {}", hex::encode(session));

    // Its legacy applications in the workspace, once it joined it.
    let provider = format!("workspace:{}", acme.room);
    let (target, title) = until("the session lists the application", deadline, || async {
        let page = ask(
            &peer,
            acme.workspace_id,
            session,
            RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query: CatalogueQuery {
                    provider: Some(provider.clone()),
                    max_results: 200,
                    ..CatalogueQuery::default()
                },
            },
            |message| match message {
                RemoteActionMessage::CatalogueResponse { result, .. } => Some(result),
                _ => None,
            },
        )
        .await?
        .ok()?;
        page.rows.into_iter().find_map(|row| {
            let action = row.action.as_ref()?;
            (action.resource_kind == "applications.host-app"
                && action.address.action == "open"
                && action.resource_label.to_lowercase().contains(&args.app))
            .then(|| {
                (
                    RemoteActionTarget {
                        provider: row.provider.clone(),
                        address: action.address.clone(),
                    },
                    action.resource_label.clone(),
                )
            })
        })
    })
    .await;
    println!("probe: opening {title}");
    let invocation = tilo_actions::InvocationId::new();
    let status = ask(
        &peer,
        acme.workspace_id,
        session,
        RemoteActionMessage::Invoke(InvokeRequest {
            invocation,
            target,
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
        }),
        |message| match message {
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation => {
                Some(receipt.status)
            }
            _ => None,
        },
    )
    .await
    .expect("the session answers the open");
    assert!(
        matches!(
            status,
            ReceiptStatus::Accepted | ReceiptStatus::Completed { .. }
        ),
        "the open was refused: {status:?}"
    );

    // Its window, on the live plane under the session's namespace.
    let (reply, answer) = tokio::sync::oneshot::channel();
    peer.live_grant_then(
        acme.workspace_id,
        Box::new(move |result| {
            let _ = reply.send(result);
        }),
    )
    .unwrap();
    let grant = answer.await.unwrap().expect("a live grant");
    let url = format!("https://127.0.0.1:{}/moq/live", args.addr.port())
        .parse()
        .unwrap();
    let trust = tilo_moq::carrier::Trust::Fingerprints(vec![args.cert_sha256.clone()]);
    let live = tilo_moq::live::peer::Peer::connect(&url, &trust, &grant.token, None)
        .await
        .expect("the live plane");
    let path = tilo_moq::live::namespace_path(&grant.room, &hex::encode(session)).unwrap();
    let namespace = tokio::time::timeout_at(deadline.into(), live.member(&path))
        .await
        .expect("timed out: the session's live namespace")
        .unwrap();
    let track = |record: &tilo_relay_protocol::live::LiveRecord| {
        format!("ops/{}", hex::encode(record.digest()))
    };
    let mut index = namespace
        .track(&track(&wire::index_record()))
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    // Every window it lists, until one with an application id (a splash
    // screen has none, and comes and goes first) delivers a whole keyframe.
    let (found, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let mut watched = std::collections::BTreeSet::new();
    let stream = tokio::time::timeout_at(deadline.into(), async {
        loop {
            tokio::select! {
                group = index.recv_group() => {
                    let Some(mut group) = group.unwrap() else { continue };
                    let Some(frame) = group.read_frame().await.unwrap() else { continue };
                    let Ok(wire::Frame::Index(streams)) = wire::Frame::decode(&frame.payload) else {
                        continue;
                    };
                    for stream in streams {
                        if !watched.insert(stream.surface.clone()) {
                            continue;
                        }
                        let named = !stream.app.starts_with("anon-");
                        println!(
                            "probe: stream {} {} {}x{}",
                            stream.surface, stream.app, stream.width, stream.height
                        );
                        let surface = u64::from_str_radix(&stream.surface, 16).unwrap();
                        let mut video = namespace
                            .track(&track(&wire::stream_record(surface)))
                            .unwrap()
                            .subscribe(None)
                            .await
                            .unwrap();
                        let found = found.clone();
                        tokio::spawn(async move {
                            while let Ok(Some(mut group)) = video.recv_group().await {
                                while let Ok(Some(frame)) = group.read_frame().await {
                                    if let Ok(wire::Frame::Chunk { flags, .. }) =
                                        wire::Frame::decode(&frame.payload)
                                        && flags & wire::CHUNK_KEY != 0
                                        && flags & wire::CHUNK_LAST != 0
                                        && named
                                    {
                                        let _ = found.send(stream.clone());
                                    }
                                }
                            }
                        });
                    }
                }
                Some(stream) = arrivals.recv() => break stream,
            }
        }
    })
    .await
    .expect("timed out: a keyframe of the window");
    println!(
        "probe: a whole keyframe of {} ({})",
        stream.surface, stream.app
    );
    let surface = u64::from_str_radix(&stream.surface, 16).unwrap();

    // Driving it closes it at the session.
    let input = |events: Vec<wire::InputEvent>| {
        let (reply, answer) = tokio::sync::oneshot::channel();
        peer.legacy_input_then(
            wire::InputRequest {
                workspace_id: acme.workspace_id,
                publisher: session,
                surface,
                events,
            },
            Box::new(move |result| {
                let _ = reply.send(result);
            }),
        )
        .unwrap();
        answer
    };
    assert_eq!(
        input(Vec::new()).await.unwrap(),
        Ok(true),
        "the person drives the window"
    );
    assert_eq!(
        input(vec![wire::InputEvent::Close]).await.unwrap(),
        Ok(true)
    );
    tokio::time::timeout_at(deadline.into(), async {
        loop {
            if let Some(mut group) = index.recv_group().await.unwrap()
                && let Some(frame) = group.read_frame().await.unwrap()
                && let Ok(wire::Frame::Index(streams)) = wire::Frame::decode(&frame.payload)
                && !streams
                    .iter()
                    .any(|listed| listed.surface == stream.surface)
            {
                break;
            }
        }
    })
    .await
    .expect("timed out: the closed window's stream to end");
    println!("probe: closed; node apps ok");
    peer.close();
}
