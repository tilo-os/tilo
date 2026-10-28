//! A new read generation uses the same original mounted owner and root custody.
use super::blocked_read::{commit_edit, phase};
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mounted_code_recovers_after_original_read_deadline() {
    trace_publication_hops();
    Box::pin(super::super::actual_exchange(
        false,
        true,
        Some(Journey::ReadRecovery),
    ))
    .await;
}

#[expect(
    clippy::future_not_send,
    reason = "the original mounted UI uses Rc on its caller thread"
)]
pub(super) async fn run(device: &Device, target: &mut Mounted) {
    tilo_test_env::within(
        "the original native read gate before the controlled deadline",
        async {
            loop {
                target.frame();
                if phase(target)["committed_read_ready"] == true {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        },
    )
    .await;
    let owner = target.network.native_owner().unwrap();
    let tree = crate::shell::native_workspace_owner::test_tree_of(&owner);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let _guard = tree.write();
        held_tx.send(()).unwrap();
        tilo_test_env::recv("release of the observed native read deadline", &release_rx);
    });
    tilo_test_env::recv("actual native workspace writer acquisition", &held_rx);
    // An idle reader asks the hung owner nothing and arms no timer: the hang
    // is met by the next ask. The app asks for its whole read again (as an
    // owner being rebuilt does); the reader withdraws readiness and Opens,
    // and that Open meets the hung owner.
    assert_eq!(
        target
            .code
            .app
            .controlled_request_committed_reads_for_test(),
        1,
        "the mounted Code route takes the ask"
    );
    let observed = tilo_test_env::within("the ask to close readiness", async {
        loop {
            let observed = phase(target);
            if observed["committed_read_ready"] == false {
                break observed;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    // Past the Open's two-second deadline the reader renews its read
    // generation rather than failing the route, however long the owner hangs.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert_eq!(
        phase(target)["committed_read_ready"],
        false,
        "no read is ready while the owner hangs"
    );
    let connected = target.code.app.collaboration_is_connected();
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    eprintln!(
        "controlled read renewal: ready={} connected={connected}",
        observed["committed_read_ready"]
    );
    assert_eq!(
        observed["committed_read_ready"], false,
        "the ask must withdraw readiness before renewal"
    );
    assert!(
        connected,
        "an Open past its deadline must preserve the original authenticated root"
    );
    let expected = format!("{BODY}// committed after the original read deadline\n");
    commit_edit(device, &owner, BODY, &expected);
    assert!(has_committed_body(&owner, &expected));
    tilo_test_env::within(
        "the same mounted Code owner after read generation renewal",
        async {
            loop {
                target.frame();
                if target.has_body(&expected) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        },
    )
    .await;
    assert!(target.code.app.collaboration_is_connected());
    assert_eq!(
        phase(target)["owners"],
        1,
        "renewal keeps the original document owner mounted"
    );
}
