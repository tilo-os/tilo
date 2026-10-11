//! Signed governor progress must not depend on draining compositor replay.
use super::*;

pub(super) async fn assert_admitted_without_ui_drain(
    authority: &ActualAuthority,
    owner: &NativeWorkspaceOwner,
    room: RoomId,
) {
    let expected = {
        let authority = authority.lock().unwrap();
        let governor = authority.as_ref().unwrap();
        tilo_peer_state::durability::entries(governor.peer().admitted_room().unwrap()).unwrap()
    };
    // Deliberately leave the real NetEvent receiver untouched. Neither this
    // test nor a synthetic helper may admit or project the arriving message.
    tilo_test_env::within(
        "signed governance admitted without a UI event drain",
        async {
            loop {
                let actual = owner
                    .with_current(room, |view| {
                        tilo_peer_state::durability::entries(view.peer().admitted_room().unwrap())
                    })
                    .unwrap();
                if actual == expected {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        },
    )
    .await;
}
