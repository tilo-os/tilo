//! The device's dispatch as the shell reaches it, against a daemon that answers on the link: a
//! call followed until it settles is answered once it does, however the link's thread is shared.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::remote::{
    Caller, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};

use crate::ShellClient;
use crate::wire::shell::{ShellCall, ShellReply};
use crate::wire::{Call, FRAMING, HostFrame, Operation, Reply};

/// How long the daemon takes to finish a call it took up.
const RUNNING: Duration = Duration::from_millis(50);

/// A daemon on a thread of its own: it takes every call up, finishes it [`RUNNING`] later, and
/// holds a wait for a call until it finished.
fn daemon(link: std::os::unix::net::UnixStream) {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            link.set_nonblocking(true).unwrap();
            let link = tokio::net::UnixStream::from_std(link).unwrap();
            let (mut reader, writer) = link.into_split();
            let writer = Arc::new(tokio::sync::Mutex::new(writer));
            while let Ok(Some(Call { id, request })) =
                FRAMING.read_value_async::<_, Call>(&mut reader).await
            {
                let Operation::Shell(ShellCall::Action { frame, .. }) = request else {
                    panic!("the dispatch asked the daemon something else: {request:?}");
                };
                let message = RemoteActionFrame::decode(&frame).unwrap().message;
                let writer = Arc::clone(&writer);
                tokio::spawn(async move {
                    let answer = match message {
                        RemoteActionMessage::Invoke(request) => {
                            RemoteActionMessage::Receipt(InvocationReceipt {
                                invocation: request.invocation,
                                status: ReceiptStatus::Accepted,
                            })
                        }
                        RemoteActionMessage::Observe {
                            correlation,
                            invocation,
                            ..
                        } => {
                            tokio::time::sleep(RUNNING).await;
                            RemoteActionMessage::Update {
                                invocation,
                                state: Some(InvocationState::new(
                                    invocation,
                                    InvocationUpdate::Succeeded {
                                        invocation,
                                        output: ActionOutput {
                                            value: Some(serde_json::json!("done")),
                                            ..ActionOutput::default()
                                        },
                                    },
                                )),
                                correlation: Some(correlation),
                            }
                        }
                        other => panic!("the daemon is asked nothing like {other:?}"),
                    };
                    let reply = HostFrame::Reply {
                        id,
                        result: Ok(Reply::Shell(ShellReply::Action(
                            RemoteActionFrame::new(answer).encode().unwrap(),
                        ))),
                    };
                    FRAMING
                        .write_value_async(&mut *writer.lock().await, &reply)
                        .await
                        .unwrap();
                });
            }
        });
    });
}

/// The shell's dispatch over a link carried by one thread of its own, as a desktop's is.
fn dispatch() -> crate::ShellDispatch {
    let (near, far) = std::os::unix::net::UnixStream::pair().unwrap();
    daemon(far);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = runtime.handle().clone();
    std::thread::spawn(move || runtime.block_on(std::future::pending::<()>()));
    let shell = {
        let _entered = handle.enter();
        near.set_nonblocking(true).unwrap();
        ShellClient::open(tokio::net::UnixStream::from_std(near).unwrap()).0
    };
    shell.dispatch()
}

fn invoked(wait_ms: Option<u32>) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: "so.tilo.session".to_owned(),
            address: ActionAddress::new("desktop:chrome", "quick-settings"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms,
        appearance: None,
        signed: None,
    }
}

/// A caller's call held until it settles (a script's step, a tool's call) is answered with how it
/// ended as soon as the daemon says it did: the wait looks again on the link's own thread when its
/// receipt and its wake arrive there, and is answered from what they brought rather than holding
/// that thread, and every other ask of the desktop's, for a look that cannot come.
#[test]
fn a_followed_call_is_answered_once_it_settles() {
    let dispatch = dispatch();
    let caller = Caller::local_system();
    for _ in 0..3 {
        let asked = Instant::now();
        let answer = tilo_action_calls::serve(
            &dispatch,
            &caller,
            RemoteActionMessage::Invoke(invoked(Some(10_000))),
        )
        .expect("an invoke is a request")
        .wait_timeout(Duration::from_secs(30))
        .ok()
        .flatten()
        .expect("the call is answered");
        let RemoteActionMessage::Receipt(receipt) = answer else {
            panic!("an invoke is answered with its receipt: {answer:?}");
        };
        assert!(
            matches!(&receipt.status, ReceiptStatus::Completed { output }
                if output.value == Some(serde_json::json!("done"))),
            "the receipt says how the call ended: {receipt:?}"
        );
        assert!(
            asked.elapsed() < Duration::from_secs(2),
            "the answer came once the call settled, not when a look gave up: {:?}",
            asked.elapsed()
        );
    }
}
