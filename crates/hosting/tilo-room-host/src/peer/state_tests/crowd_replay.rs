//! A crowd entering a workspace at once, as one does after a public post:
//! every page is replayed once, and none waits on another page's network.
use super::*;

/// A page whose network stalled: its connection never takes a byte.
struct Stalled;
impl tokio::io::AsyncWrite for Stalled {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Pending
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }
}

/// More stalled pages than the host has replay slots.
const STALLED: usize = crate::store::replay::DEFAULT_REPLAY_TICKETS + 8;
/// The crowd entering behind them.
const CROWD: usize = 100;
/// Well inside the time a subscription waits for a replay slot before its
/// session ends: a replay that waited on a stalled page's network instead
/// would end its session, and the page would come back for all of it again.
const CROWD_REPLAYED: std::time::Duration = std::time::Duration::from_secs(20);

/// Subscribe `peer` on a connection of its own and write its replay and
/// `Ready` to `write`.
async fn enter<W: tokio::io::AsyncWrite + Unpin>(
    fixture: &Fixture,
    peer: &AuthenticatedPeer,
    write: &mut W,
) -> Result<(), PeerError> {
    let (outbound, _outbound_rx) = mpsc::channel(32);
    let (disconnect, _disconnected) = mpsc::channel(1);
    let (state_tx, _state_rx) = mpsc::channel(32);
    let session = fixture.connection_ctx(&outbound, &disconnect, Some(&state_tx));
    Box::pin(handle_inbound_frame(
        Envelope::Subscribe {
            workspace_id: fixture.store.workspace_id(),
        },
        peer,
        &session,
        &mut Vec::new(),
        write,
    ))
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crowd_is_replayed_once_each_and_never_waits_on_a_stalled_page() {
    let fixture = Arc::new(Fixture::new());
    // What each newcomer is replayed.
    fixture.enqueue_checkpoint(1, vec![1; 64]).await;
    fixture.drained().await;
    let peers: Vec<AuthenticatedPeer> = (0..STALLED + CROWD)
        .map(|n| {
            certified_device(
                &fixture.root_key,
                fixture.source.own_user,
                &format!("page {n}"),
            )
            .0
        })
        .collect();
    {
        let mut runtime = fixture.workspace.lock().await;
        for peer in &peers {
            runtime.admit_device(peer.device_public_key).unwrap();
        }
    }
    let (stalled, crowd) = peers.split_at(STALLED);

    let mut stalled_pages = tokio::task::JoinSet::new();
    for peer in stalled {
        let (fixture, peer) = (fixture.clone(), peer.clone());
        stalled_pages.spawn(async move { enter(&fixture, &peer, &mut Stalled).await });
    }
    // Every stalled page's replay is captured and waits on its network.
    tilo_test_env::within("the stalled pages' replays to be captured", async {
        while fixture.metrics.snapshot().subscribes < STALLED as u64 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;

    let entering = std::time::Instant::now();
    let mut crowd_pages = tokio::task::JoinSet::new();
    for peer in crowd {
        let (fixture, peer) = (fixture.clone(), peer.clone());
        crowd_pages.spawn(async move { enter(&fixture, &peer, &mut tokio::io::sink()).await });
    }
    let replayed = tokio::time::timeout(CROWD_REPLAYED, async {
        let mut replayed = 0usize;
        while let Some(entered) = crowd_pages.join_next().await {
            entered.unwrap().unwrap();
            replayed += 1;
        }
        replayed
    })
    .await;
    assert_eq!(
        replayed.ok(),
        Some(CROWD),
        "{CROWD} pages entering behind {STALLED} stalled ones were not all replayed within \
         {CROWD_REPLAYED:?}: a replay waited on a stalled page's network"
    );
    eprintln!("{CROWD} pages replayed in {:?}", entering.elapsed());
    assert_eq!(
        fixture.metrics.snapshot().subscribes,
        (STALLED + CROWD) as u64,
        "each page was replayed once"
    );
    stalled_pages.abort_all();
}
