//! Authenticated SDK admission only; deliberately no application delivery.
use super::*;
use crate::local_document_action::Request;

#[expect(
    clippy::too_many_lines,
    reason = "the RPC keeps capture, asynchronous resolution, serialized revalidation and exact-owner admission observation together"
)]
pub(super) fn admit(
    channel: Rc<CapnpChannelHandle>,
    params: &channel_handle::AdmitLocalDocumentActionParams,
    mut results: channel_handle::AdmitLocalDocumentActionResults,
) -> Promise<(), capnp::Error> {
    let parsed = (|| {
        let request =
            Request::decode(params.get()?.get_request()?).map_err(capnp::Error::failed)?;
        let route = channel.route();
        let admitted = channel
            .state
            .revalidate_admission(&route, &channel.verified_identity)?;
        if admitted.certificate.is_none() {
            return Err(capnp::Error::failed(
                "original SDK caller certificate missing".into(),
            ));
        }
        let time = channel
            .state
            .registered_policy_time()
            .map_err(capnp::Error::failed)?
            .ok_or_else(|| capnp::Error::failed("original policy time unavailable".into()))?;
        Ok((request, route, admitted, time))
    })();
    let (request, route, admitted, time) = match parsed {
        Ok(value) => value,
        Err(error) => return Promise::err(error),
    };
    Promise::from_future(async move {
        let state = &channel.state;
        let peer = channel.peer_id;
        let offer = state
            .resolve_local_document_offer(
                &channel.directories,
                &admitted.broker_actor(),
                &route.cluster_id,
                peer,
                &request.revision,
                &request.resource,
                &request.action,
            )
            .await
            .map_err(capnp::Error::failed)?;
        let _admission = state
            .admission_lock
            .lock()
            .map_err(|_| capnp::Error::failed("invocation admission unavailable".into()))?;
        if !state.is_accepting() {
            return Err(capnp::Error::failed("invocation broker retired".into()));
        }
        state
            .checked_policy_time(Some(&time))
            .map_err(capnp::Error::failed)?;
        let current = state.revalidate_admission(&route, &admitted)?;
        offer
            .check_registration(state, &channel.directories, peer)
            .map_err(capnp::Error::failed)?;
        let offered = offer.offered(peer);
        if offered.0.route != route || !offer.has_original_worker() {
            return Err(capnp::Error::failed(
                "local document report route or worker changed".into(),
            ));
        }
        let target = state
            .host_workers
            .lock()
            .map_err(|_| capnp::Error::failed("worker registry unavailable".into()))?
            .get(&peer)
            .cloned()
            .ok_or_else(|| capnp::Error::failed("original worker retired".into()))?;
        state.revalidate_admission(&route, &target)?;
        let certificate = current
            .certificate
            .as_ref()
            .ok_or_else(|| capnp::Error::failed("original caller certificate missing".into()))?;
        let owner = InvocationOwner::AuthenticatedLocal {
            root: certificate.user_public_key,
            device: certificate.device_public_key,
            agent: current.agent_id.clone(),
        };
        let actor = current.broker_actor();
        // Preserve the exact app context privately in the report, including
        // its parent resource and source invocation. Do not promote its invoker,
        // app, group or resource into authenticated caller/target facts.
        let invocation = tilo_actions::InvocationId(request.invocation);
        // No confirmation is synthesized from a report. Actions requiring it
        // fail closed at the same policy gate as ordinary invocation.
        let job = state
            .admit_invocation_from_offered_owned(
                peer,
                offered,
                InvocationInputs {
                    original_request: None,
                    requested: Some(invocation),
                    caller: None,
                    execution: None,
                    argument: request.argument.as_deref(),
                    actor: &actor,
                    confirmation_token: None,
                    context: None,
                },
                AdmissionProvenance {
                    durable: None,
                    revision: Some(&request.revision),
                    origin: Some(InvocationCaller::Local {
                        route: route.clone(),
                        admitted: Box::new(current),
                    }),
                    retained_time: Some(&time),
                },
                Some(&request),
            )
            .map_err(capnp::Error::failed)?;
        drop(job);
        let journal = state
            .journal
            .lock()
            .map_err(|_| capnp::Error::failed("invocation journal unavailable".into()))?;
        let journal = journal
            .as_ref()
            .map_err(|error| capnp::Error::failed(error.clone()))?
            .as_ref()
            .ok_or_else(|| capnp::Error::failed("admission journal unavailable".into()))?;
        let key = InvocationKey {
            room: route.cluster_id,
            host: journal.host(),
            invocation,
            owner,
        };
        let record = journal.lookup(&key).map_err(capnp::Error::failed)?;
        durable_local_invocation::write_observation(
            results.get().init_observation(),
            journal.host(),
            invocation,
            record.as_ref(),
        )
    })
}
