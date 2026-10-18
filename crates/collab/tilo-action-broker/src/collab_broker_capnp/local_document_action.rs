//! Authenticated SDK admission only; deliberately no application delivery.
use super::*;
use crate::local_document_action::Request;

/// Retry acceleration only. Durability belongs to the SDK's private receipt
/// history checkpoint, not the execution journal. Eviction is safe because the
/// invocation address binds every report byte, and no report dispatches work.
#[derive(Default)]
pub(super) struct ReceiptCache {
    pub(super) records: std::collections::VecDeque<ReceiptRecord>,
    pub(super) bytes: usize,
}

#[derive(Clone)]
pub(super) struct ReceiptRecord {
    pub key: InvocationKey,
    pub intent: serde_json::Value,
    pub admission: crate::action_evidence::AdmissionEvidence,
    pub id: u64,
    request_digest: [u8; 32],
    report_digest: [u8; 32],
    peer_id: u64,
    bytes: usize,
}

impl ReceiptRecord {
    #[cfg(test)]
    pub(super) fn admitted_request_is(&self, request: &serde_json::Value) -> bool {
        self.request_digest == admission_request_digest(request)
    }
}

impl ReceiptCache {
    pub(super) fn lookup(&self, key: &InvocationKey) -> Option<&ReceiptRecord> {
        self.records.iter().find(|record| record.key == *key)
    }

    pub(super) fn admit(
        &mut self,
        key: InvocationKey,
        intent: serde_json::Value,
        id: u64,
        peer_id: u64,
    ) -> Result<(), String> {
        let report_digest = admission_request_digest(
            intent
                .get("request")
                .and_then(|request| request.get("report"))
                .ok_or("local report absent")?,
        );
        let digest =
            admission_request_digest(intent.get("request").ok_or("local report request absent")?);
        let mut retained = intent.clone();
        retained
            .as_object_mut()
            .ok_or("invalid local report intent")?
            .remove("request");
        if let Some(record) = self.lookup(&key) {
            if record.request_digest != digest || record.intent != retained {
                return Err("local report identity reused with different input".into());
            }
            return Ok(());
        }
        let facts = invocation_journal::evidence_facts(&key, &intent)?;
        let admission = crate::action_evidence::mint_local_report(facts, &intent)?;
        let bytes = serde_json::to_vec(&(&key, &retained, &admission, &report_digest, peer_id))
            .map_err(|_| "local receipt encoding failed")?
            .len();
        if bytes > 4 * 1024 * 1024 {
            return Err("local receipt exceeds host byte budget".into());
        }
        while self.records.len() >= 128 || self.bytes + bytes > 4 * 1024 * 1024 {
            let retired = self
                .records
                .pop_front()
                .ok_or("invalid local receipt accounting")?;
            self.bytes -= retired.bytes;
        }
        self.bytes += bytes;
        self.records.push_back(ReceiptRecord {
            key,
            intent: retained,
            admission,
            id,
            request_digest: digest,
            report_digest,
            peer_id,
            bytes,
        });
        Ok(())
    }
}

/// Called only under admission serialization. A cached report needs current
/// caller and host custody, but no longer needs the SDK's pre-event pin or a
/// second policy decision (which could spend another one-use grant).
fn cached_receipt(
    channel: &CapnpChannelHandle,
    route: &CrdtRoute,
    request: &Request,
) -> Result<Option<ReceiptRecord>, capnp::Error> {
    let state = &channel.state;
    if !state.is_accepting() {
        return Err(capnp::Error::failed("invocation broker retired".into()));
    }
    let current = state.revalidate_admission(route, &channel.verified_identity)?;
    let target = state
        .host_workers
        .lock()
        .map_err(|_| capnp::Error::failed("worker registry unavailable".into()))?
        .get(&channel.peer_id)
        .cloned()
        .ok_or_else(|| capnp::Error::failed("original worker retired".into()))?;
    state.revalidate_admission(route, &target)?;
    let certificate = current
        .certificate
        .as_ref()
        .ok_or_else(|| capnp::Error::failed("original caller certificate missing".into()))?;
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
        room: route.cluster_id.clone(),
        host: journal.host(),
        invocation: tilo_actions::InvocationId(request.invocation),
        owner: InvocationOwner::AuthenticatedLocal {
            root: certificate.user_public_key,
            device: certificate.device_public_key,
            agent: current.agent_id,
        },
    };
    let receipts = state
        .local_report_receipts
        .lock()
        .map_err(|_| capnp::Error::failed("local receipt cache unavailable".into()))?;
    let Some(record) = receipts.lookup(&key) else {
        return Ok(None);
    };
    let report = serde_json::to_value(request)
        .map_err(|_| capnp::Error::failed("local report encoding failed".into()))?;
    if record.peer_id != channel.peer_id
        || record.intent["app_id"] != route.app_id
        || record.intent["record_id"] != route.record_id
        || record.report_digest != admission_request_digest(&report)
    {
        return Err(capnp::Error::failed(
            "local report identity reused with different input or route".into(),
        ));
    }
    Ok(Some(record.clone()))
}

fn write_observation(
    mut out: runtime_collab::collab_capnp::published_invocation_observation::Builder<'_>,
    record: &ReceiptRecord,
) -> Result<(), capnp::Error> {
    out.set_host(record.key.host.as_bytes());
    out.set_invocation(record.key.invocation.0.as_bytes());
    out.set_found(true);
    out.set_id(record.id);
    out.set_update_json(
        &serde_json::to_string(&tilo_actions::InvocationUpdate::Accepted {
            invocation: record.key.invocation,
        })
        .map_err(|_| capnp::Error::failed("local observation encoding failed".into()))?,
    );
    out.set_admission_json(
        &serde_json::to_string(&record.admission)
            .map_err(|_| capnp::Error::failed("local admission encoding failed".into()))?,
    );
    out.set_terminal_json("");
    out.set_uncertain(false);
    out.set_cancellation_requested(false);
    Ok(())
}

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
        if request.bound_invocation().map_err(capnp::Error::failed)? != request.invocation {
            return Err(capnp::Error::failed(
                "local report invocation does not bind capture".into(),
            ));
        }
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
        {
            let _admission = state
                .admission_lock
                .lock()
                .map_err(|_| capnp::Error::failed("invocation admission unavailable".into()))?;
            if let Some(record) = cached_receipt(&channel, &route, &request)? {
                return write_observation(results.get().init_observation(), &record);
            }
        }
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
            .await;
        let _admission = state
            .admission_lock
            .lock()
            .map_err(|_| capnp::Error::failed("invocation admission unavailable".into()))?;
        // Resolution yields: another concurrent report may have won admission.
        if let Some(record) = cached_receipt(&channel, &route, &request)? {
            return write_observation(results.get().init_observation(), &record);
        }
        let offer = offer.map_err(capnp::Error::failed)?;
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
        let receipts = state
            .local_report_receipts
            .lock()
            .map_err(|_| capnp::Error::failed("local receipt cache unavailable".into()))?;
        let record = receipts
            .lookup(&key)
            .ok_or_else(|| capnp::Error::failed("local report receipt unavailable".into()))?;
        write_observation(results.get().init_observation(), record)
    })
}
