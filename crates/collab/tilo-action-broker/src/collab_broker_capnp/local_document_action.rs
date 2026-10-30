//! Authenticated SDK admission only; deliberately no application delivery.
use super::{
    AdmissionProvenance, AdmittedJoinIdentity, CapnpBrokerState, CapnpChannelHandle, CrdtRoute,
    InvocationCaller, InvocationInputs, InvocationJournal, InvocationKey, InvocationOwner,
    PublishedAction, Rc, admission_request_digest, invocation_journal,
};
use crate::BrokerError;
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
    /// The connection whose report this admitted.
    #[cfg(test)]
    pub(super) const fn peer_id(&self) -> u64 {
        self.peer_id
    }

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
        intent: &serde_json::Value,
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
        let facts = invocation_journal::evidence_facts(&key, intent)?;
        let admission = crate::action_evidence::mint_local_report(facts, intent)?;
        let bytes = serde_json::to_vec(&(&key, &retained, &admission, &report_digest, peer_id))
            .map_err(|_| "local receipt encoding failed")?
            .len();
        if bytes > 4 * 1024 * 1024 {
            return Err("local receipt exceeds host byte budget".into());
        }
        while self.records.len() >= 128 || self.bytes.saturating_add(bytes) > 4 * 1024 * 1024 {
            let retired = self
                .records
                .pop_front()
                .ok_or("invalid local receipt accounting")?;
            self.bytes = self.bytes.saturating_sub(retired.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
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

/// Recovery provenance must match the live authenticated channel, not replace it.
fn validate_report_custody(
    request: &Request,
    route: &CrdtRoute,
    current: &AdmittedJoinIdentity,
) -> Result<(), BrokerError> {
    let custody = request
        .custody
        .as_ref()
        .ok_or_else(|| rejected("local report authenticated custody missing".into()))?;
    let certificate = current
        .certificate
        .as_ref()
        .ok_or_else(|| rejected("original SDK caller certificate missing".into()))?;
    if !custody.valid()
        || custody.root != certificate.user_public_key
        || custody.device != certificate.device_public_key
        || custody.agent != current.agent_id
        || custody.app_id != route.app_id
        || custody.room != route.cluster_id
        || custody.document != route.record_id
    {
        return Err(rejected(
            "local report authenticated custody changed".into(),
        ));
    }
    Ok(())
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "`state` is a borrow of the channel's broker state, not a guard; the journal and receipt locks are taken and released in their own helpers"
)]
fn cached_receipt(
    channel: &CapnpChannelHandle,
    route: &CrdtRoute,
    request: &Request,
) -> Result<Option<ReceiptRecord>, BrokerError> {
    let state = &channel.state;
    if !state.is_accepting() {
        return Err(BrokerError::failed("invocation broker retired".into()));
    }
    let current = state.revalidate_admission(route, &channel.verified_identity)?;
    validate_report_custody(request, route, &current)?;
    // A worker is registered on a route when its connection joins it, for
    // as long as the connection lives, so a connection that is no worker
    // here never was: nothing reported on it can have been admitted, and
    // nothing will be. Say so definitively, or the caller keeps every
    // report (and the surface it was made under) forever.
    let target = state
        .host_workers
        .lock()
        .map_err(|_| BrokerError::failed("worker registry unavailable".into()))?
        .get(channel.peer_id, route)
        .cloned()
        .ok_or_else(|| not_a_host_worker(state))?;
    state.revalidate_admission(route, &target)?;
    let certificate = current
        .certificate
        .as_ref()
        .ok_or_else(|| BrokerError::failed("original caller certificate missing".into()))?;
    let key = InvocationKey {
        room: route.cluster_id.clone(),
        host: journal_host(state)?,
        invocation: tilo_actions::InvocationId(request.invocation),
        owner: InvocationOwner::AuthenticatedLocal {
            root: certificate.user_public_key,
            device: certificate.device_public_key,
            agent: current.agent_id,
        },
    };
    let Some(record) = receipt(state, &key)? else {
        return Ok(None);
    };
    let report = serde_json::to_value(request)
        .map_err(|_| BrokerError::failed("local report encoding failed".into()))?;
    let intent_str = |field: &str| record.intent.get(field).and_then(serde_json::Value::as_str);
    if record.peer_id != channel.peer_id
        || intent_str("app_id") != Some(route.app_id.as_str())
        || intent_str("record_id") != Some(route.record_id.as_str())
        || record.report_digest != admission_request_digest(&report)
    {
        return Err(BrokerError::failed(
            "local report identity reused with different input or route".into(),
        ));
    }
    Ok(Some(record))
}

/// The host the admission journal records invocations under; its lock is
/// held only to read it.
fn journal_host(state: &CapnpBrokerState) -> Result<uuid::Uuid, BrokerError> {
    let journal = state
        .journal
        .lock()
        .map_err(|_| BrokerError::failed("invocation journal unavailable".into()))?;
    journal
        .as_ref()
        .map_err(|error| BrokerError::failed(error.clone()))?
        .as_ref()
        .map(InvocationJournal::host)
        .ok_or_else(|| BrokerError::failed("admission journal unavailable".into()))
}

/// The cached receipt of `key`, if the cache still holds it; its lock is
/// held only to copy it out.
fn receipt(
    state: &CapnpBrokerState,
    key: &InvocationKey,
) -> Result<Option<ReceiptRecord>, BrokerError> {
    let receipts = state
        .local_report_receipts
        .lock()
        .map_err(|_| BrokerError::failed("local receipt cache unavailable".into()))?;
    Ok(receipts.lookup(key).cloned())
}

fn observation(
    record: &ReceiptRecord,
) -> Result<runtime_collab::PublishedInvocationObservation, BrokerError> {
    Ok(runtime_collab::PublishedInvocationObservation {
        host: *record.key.host.as_bytes(),
        invocation: *record.key.invocation.0.as_bytes(),
        result: Some(runtime_collab::PublishedInvocationResult {
            id: record.id,
            update_json: serde_json::to_string(&tilo_actions::InvocationUpdate::Accepted {
                invocation: record.key.invocation,
            })
            .map_err(|_| BrokerError::failed("local observation encoding failed".into()))?,
            admission_json: Some(
                serde_json::to_string(&record.admission)
                    .map_err(|_| BrokerError::failed("local admission encoding failed".into()))?,
            ),
            terminal_json: None,
            uncertain: false,
            cancellation_requested: false,
        }),
    })
}

fn validate_report_offer(request: &Request, action: &PublishedAction) -> Result<(), String> {
    if (!matches!(
        action.operation_kind,
        tilo_actions::OperationKind::Document | tilo_actions::OperationKind::View
    ) && action.contract.need().kind != tilo_actions::ActionKind::Read)
        || (action.operation_kind != tilo_actions::OperationKind::Document
            && !request.mutation_hashes.is_empty())
    {
        return Err(
            "local action report requires authenticated document or read-only admission".into(),
        );
    }
    request.typed_arguments(&action.params)?;
    Ok(())
}

/// The answer to a report on a route its connection is no worker on: a
/// connection no launcher authenticated, or a route of another app than the
/// one launched. Retirement clears every registration, and says nothing
/// about what was admitted before it: that stays uncertain.
fn not_a_host_worker(state: &CapnpBrokerState) -> BrokerError {
    if state.is_accepting() {
        rejected(runtime_collab::LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING.into())
    } else {
        BrokerError::failed("invocation broker retired".into())
    }
}

fn rejected(mut reason: String) -> BrokerError {
    reason.insert_str(0, runtime_collab::LOCAL_DOCUMENT_REJECTED);
    BrokerError::failed(reason)
}

#[expect(
    clippy::too_many_lines,
    reason = "the RPC keeps capture, asynchronous resolution, serialized revalidation and exact-owner admission observation together"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "`state` borrows the channel's broker state; the admission guard is held to the end on purpose, serializing admission"
)]
#[allow(
    clippy::future_not_send,
    clippy::needless_pass_by_value,
    reason = "admission runs on the broker's LocalSet; its future holds the channel"
)]
pub(super) fn admit(
    channel: Rc<CapnpChannelHandle>,
    report: Vec<u8>,
) -> impl std::future::Future<Output = Result<runtime_collab::PublishedInvocationObservation, BrokerError>>
{
    let parsed = (|| {
        let request = Request::decode(&report).map_err(rejected)?;
        if request.bound_invocation().map_err(rejected)? != request.invocation {
            return Err(rejected(
                "local report invocation does not bind capture".into(),
            ));
        }
        let route = channel.route();
        let admitted = channel
            .state
            .revalidate_admission(&route, &channel.verified_identity)?;
        validate_report_custody(&request, &route, &admitted)?;
        Ok((request, route, admitted))
    })();
    async move {
        let (request, route, admitted) = parsed?;
        let state = &channel.state;
        let peer = channel.peer_id;
        {
            let _admission = state
                .admission_lock
                .lock()
                .map_err(|_| BrokerError::failed("invocation admission unavailable".into()))?;
            if let Some(record) = cached_receipt(&channel, &route, &request)? {
                return observation(&record);
            }
        }
        // A fresh policy failure cannot prove an earlier admission never
        // happened. Exact cached retries return before consulting policy time.
        let time = state
            .registered_policy_time()
            .map_err(BrokerError::failed)?
            .ok_or_else(|| BrokerError::failed("original policy time unavailable".into()))?;
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
        let admission = state
            .admission_lock
            .lock()
            .map_err(|_| BrokerError::failed("invocation admission unavailable".into()))?;
        // Resolution yields: another concurrent report may have won admission.
        if let Some(record) = cached_receipt(&channel, &route, &request)? {
            return observation(&record);
        }
        let offer = offer.map_err(BrokerError::failed)?;
        state
            .checked_policy_time(Some(&time))
            .map_err(BrokerError::failed)?;
        let current = state.revalidate_admission(&route, &admitted)?;
        validate_report_custody(&request, &route, &current)?;
        offer
            .check_registration(state, &channel.directories, peer)
            .map_err(rejected)?;
        let offered = offer.offered(peer);
        // These are structural nonadmissions, checked after exact replay and
        // before any journal/cache mutation. Transport, policy and admission
        // failures remain uncertain: none proves an earlier attempt absent.
        validate_report_offer(&request, &offered.1).map_err(rejected)?;
        if offered.0.route != route || !offer.has_original_worker() {
            return Err(rejected(
                "local document report route or worker changed".into(),
            ));
        }
        let target = state
            .host_workers
            .lock()
            .map_err(|_| rejected("worker registry unavailable".into()))?
            .get(peer, &route)
            .cloned()
            .ok_or_else(|| not_a_host_worker(state))?;
        state
            .revalidate_admission(&route, &target)
            .map_err(|error| rejected(error.to_string()))?;
        let certificate = current
            .certificate
            .as_ref()
            .ok_or_else(|| rejected("original caller certificate missing".into()))?;
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
        let staged = state
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
                        time: Some(time.clone()),
                    }),
                    retained_time: Some(&time),
                },
                Some(&request),
            )
            .map_err(BrokerError::failed)?;
        drop(admission);
        drop(staged.start(state).await.map_err(BrokerError::failed)?);
        let key = InvocationKey {
            room: route.cluster_id,
            host: journal_host(state)?,
            invocation,
            owner,
        };
        let record = receipt(state, &key)?
            .ok_or_else(|| BrokerError::failed("local report receipt unavailable".into()))?;
        observation(&record)
    }
}
