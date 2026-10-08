//! Original custody for the existing remote frame's explicit continuation.
use super::{
    AdmittedJoinIdentity, Arc, CapnpBrokerState, CapnpChannelHandle, InvocationCaller, Promise, Rc,
    action_host, app_consent, channel_handle, published_invocation::OriginalInvocation,
    reply_frame,
};
use tilo_actions::remote::{Caller, RemoteActionFrame, RemoteActionMessage, serve as serve_host};

pub(super) struct CheckedContinuation {
    original: Arc<OriginalInvocation>,
    pub(super) caller: Caller,
}
impl CheckedContinuation {
    pub(super) fn check(&self, state: &CapnpBrokerState) -> Result<(), String> {
        self.original.check_lifecycle()?;
        self.original.check_execution_frontier(state)
    }

    #[expect(
        clippy::future_not_send,
        reason = "directory capabilities run on the broker LocalSet"
    )]
    async fn check_publication(&self, channel: &CapnpChannelHandle) -> Result<(), String> {
        self.check(&channel.state)?;
        self.original
            .check_continuation_publication(channel, &self.caller)
            .await?;
        self.check(&channel.state)
    }
}

#[expect(
    clippy::future_not_send,
    reason = "directory capabilities run on the broker LocalSet"
)]
pub(super) async fn prepare(
    channel: &CapnpChannelHandle,
    request: &RemoteActionFrame,
) -> Result<CheckedContinuation, String> {
    let parent = request
        .on_behalf_of
        .ok_or("original continuation parent absent")?;
    let original = OriginalInvocation::for_delivered(channel, parent)?;
    let caller = match channel
        .state
        .continued_caller(channel.peer_id, &request.message)
    {
        Some(result) => result.map_err(|_| "original continuation caller is unavailable")?,
        None => channel
            .state
            .continuing_caller(channel.peer_id, parent)
            .ok_or("original continuation caller is unavailable")?,
    };
    let checked = CheckedContinuation { original, caller };
    checked.check_publication(channel).await?;
    if request.on_behalf_of.is_some() {
        match &request.message {
            RemoteActionMessage::Observe { invocation }
            | RemoteActionMessage::Cancel { invocation } => {
                let owns = channel.state.remote_invocations.lock().is_ok_and(|ledger| {
                    ledger
                        .get_continued(&checked.caller, invocation, parent)
                        .is_some()
                });
                if !owns {
                    return Err("invocation is not retained under the original continuation".into());
                }
            }
            RemoteActionMessage::CatalogueRequest { query, .. }
                if query.view_resource.is_some() =>
            {
                return Err("view discovery has no original continuation contract".into());
            }
            _ => {}
        }
    }
    Ok(checked)
}

fn connection_host(
    channel: &CapnpChannelHandle,
    current: AdmittedJoinIdentity,
    message: &RemoteActionMessage,
) -> Result<action_host::ConnectionActionHost, Box<RemoteActionMessage>> {
    let route = channel.route();
    let launch_connection = channel
        .lease
        .as_ref()
        .and_then(|lease| lease.local_media.as_ref())
        .filter(|connection| {
            connection.current()
                && connection.app_id == route.app_id
                && connection.workspace_id == route.cluster_id
        })
        .cloned();
    Ok(action_host::ConnectionActionHost {
        launch_connection,
        continuation: match message {
            RemoteActionMessage::Invoke(invoke) => invoke
                .on_behalf_of
                .map(|parent| {
                    OriginalInvocation::for_delivered(channel, parent).map_err(|_| {
                        Box::new(RemoteActionMessage::Receipt(
                            tilo_actions::remote::InvocationReceipt {
                                invocation: invoke.invocation,
                                status: tilo_actions::remote::ReceiptStatus::Rejected {
                                    error: tilo_actions::remote::ActionHostError::NotPermitted,
                                },
                            },
                        ))
                    })
                })
                .transpose()?,
            _ => None,
        },
        state: Arc::clone(&channel.state),
        own_peer: channel.peer_id,
        origin: InvocationCaller::Local {
            route,
            admitted: Box::new(current),
        },
    })
}

/// Use the same certified host, consent and response path for ordinary and
/// original-caller frames; continuation contributes no separate dispatcher.
pub(super) fn serve(
    channel: Rc<CapnpChannelHandle>,
    current: AdmittedJoinIdentity,
    request: RemoteActionFrame,
    continuation: Option<CheckedContinuation>,
    mut results: channel_handle::RemoteActionResults,
) -> Promise<(), capnp::Error> {
    let route = channel.route();
    let person = current.caller(&route.cluster_id);
    // An edit an effect submits while running for someone is theirs.
    let caller = if let Some(continuation) = &continuation {
        continuation.caller.clone()
    } else if let Some(continued) = channel
        .state
        .continued_caller(channel.peer_id, &request.message)
    {
        match continued {
            Ok(caller) => caller,
            Err(refused) => return reply_frame(*refused, results),
        }
    } else if current.agent_id.is_some() {
        person.clone()
    } else {
        match channel
            .state
            .app_door(&person, &route.app_id, &request.message)
        {
            Ok(caller) => caller,
            Err(refusal) => return reply_frame(*refusal, results),
        }
    };
    if let tilo_actions::remote::RemoteActionMessage::CatalogueRequest { correlation, query } =
        &request.message
        && query.view_resource.is_some()
    {
        return channel.resource_view_query(*correlation, query.clone(), results);
    }

    let host = match connection_host(&channel, current, &request.message) {
        Ok(host) => host,
        Err(refused) => return reply_frame(*refused, results),
    };
    // An application reaching for another app's action it holds no
    // grant for: the person using it is asked first, or it is refused.
    if let (
        tilo_actions::remote::CallerPrincipal::App(app_id),
        tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
    ) = (caller.principal(), &request.message)
    {
        match channel.state.app_action_consent(&person, app_id, invoke) {
            app_consent::ConsentStep::Serve => {}
            app_consent::ConsentStep::Refuse => {
                let refused = tilo_actions::remote::RemoteActionMessage::Receipt(
                    tilo_actions::remote::InvocationReceipt {
                        invocation: invoke.invocation,
                        status: tilo_actions::remote::ReceiptStatus::Rejected {
                            error: tilo_actions::remote::ActionHostError::NotPermitted,
                        },
                    },
                );
                return reply_frame(refused, results);
            }
            app_consent::ConsentStep::Ask(consent) => {
                return app_consent::serve_after_consent(
                    Arc::clone(&channel.state),
                    consent,
                    host,
                    caller,
                    request.message,
                    results,
                );
            }
        }
    }
    let answer = serve_host(&host, &caller, request.message);
    drop(host);
    let Some(answer) = answer else {
        return Promise::err(capnp::Error::failed(
            "only remote-action requests are answered".into(),
        ));
    };
    // Awaited on this local set, which is also where an application's
    // directory answers; the connection's other calls keep flowing.
    Promise::from_future(async move {
        let answer = answer
            .await
            .ok_or_else(|| capnp::Error::failed("the action host dropped the request".into()))?;
        if let Some(continuation) = &continuation {
            continuation
                .check_publication(&channel)
                .await
                .map_err(capnp::Error::failed)?;
        }
        let bytes = RemoteActionFrame::new(answer)
            .encode()
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        results.get().set_frame(&bytes);
        Ok(())
    })
}
