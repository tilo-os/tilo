//! Finite turns on the existing authenticated storage enrollment connection.
//! The registry, not the wire reply, owns every possibly submitted native effect.
use super::*;
use super::{coordinator::OwnedReplacement, pending_source::AuthenticatedPendingSource};
use runtime_fs::external_history::{EnrollmentCommand, replacement_control::ReplacementReply};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
const MAX_REQUESTS: usize = 64;

#[derive(Clone, Eq, PartialEq)]
struct RetiredSelector {
    successor_request_id: String,
    operation_id: String,
    transaction_digest: [u8; 32],
}
impl RetiredSelector {
    fn reply(&self, request_id: String) -> ReplacementReply {
        ReplacementReply::Retired {
            request_id,
            operation_id: self.operation_id.clone(),
            transaction_digest: self.transaction_digest,
        }
    }
}
/// Only successfully joined actual owners enter this bounded retry cache.
#[derive(Default)]
struct RetiredReplacements {
    known: BTreeMap<String, RetiredSelector>,
    order: std::collections::VecDeque<String>,
}
impl RetiredReplacements {
    fn matches(&self, request: &str, selector: &RetiredSelector) -> bool {
        self.known.get(request) == Some(selector)
    }
    fn insert(&mut self, request: String, selector: RetiredSelector) {
        self.order.retain(|id| id != &request);
        self.order.push_back(request.clone());
        self.known.insert(request, selector);
        while self.order.len() > MAX_REQUESTS {
            if let Some(expired) = self.order.pop_front() {
                self.known.remove(&expired);
            }
        }
    }
}

async fn control<T: Send + 'static>(
    stream: &UnixStream,
    cancel: &AtomicBool,
    deadline: Instant,
    call: impl FnOnce(&mut UnixStream) -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let (_, result) = owned_io::exchange(stream.try_clone()?, cancel, deadline, call).await?;
    result
}
async fn write_replacement(
    stream: &UnixStream,
    reply: &ReplacementReply,
    cancel: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    let bytes = reply.encode()?;
    control(stream, cancel, deadline, move |stream| {
        wire_frame::Framing::little_endian(ReplacementReply::LIMIT)
            .reject_empty()
            .write(stream, &bytes)
            .map_err(io::Error::other)
    })
    .await
}
async fn write_history(
    stream: &UnixStream,
    reply: &runtime_fs::external_history::read_control::HistoryReadReply,
    cancel: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    let bytes = reply.encode()?;
    control(stream, cancel, deadline, move |stream| {
        wire_frame::Framing::little_endian(
            runtime_fs::external_history::read_control::HistoryReadReply::LIMIT,
        )
        .reject_empty()
        .write(stream, &bytes)
        .map_err(io::Error::other)
    })
    .await
}
/// Root EOF or refusal cannot abandon a possibly submitted operation. The
/// daemon keeps its actual pending RPC driver/job/pin and durable journal even
/// when this private lane can no longer yield a positive reply. Recovery is
/// deliberately conservative; an unknown effect is never treated as undone.
async fn hold_unknown(owners: &mut BTreeMap<String, OwnedReplacement>) -> io::Result<()> {
    let cancel = AtomicBool::new(false);
    for owner in owners.values_mut() {
        owner
            .cancel_owned(&cancel, Instant::now() + Duration::from_secs(10))
            .await;
    }
    for owner in owners.values() {
        owner.retire_repository_check().await?;
    }
    while owners.values().any(OwnedReplacement::unresolved_effect) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}
async fn retire_lane(
    owners: &mut BTreeMap<String, OwnedReplacement>,
    readers: &mut BTreeMap<String, super::read_handle::ExternalHistoryReadHandle>,
) -> io::Result<()> {
    let mut failure = None;
    while let Some((_, reader)) = readers.pop_first() {
        if let Err(error) = reader.retire().await {
            failure = Some(error);
        }
    }
    hold_unknown(owners).await?;
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
#[expect(
    clippy::too_many_lines,
    reason = "one closed private connection controller preserves framing and actual unresolved native custody across every command and error"
)]
pub(super) async fn serve(
    admission: ExternalSourceAdmission,
    stream: UnixStream,
    cancel: Arc<AtomicBool>,
    registry: super::lane_controller::AcknowledgedRegistry,
) -> io::Result<()> {
    let admission = Rc::new(RefCell::new(admission));
    let mut owners = BTreeMap::<String, OwnedReplacement>::new();
    let mut previous = None;
    let mut retired_replacements = RetiredReplacements::default();
    let mut readers = BTreeMap::<String, super::read_handle::ExternalHistoryReadHandle>::new();
    let mut retired_readers = std::collections::BTreeSet::new();
    let mut retired_order = std::collections::VecDeque::new();
    loop {
        tokio::task::yield_now().await;
        let mut readiness = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&raw mut readiness, 1, 0) };
        if ready < 0 {
            retire_lane(&mut owners, &mut readers).await?;
            return Err(io::Error::last_os_error());
        }
        if ready == 0 {
            if cancel.load(Ordering::Acquire) {
                return retire_lane(&mut owners, &mut readers).await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        }
        let deadline = match Instant::now().checked_add(Duration::from_secs(10)) {
            Some(deadline) => deadline,
            None => {
                retire_lane(&mut owners, &mut readers).await?;
                return Err(refused());
            }
        };
        let command =
            control(
                &stream,
                &cancel,
                deadline,
                |stream| match wire_frame::Framing::little_endian(4096)
                    .reject_empty()
                    .read(stream)
                    .map_err(io::Error::other)?
                {
                    Some(bytes) => EnrollmentCommand::decode(&bytes).map(Some),
                    None => Ok(None),
                },
            )
            .await;
        let command = match command {
            Ok(Some(command)) => command,
            Ok(None) => return retire_lane(&mut owners, &mut readers).await,
            Err(error) => {
                retire_lane(&mut owners, &mut readers).await?;
                return Err(error);
            }
        };
        if matches!(command, EnrollmentCommand::Retire) {
            return retire_lane(&mut owners, &mut readers).await;
        }
        let result: io::Result<()> = async {
            match command {
                EnrollmentCommand::Retire => Err(refused()),
                EnrollmentCommand::Refresh { nonce } => {
                    let bytes = {
                        let mut a = admission.try_borrow_mut().map_err(|_| refused())?;
                        a.refresh_async(&cancel, deadline).await?;
                        let mut reply = EnrolledVerifier {
                            schema: 1,
                            daemon_nonce: a.daemon_nonce,
                            requester_nonce: nonce,
                            context_digest: a.context.digest()?,
                            verifier: a.context.verifier.clone(),
                            signature: Vec::new(),
                        };
                        reply.signature = a
                            .factory
                            .credentials
                            .signing_key()
                            .sign(&reply.signing_bytes()?)
                            .to_bytes()
                            .to_vec();
                        a.factory.check_current()?;
                        a.helper.check_current()?;
                        serde_json::to_vec(&reply).map_err(io::Error::other)?
                    };
                    control(&stream, &cancel, deadline, move |stream| {
                        frame().write(stream, &bytes).map_err(io::Error::other)
                    })
                    .await
                }
                EnrollmentCommand::RetireHistoryRead { request_id } => {
                    use runtime_fs::external_history::read_control::HistoryReadReply;
                    let reply = if !admission
                        .try_borrow()
                        .map_err(|_| refused())?
                        .purpose
                        .history_reader()
                    {
                        HistoryReadReply::Refused { request_id }
                    } else if let Some(reader) = readers.remove(&request_id) {
                        reader.retire().await?;
                        retired_readers.insert(request_id.clone());
                        retired_order.push_back(request_id.clone());
                        while retired_order.len() > MAX_REQUESTS {
                            if let Some(expired) = retired_order.pop_front() {
                                retired_readers.remove(&expired);
                            }
                        }
                        HistoryReadReply::Retired { request_id }
                    } else if retired_readers.contains(&request_id) {
                        HistoryReadReply::Retired { request_id }
                    } else {
                        HistoryReadReply::Refused { request_id }
                    };
                    write_history(&stream, &reply, &cancel, deadline).await
                }
                EnrollmentCommand::AdoptHistoryRead { request_id } => {
                    use runtime_fs::external_history::read_control::HistoryReadReply;
                    let descriptors = control(&stream, &cancel, deadline, |stream| {
                        Ok(super::read_handle::ReadDescriptors {
                            sealed: receive_file(stream, 68)?,
                            stream: receive_file(stream, 69)?,
                            program: receive_file(stream, 70)?,
                        })
                    })
                    .await;
                    match descriptors {
                        Err(error) => Err(error),
                        Ok(descriptors) => {
                            let adopted = if readers.len() >= MAX_REQUESTS
                                && !readers.contains_key(&request_id)
                                || !admission
                                    .try_borrow()
                                    .map_err(|_| refused())?
                                    .purpose
                                    .history_reader()
                            {
                                drop(descriptors);
                                Err(refused())
                            } else {
                                super::read_handle::ExternalHistoryReadHandle::adopt(
                                    Rc::clone(&admission),
                                    descriptors,
                                    &request_id,
                                    &registry,
                                    &cancel,
                                    deadline,
                                )
                                .await
                            };
                            let reply = match adopted {
                                Ok((handle, completed_request_id)) => {
                                    // Own the new RPC driver before awaiting retirement: an
                                    // old driver join failure must still leave the new one
                                    // in canonical lane teardown custody.
                                    if let Some(old) = readers.insert(request_id.clone(), handle) {
                                        old.retire().await?;
                                    }
                                    retired_readers.remove(&request_id);
                                    retired_order.retain(|id| id != &request_id);
                                    HistoryReadReply::Adopted {
                                        request_id,
                                        completed_request_id,
                                    }
                                }
                                Err(_) => {
                                    retired_readers.insert(request_id.clone());
                                    retired_order.retain(|id| id != &request_id);
                                    retired_order.push_back(request_id.clone());
                                    while retired_order.len() > MAX_REQUESTS {
                                        if let Some(expired) = retired_order.pop_front() {
                                            retired_readers.remove(&expired);
                                        }
                                    }
                                    HistoryReadReply::Refused { request_id }
                                }
                            };
                            write_history(&stream, &reply, &cancel, deadline).await
                        }
                    }
                }
                EnrollmentCommand::QueryHistory { request_id, query } => {
                    use runtime_fs::external_history::read_control::HistoryReadReply;
                    let read = match readers.get(&request_id) {
                        Some(reader) => reader.query(&query, &cancel, deadline).await,
                        None => Err(refused()),
                    };
                    let reply = match read {
                        Ok(bytes) => HistoryReadReply::Read { request_id, bytes },
                        Err(_) => HistoryReadReply::Refused { request_id },
                    };
                    write_history(&stream, &reply, &cancel, deadline).await
                }
                EnrollmentCommand::StartReplacement { request_id } => {
                    // Always consume all three exact transport slots before an operation
                    // refusal, so a duplicate request cannot leave shared framing
                    // misaligned. Transport errors remain whole-lane failures.
                    let descriptors = control(&stream, &cancel, deadline, |stream| {
                        Ok((
                            receive_file(stream, 63)?,
                            receive_file(stream, 64)?,
                            receive_file(stream, 65)?,
                        ))
                    })
                    .await;
                    match descriptors {
                        Err(error) => Err(error),
                        Ok((sealed, source_stream, program)) => {
                            if admission.try_borrow().map_err(|_| refused())?.purpose
                                != AdmissionPurpose::SelectedReplace
                                || owners.contains_key(&request_id)
                                || owners.len() >= MAX_REQUESTS
                                || owners.values().any(OwnedReplacement::unresolved_effect)
                                || {
                                    let shared = registry.try_borrow().map_err(|_| refused())?;
                                    shared.reserved.contains(&request_id)
                                        || shared.reserved.len() >= MAX_REQUESTS
                                }
                            {
                                write_replacement(
                                    &stream,
                                    &ReplacementReply::Refused {
                                        request_id,
                                        operation_id: None,
                                    },
                                    &cancel,
                                    deadline,
                                )
                                .await
                            } else {
                                registry
                                    .try_borrow_mut()
                                    .map_err(|_| refused())?
                                    .reserved
                                    .insert(request_id.clone());
                                let pending = AuthenticatedPendingSource::receive(
                                    Rc::clone(&admission),
                                    sealed,
                                    UnixStream::from(std::os::fd::OwnedFd::from(source_stream)),
                                    program,
                                    &cancel,
                                    deadline,
                                )
                                .await;
                                let owner = match pending {
                                    Ok(pending) => OwnedReplacement::reserve(
                                        request_id.clone(),
                                        Rc::new(pending),
                                        Rc::clone(&admission),
                                        previous.as_deref(),
                                    ),
                                    Err(error) => Err(error),
                                };
                                match owner {
                                    Err(_) => {
                                        registry
                                            .try_borrow_mut()
                                            .map_err(|_| refused())?
                                            .reserved
                                            .remove(&request_id);
                                        write_replacement(
                                            &stream,
                                            &ReplacementReply::Refused {
                                                request_id,
                                                operation_id: None,
                                            },
                                            &cancel,
                                            deadline,
                                        )
                                        .await
                                    }
                                    Ok(owner) => {
                                        // Own before the first start syscall/RPC.
                                        owners.insert(request_id.clone(), owner);
                                        let owner = match owners.get_mut(&request_id) {
                                            Some(owner) => owner,
                                            None => {
                                                retire_lane(&mut owners, &mut readers).await?;
                                                return Err(refused());
                                            }
                                        };
                                        // Start acknowledges held admission/preparation only.
                                        // The first Poll drives the same once-owned native start.
                                        write_replacement(
                                            &stream,
                                            &owner.status(),
                                            &cancel,
                                            deadline,
                                        )
                                        .await
                                    }
                                }
                            }
                        }
                    }
                }
                EnrollmentCommand::RetireReplacement {
                    request_id,
                    successor_request_id,
                    operation_id,
                    transaction_digest,
                } => {
                    let selector = RetiredSelector {
                        successor_request_id,
                        operation_id,
                        transaction_digest,
                    };
                    let reply = if retired_replacements.matches(&request_id, &selector) {
                        selector.reply(request_id)
                    } else {
                        let retirement: io::Result<()> = async {
                            let old = owners.get(&request_id).ok_or_else(refused)?;
                            let old_ack = old.acknowledged().ok_or_else(refused)?;
                            let successor = owners
                                .get(&selector.successor_request_id)
                                .and_then(OwnedReplacement::acknowledged)
                                .ok_or_else(refused)?;
                            if previous
                                .as_ref()
                                .is_none_or(|current| !Rc::ptr_eq(current, &successor))
                                || Rc::ptr_eq(&old_ack, &successor)
                            {
                                return Err(refused());
                            }
                            old.check_retirement_successor(
                                &successor,
                                &selector.operation_id,
                                selector.transaction_digest,
                                &cancel,
                                deadline,
                            )
                            .await?;
                            {
                                let mut shared =
                                    registry.try_borrow_mut().map_err(|_| refused())?;
                                let baseline = match shared.completed.get(&request_id) {
                                    Some(registered) if Rc::ptr_eq(registered, &old_ack) => 3,
                                    Some(_) => return Err(refused()),
                                    None => 2,
                                };
                                // Owner + local borrow + optional discovery registry;
                                // actual Reader derivation must finish before cleanup.
                                if Rc::strong_count(&old_ack) != baseline {
                                    return Err(refused());
                                }
                                shared.completed.remove(&request_id);
                            }
                            // Owner stays in its map through observed task exit. Only
                            // this RPC connection retires; the shared worker stays alive.
                            old.retire_acknowledged_rpc().await?;
                            registry
                                .try_borrow_mut()
                                .map_err(|_| refused())?
                                .reserved
                                .remove(&request_id);
                            owners.remove(&request_id);
                            Ok(())
                        }
                        .await;
                        if retirement.is_ok() {
                            retired_replacements.insert(request_id.clone(), selector.clone());
                            selector.reply(request_id)
                        } else {
                            ReplacementReply::Refused {
                                request_id,
                                operation_id: Some(selector.operation_id),
                            }
                        }
                    };
                    write_replacement(&stream, &reply, &cancel, deadline).await
                }
                EnrollmentCommand::PollReplacement { request_id } => {
                    if let Some(owner) = owners.get_mut(&request_id) {
                        if let Err(error) = owner.advance(&cancel, deadline).await {
                            owner.record_turn_error(&error);
                        }
                        if let Some(done) = owner.acknowledged() {
                            let mut shared = registry.try_borrow_mut().map_err(|_| refused())?;
                            if !shared.reserved.contains(&request_id) {
                                return Err(refused());
                            }
                            if !shared.completed.contains_key(&request_id) {
                                shared
                                    .completed
                                    .insert(request_id.clone(), Rc::clone(&done));
                            }
                            previous = Some(done);
                        }
                        write_replacement(&stream, &owner.status(), &cancel, deadline).await
                    } else {
                        write_replacement(
                            &stream,
                            &ReplacementReply::Refused {
                                request_id,
                                operation_id: None,
                            },
                            &cancel,
                            deadline,
                        )
                        .await
                    }
                }
                EnrollmentCommand::CancelReplacement { request_id } => {
                    if let Some(owner) = owners.get_mut(&request_id) {
                        owner.cancel_owned(&cancel, deadline).await;
                        write_replacement(&stream, &owner.status(), &cancel, deadline).await
                    } else {
                        write_replacement(
                            &stream,
                            &ReplacementReply::Refused {
                                request_id,
                                operation_id: None,
                            },
                            &cancel,
                            deadline,
                        )
                        .await
                    }
                }
            }
        }
        .await;
        if let Err(error) = result {
            retire_lane(&mut owners, &mut readers).await?;
            return Err(error);
        }
    }
}

#[cfg(test)]
mod retirement_cache_tests {
    use super::*;
    #[test]
    fn retirement_retry_cache_is_resident_bounded_and_exact() {
        let mut cache = RetiredReplacements::default();
        let selector = RetiredSelector {
            successor_request_id: "successor".into(),
            operation_id: "operation".into(),
            transaction_digest: [1; 32],
        };
        for number in 0..(MAX_REQUESTS * 3) {
            cache.insert(format!("retired-{number}"), selector.clone());
        }
        assert_eq!(cache.known.len(), MAX_REQUESTS);
        assert_eq!(cache.order.len(), MAX_REQUESTS);
        assert!(!cache.matches("retired-0", &selector));
        let latest = format!("retired-{}", MAX_REQUESTS * 3 - 1);
        assert!(cache.matches(&latest, &selector));
        let changed = RetiredSelector {
            transaction_digest: [2; 32],
            ..selector.clone()
        };
        assert!(!cache.matches(&latest, &changed));
        assert!(!cache.matches("never-retired", &selector));
        cache.insert(latest.clone(), selector.clone());
        assert_eq!(cache.known.len(), MAX_REQUESTS);
        assert_eq!(cache.order.len(), MAX_REQUESTS);
        assert!(cache.matches(&latest, &selector));
    }
}
