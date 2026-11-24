//! Another of the person's devices asking for its disk-unlock half.
//! Every device of the person holds the sibling halves of the others, sealed to the person's
//! root-derived secret-sync key ([`crate::platform::fabric::unlock`]). Early boot of an enrolled device
//! reaches this one over [`tilo_unlock_protocol::UNLOCK_ALPN`]: it presents the boot key
//! certificate the person's root signed, answers this device's challenge, and waits. The person is
//! asked on the session's question sheet, with the approval code the boot screen shows; an approval
//! releases the half sealed to the boot's ephemeral key, and the next boot needs another approval.
//! The answer is a fact this device keeps ([`UnlockDecisionEntry`]) and their other devices carry:
//! a boot whose stream went away before the answer reached it hears the same answer again, with a
//! half sealed to its own new key where the person allowed it, instead of asking them twice.
//! `tilo-disk-encrypt enrol --sibling` reaches [`Unlock::enrol`] through the session's enrolment
//! socket ([`tilo_unlock_protocol::ENROL_SOCKET`]): the boot key is certified by the person's root,
//! the sibling half is sealed to the secret-sync key, and the enrolment is kept in this device's
//! part of the person's store for their other devices to carry. A device that holds no root asks
//! one that does over [`tilo_unlock_protocol::ENROL_ALPN`] instead ([`Unlock::serve_enrol`]); that
//! device asks its person first.

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::platform::fabric::DeviceId;
use crate::platform::fabric::SharedLink;
use crate::platform::fabric::{
    AutoApproval, Decided, DecisionOutcome, MAX_UNLOCK_DECISIONS, MergedUnlock, UnlockDecision,
    UnlockDecisionEntry, UnlockEnrolment, UnlockFabricState, UnlockSibling, UnlockView,
    decision_for, merged_unlock_state,
};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tilo_service::{AskSender, Asked, Question};
use tilo_unlock_protocol::{
    APPROVAL_TIMEOUT_NS, CHALLENGE_LIFETIME_NS, ClientFrame, ENROL_SOCKET, EnrolReply,
    EnrolRequest, EnrolResponse, RefusalReason, SiblingContact, SiblingFrame, SlotId, UnlockHello,
    UnlockRequest, decode_frame, encode_frame, frame_len,
};
use tilo_user_identity::{
    CertifiedFleet, DeviceCert, DeviceCertification, device_cert_signing_digest,
    verify_device_cert,
};

/// The kind the session's sheet reads a disk-unlock request as.
pub(crate) const KIND: &str = "network.device-unlock";
/// The kind it reads another device's enrolment request as.
pub(crate) const ENROL_KIND: &str = "network.device-enrol";

/// How long a boot may take to answer its challenge.
const ANSWER_TIMEOUT: Duration = Duration::from_nanos(CHALLENGE_LIFETIME_NS);
/// How long an approved boot may take to read the half once decided.
const WRITE_GRACE: Duration = Duration::from_secs(2);

/// A request waiting for the owner, as the sheet shows it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct UnlockQuestion {
    /// The code the boot screen shows; the person compares them.
    pub approval_code: String,
    /// The device that asks, as the person's store names it.
    pub device: String,
    /// The slot it wants unlocked.
    pub slot: String,
}

/// Another device's enrolment waiting for the person, as the sheet shows it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct EnrolQuestion {
    /// The device that asks, as this device's fabric knows it.
    pub device: String,
    /// Whether it asked to unlock its disk without being asked again each boot.
    pub unattended: bool,
}

/// The person's devices this session answers as, as the daemon that holds their record lists
/// them, and whether that daemon keeps their root.
struct Ownership {
    fleet: CertifiedFleet,
    /// Whether this device holds the person's root, so it can release.
    key_holder: bool,
}

/// Why an enrolment was not signed here: another device must, or this
/// request is not one this device accepts.
enum NotEnrolled {
    /// This device does not hold the person's root.
    NotKeyHolder,
    /// Anything else, with a reason for the person.
    Reason(String),
}

/// The sibling side of disk unlock, served by one session.
pub(crate) struct Unlock {
    link: SharedLink,
    questions: Option<AskSender>,
    /// This device's fleet key: the sibling boots answer.
    local: [u8; 32],
    /// The challenges this device issued, the boots it served and the requests waiting for it. One
    /// session answers as one device.
    sibling: Mutex<UnlockSibling>,
    /// Serializes what this device publishes about disk unlock: two boots answered at once must not
    /// overwrite each other's record.
    published: Mutex<()>,
}

impl Unlock {
    /// The sibling side for the device whose fleet key is `local` , asking `questions` where it
    /// needs the person's consent.
    pub(crate) fn new(link: SharedLink, local: [u8; 32], questions: Option<AskSender>) -> Self {
        Self {
            link,
            questions,
            local,
            sibling: Mutex::new(UnlockSibling::new(local)),
            published: Mutex::new(()),
        }
    }

    /// Answer one early-boot connection on [`tilo_unlock_protocol::UNLOCK_ALPN`].
    pub(crate) async fn serve(self: Arc<Self>, connection: Connection) {
        let Ok((mut send, mut recv)) = connection.accept_bi().await else {
            return;
        };
        let Some(ClientFrame::Hello(hello)) = read_client(&mut recv).await else {
            connection.close(0u32.into(), b"not a boot");
            return;
        };
        let Some(ownership) = read_ownership() else {
            tell(send, SiblingFrame::Refused(RefusalReason::NotAKeyHolder)).await;
            return;
        };
        let published = self.published(&ownership);
        let challenge = self.challenge(&hello, &ownership, &published);
        let SiblingFrame::Challenge(issued) = &challenge else {
            tell(send, challenge.clone()).await;
            return;
        };
        let nonce = issued.nonce;
        if !write_sibling(&mut send, &challenge).await {
            return;
        }
        let Some(ClientFrame::Request(request)) = read_client(&mut recv).await else {
            return;
        };
        let now = tilo_clock::unix_now_ns();
        // The owner's answer to this boot, when their devices kept one, is the whole exchange: an
        // approval needs the half, a denial needs nothing loaded.
        let boot_digest = boot_cert_digest(&hello.boot_cert);
        let request_digest = request
            .digest(&hello.boot_cert, &self.local)
            .unwrap_or_default();
        let allowed_before = boot_digest
            .and_then(|digest| {
                decision_for(
                    &published.decisions,
                    &digest,
                    request.boot_id,
                    request.slot,
                    now,
                )
            })
            .is_some_and(|record| record.outcome == DecisionOutcome::Approve);
        let frame = {
            let release = match &boot_digest {
                Some(digest) if allowed_before => {
                    self.release(
                        &ownership,
                        &published,
                        digest,
                        &request_digest,
                        &request.ephemeral_public,
                    )
                    .await
                }
                _ => None,
            };
            let view = view_of(&ownership, &published, release.as_ref());
            self.lock().request(&request, &hello, &nonce, &view, now)
        };
        let SiblingFrame::Pending { approval_code } = &frame else {
            if write_sibling(&mut send, &frame).await {
                if let Err(error) = send.finish() {
                    tracing::warn!(%error, "a boot's pending answer did not reach it");
                }
                let _ = tokio::time::timeout(WRITE_GRACE, send.stopped()).await;
            }
            return;
        };
        let pending = approval_code.clone();
        let digest = request_digest;
        if !write_sibling(&mut send, &frame).await {
            // The boot is gone before it could read even that: nobody will wait for a decision on
            // its behalf.
            self.lock().withdraw(&digest);
            return;
        }
        // The person is asked here, and the sheet shows nothing more of it once they answered or
        // the boot went away.
        let asking = boot_digest
            .and_then(|digest| published.enrolments.get(&digest))
            .map(UnlockEnrolment::device);
        let Some(asking) = asking else {
            self.lock().withdraw(&digest);
            return;
        };
        let asked = self.ask(asking, &pending, &request);
        let Some(answer) = wait_for_person(asked, &send).await else {
            self.lock().withdraw(&digest);
            return;
        };
        let approved = answer.is_some();
        // Released only with the person's consent, and only for a boot still waiting: a decision on
        // a request that went away decides nothing.
        if let Some(decided) = self.decide(&ownership, &published, &digest, approved).await {
            if let Some(record) = decided.record {
                self.keep(record);
            }
            if !write_sibling(&mut send, &decided.frame).await {
                tracing::warn!("a boot was not told the decision on its unlock request");
            }
        }
        if let Err(error) = send.finish() {
            tracing::warn!(%error, "an unlock decision did not reach the boot that asked");
        }
        let _ = tokio::time::timeout(WRITE_GRACE, send.stopped()).await;
    }

    /// Answer one peer-network enrolment: another of the person's devices
    /// holds no root and asks this one to certify its boot key. The person is
    /// asked on this device's sheet first, and only their approval signs
    /// anything.
    pub(crate) async fn serve_enrol(self: Arc<Self>, connection: Connection) {
        let Ok((mut send, mut recv)) = connection.accept_bi().await else {
            return;
        };
        let Some(request) = read_frame::<EnrolRequest>(&mut recv).await else {
            connection.close(0u32.into(), b"not an enrolment");
            return;
        };
        // The connection is this device's proof of who asks: its endpoint key is its fleet key.
        let device = *connection.remote_id().as_bytes();
        let reply = match read_ownership() {
            Some(ownership) if ownership.key_holder => {
                if ownership
                    .fleet
                    .certification(&device, tilo_clock::unix_now_ns())
                    != DeviceCertification::Certified
                {
                    // Nobody is asked for a device that is not the person's.
                    EnrolReply::Refused("this device is not one of the person's".into())
                } else if request.verify().is_err() {
                    EnrolReply::Refused("the enrolment request does not prove its boot key".into())
                } else if self.ask_enrol(&request, device, &connection).await {
                    self.enrol_from(&request, device)
                } else {
                    EnrolReply::Denied
                }
            }
            Some(_) => EnrolReply::NotKeyHolder,
            None => EnrolReply::Refused(
                "this session holds no identity for the person's devices".into(),
            ),
        };
        if let Ok(frame) = encode_frame(&reply)
            && let Err(error) = send.write_all(&frame).await
        {
            tracing::warn!(%error, "an enrolment answer did not reach the device that asked");
        }
        if let Err(error) = send.finish() {
            tracing::warn!(%error, "an enrolment answer did not reach the device that asked");
        }
        let _ = tokio::time::timeout(WRITE_GRACE, send.stopped()).await;
    }

    /// Put another device's enrolment to the person, and wait for their answer.
    /// No answer, or the asking device going away, leaves it unenrolled.
    async fn ask_enrol(
        &self,
        request: &EnrolRequest,
        device: [u8; 32],
        connection: &Connection,
    ) -> bool {
        let Some(questions) = self.questions.as_ref() else {
            return false;
        };
        let label = self.label_of(&device);
        let asked = questions.ask(Question {
            handle: format!("device-enrol:{}", hex::encode(device)),
            kind: ENROL_KIND.to_owned(),
            app_id: None,
            label: label.clone(),
            details: serde_json::to_value(EnrolQuestion {
                device: label,
                unattended: request.unattended_chain.is_some(),
            })
            .unwrap_or_default(),
        });
        let wait = tokio::time::timeout(Duration::from_nanos(APPROVAL_TIMEOUT_NS), async {
            tokio::select! {
                answer = asked => Some(answer),
                _ = connection.closed() => None,
            }
        })
        .await;
        matches!(wait, Ok(Some(Some(_))))
    }

    /// Challenge the boot, or refuse its hello.
    fn challenge(
        &self,
        hello: &UnlockHello,
        ownership: &Ownership,
        published: &MergedUnlock,
    ) -> SiblingFrame {
        let view = view_of(ownership, published, None);
        self.lock().hello(
            hello,
            &view,
            crate::platform::fabric::nonzero::<32>(),
            tilo_clock::unix_now_ns(),
        )
    }

    /// The challenges this device issued and the requests waiting for it.
    fn lock(&self) -> std::sync::MutexGuard<'_, UnlockSibling> {
        self.sibling
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The boot's half, opened by the daemon that keeps the person's root and sealed to the
    /// boot's ephemeral key: nothing here holds the secret-sync key, and the half never appears
    /// in this process.
    async fn release(
        &self,
        ownership: &Ownership,
        published: &MergedUnlock,
        boot_cert_digest: &[u8; 32],
        request_digest: &[u8; 32],
        ephemeral_public: &[u8; 32],
    ) -> Option<tilo_unlock_protocol::Release> {
        if !ownership.key_holder {
            return None;
        }
        let enrolment = published.enrolments.get(boot_cert_digest)?;
        let args = serde_json::json!({
            "cert": hex::encode(postcard::to_allocvec(&enrolment.cert).ok()?),
            "sealed_half": hex::encode(postcard::to_allocvec(&enrolment.sealed_half).ok()?),
            "request_digest": hex::encode(request_digest),
            "ephemeral": hex::encode(ephemeral_public),
        });
        tokio::task::spawn_blocking(move || {
            tilo_api::daemon::this_computer()
                .ok()?
                .act(tilo_device_identity::login::RELEASE_HALF, args)
                .ok()?
                .get("release")?
                .as_str()
                .and_then(|release| hex::decode(release).ok())
                .and_then(|release| postcard::from_bytes::<tilo_unlock_protocol::Release>(&release).ok())
        })
        .await
        .ok()
        .flatten()
    }

    /// Record the person's decision on the request `digest` , for the boot still waiting.
    async fn decide(
        &self,
        ownership: &Ownership,
        published: &MergedUnlock,
        digest: &[u8; 32],
        approved: bool,
    ) -> Option<Decided> {
        let pending = self.lock().pending(digest);
        let release = match pending {
            Some(pending) if approved => {
                self.release(
                    ownership,
                    published,
                    &pending.enrolment,
                    &pending.request_digest,
                    &pending.ephemeral_public,
                )
                .await
            }
            _ => None,
        };
        let decision = if approved {
            UnlockDecision::Approve {
                approver: self.local,
            }
        } else {
            UnlockDecision::Deny {
                approver: self.local,
            }
        };
        let view = view_of(ownership, published, release.as_ref());
        self.lock()
            .decide(digest, decision, &view, tilo_clock::unix_now_ns())
    }

    /// Put the boot's request to the person, with the code its screen shows,
    /// and let dropping the question go with it.
    fn ask(&self, device: [u8; 32], approval_code: &str, request: &UnlockRequest) -> Option<Asked> {
        let questions = self.questions.as_ref()?;
        let handle = format!("device-unlock:{}", hex::encode(request.boot_id.0));
        let device = self.label_of(&device);
        Some(
            questions.ask(Question {
                handle,
                kind: KIND.to_owned(),
                app_id: None,
                label: device.clone(),
                details: serde_json::to_value(UnlockQuestion {
                    approval_code: approval_code.to_owned(),
                    device,
                    slot: slot_name(&request.slot),
                })
                .unwrap_or_default(),
            }),
        )
    }

    /// The name the person's store holds for `device` .
    fn label_of(&self, device: &[u8; 32]) -> String {
        let key = hex::encode(device);
        self.link
            .devices()
            .devices()
            .iter()
            .find(|(id, _)| id.as_str() == key)
            .map_or_else(|| key, |(_, device)| device.about.label.clone())
    }

    /// The enrolments, auto-approvals and answers the person's store holds now, under the fleet
    /// this device trusts.
    fn published(&self, ownership: &Ownership) -> MergedUnlock {
        let fabric = self.link.devices();
        merged_unlock_state(
            fabric.devices().values().map(|device| &device.unlock),
            &ownership.fleet,
            tilo_clock::unix_now_ns(),
        )
    }

    /// Serve `tilo-disk-encrypt enrol --sibling` on this device: certify the boot key, seal its
    /// sibling half, and publish the enrolment to the person's other devices.
    pub(crate) fn enrol(&self, request: &EnrolRequest) -> EnrolReply {
        self.enrol_from(request, self.local)
    }

    /// Enrol `device`'s boot key: the device that asked, which on the local socket is this one.
    fn enrol_from(&self, request: &EnrolRequest, device: [u8; 32]) -> EnrolReply {
        match self.try_enrol(request, device) {
            Ok(response) => EnrolReply::Enrolled(Box::new(response)),
            Err(NotEnrolled::NotKeyHolder) => EnrolReply::NotKeyHolder,
            Err(NotEnrolled::Reason(reason)) => EnrolReply::Refused(reason),
        }
    }

    fn try_enrol(
        &self,
        request: &EnrolRequest,
        enrollee: [u8; 32],
    ) -> Result<EnrolResponse, NotEnrolled> {
        request
            .verify()
            .map_err(|error| NotEnrolled::Reason(format!("enrolment request: {error}")))?;
        let ownership = read_ownership().ok_or_else(|| {
            NotEnrolled::Reason("this session holds no identity for the person's devices".into())
        })?;
        if !ownership.key_holder {
            return Err(NotEnrolled::NotKeyHolder);
        }
        let refused = NotEnrolled::Reason;
        let person = *ownership.fleet.user_public_key();
        // An enrolment for an unattended unlock asks, with the boot chain it measures now, to be
        // released without a prompt: the daemon that holds the root signs that acceptance.
        let mut approval = request.unattended_chain.map(|chain| {
            AutoApproval::unsigned(person, enrollee, vec![chain], tilo_clock::unix_now_ns())
        });
        let approval_digest = match &approval {
            Some(approval) => Some(approval.digest().ok_or_else(|| {
                refused("the unlock policy could not be encoded".into())
            })?),
            None => None,
        };
        let mut args = serde_json::json!({
            "request": hex::encode(
                postcard::to_allocvec(request)
                    .map_err(|error| refused(format!("the enrolment request: {error}")))?,
            ),
            "device": hex::encode(enrollee),
        });
        if let Some(digest) = &approval_digest {
            args["approval_digest"] = serde_json::Value::String(hex::encode(digest));
        }
        let answer = tilo_api::daemon::this_computer()
            .and_then(|daemon| daemon.act(tilo_device_identity::login::ENROL_BOOT_KEY, args))
            .map_err(refused)?;
        let cert: DeviceCert = answer
            .get("cert")
            .and_then(serde_json::Value::as_str)
            .and_then(|cert| hex::decode(cert).ok())
            .and_then(|cert| postcard::from_bytes(&cert).ok())
            .ok_or_else(|| {
                refused("the person's root gave no certificate for the boot key".into())
            })?;
        verify_device_cert(&cert)
            .map_err(|error| refused(format!("the boot key's certificate: {error}")))?;
        let sealed_half: tilo_unlock_protocol::SealedSiblingHalf = answer
            .get("sealed_half")
            .and_then(serde_json::Value::as_str)
            .and_then(|half| hex::decode(half).ok())
            .and_then(|half| postcard::from_bytes(&half).ok())
            .ok_or_else(|| refused("the boot key's half was not sealed".into()))?;
        // Every one of the person's devices holds the half once the enrolment reaches them,
        // whatever is beside this device at this moment: the list is their record, named as it
        // names them.
        let siblings = answer
            .get("siblings")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|sibling| {
                Some(SiblingContact {
                    device_public_key: hex::decode(sibling.get("device")?.as_str()?)
                        .ok()?
                        .try_into()
                        .ok()?,
                    label: sibling
                        .get("label")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    endpoint: String::new(),
                })
            })
            .collect();
        if let Some(approval) = &mut approval {
            approval.signature = answer
                .get("approval_signature")
                .and_then(serde_json::Value::as_str)
                .and_then(|signature| hex::decode(signature).ok())
                .ok_or_else(|| refused("the unlock policy was not signed".into()))?;
            approval.validate().map_err(|error| {
                refused(format!("the unlock policy is not admissible: {error}"))
            })?;
        }
        let holder =
            DeviceId::new(hex::encode(self.local)).map_err(|error| refused(error.to_string()))?;
        let enrolment = UnlockEnrolment {
            cert: cert.clone(),
            device_public_key: enrollee,
            sealed_half,
            slot: request.slot,
            attestation_key: request.attestation_key.clone(),
            stamp: tilo_network_actor::fabric::FabricStamp {
                counter: 0,
                device: holder,
            },
        };
        self.publish(enrolment, approval);
        tracing::info!(
            device = %hex::encode(enrollee),
            slot = %slot_name(&request.slot),
            "device unlock: enrolled",
        );
        Ok(EnrolResponse { cert, siblings })
    }

    /// Add `enrolment`, and the owner's acceptance of the boot chain it asked
    /// for, to this device's own part of the person's store, keeping what else
    /// it published there.
    fn publish(&self, enrolment: UnlockEnrolment, approval: Option<AutoApproval>) {
        self.update_state(|state| {
            state
                .enrolments
                .retain(|current| current.slot != enrolment.slot);
            state.enrolments.push(enrolment);
            if let Some(approval) = approval {
                let device = approval.device_public_key;
                state
                    .auto_approvals
                    .retain(|current| current.device_public_key != device);
                state.auto_approvals.push(approval);
            }
        });
    }

    /// Keep the owner's answer in this device's own part of the person's store, so their other
    /// devices answer the same boot the same way. Answers that no longer hold give way to the
    /// newest ones.
    fn keep(&self, record: UnlockDecisionEntry) {
        let now = tilo_clock::unix_now_ns();
        self.update_state(|state| {
            state
                .decisions
                .retain(|current| current.valid_until_ns > now && current.key() != record.key());
            state.decisions.push(record);
            state.decisions.sort_by_key(|kept| kept.decided_at_ns);
            let over = state.decisions.len().saturating_sub(MAX_UNLOCK_DECISIONS);
            state.decisions.drain(..over);
        });
    }

    /// Change what this device publishes about disk unlock: its own part of the person's store is
    /// the only one it may write.
    fn update_state(&self, change: impl FnOnce(&mut UnlockFabricState)) {
        let _serialized = self
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fabric = self.link.devices();
        let key = hex::encode(self.local);
        let mut state = fabric
            .devices()
            .iter()
            .find(|(id, _)| id.as_str() == key)
            .map(|(_, device)| device.unlock.clone())
            .unwrap_or_default();
        change(&mut state);
        self.link.share_unlock(state);
    }

    /// Serve the session's enrolment socket, one request per connection.
    pub(crate) fn spawn_enrol_socket(self: &Arc<Self>) {
        let runtime = tilo_paths::xdg::runtime_dir_or_default();
        let path = runtime.join(ENROL_SOCKET);
        let service = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("device-unlock-enrol".into())
            .spawn(move || {
                let listener = match crate::mcp_server::bind_private(&path) {
                    Ok(listener) => listener,
                    Err(error) => {
                        tilo_failure::ignored!(
                            "a session without the enrolment socket enrolls nothing",
                            tracing::debug!(%error, "disk-unlock enrolment socket unavailable")
                        );
                        return;
                    }
                };
                for stream in listener.incoming().flatten() {
                    if let Err(error) = stream.set_read_timeout(Some(Duration::from_secs(10))) {
                        // Without it a boot that says nothing holds this connection open for as
                        // long as it likes.
                        tracing::warn!(%error, "an enrolment connection has no read timeout");
                    }
                    let _ = service.answer_enrolment(stream);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start the disk-unlock enrolment socket");
        }
    }

    fn answer_enrolment(&self, mut stream: std::os::unix::net::UnixStream) -> Option<()> {
        let mut prefix = [0_u8; 4];
        stream.read_exact(&mut prefix).ok()?;
        let len = frame_len(prefix).ok()?;
        let mut body = zeroize::Zeroizing::new(vec![0_u8; len]);
        stream.read_exact(&mut body).ok()?;
        let reply = decode_frame::<EnrolRequest>(&body).map_or_else(
            |_| EnrolReply::Refused("malformed enrolment request".into()),
            |request| self.enrol(&request),
        );
        let frame = encode_frame(&reply).ok()?;
        stream.write_all(&frame).ok()
    }
}

/// What the sibling state machine reads: the fleet this device trusts and the person's fabric as it
/// holds it.
fn view_of<'a>(
    ownership: &'a Ownership,
    published: &'a MergedUnlock,
    release: Option<&'a tilo_unlock_protocol::Release>,
) -> UnlockView<'a> {
    UnlockView {
        fleet: &ownership.fleet,
        enrolments: &published.enrolments,
        auto_approvals: &published.auto_approvals,
        decisions: &published.decisions,
        key_holder: ownership.key_holder,
        release,
    }
}

/// Tell the boot why it is refused, and finish the stream.
async fn tell(mut send: SendStream, frame: SiblingFrame) {
    if !write_sibling(&mut send, &frame).await {
        tracing::warn!("a boot was not told why it is refused");
    }
    if let Err(error) = send.finish() {
        tracing::warn!(%error, "a refusal did not reach the boot");
    }
}

/// The person's answer to the question `asked` : their decision, or `None` when the boot stopped
/// waiting before one came.
async fn wait_for_person(
    asked: Option<Asked>,
    send: &SendStream,
) -> Option<Option<tilo_service::Answer>> {
    let Some(asked) = asked else {
        // No sheet to ask on: the boot is refused as if nobody answered.
        return Some(None);
    };
    let wait = tokio::time::timeout(Duration::from_nanos(APPROVAL_TIMEOUT_NS), async {
        tokio::select! {
            answer = asked => Some(answer),
            // The boot stopped waiting: its question is taken back rather than answered for nobody.
            _ = send.stopped() => None,
        }
    })
    .await;
    match wait {
        Ok(answer) => answer,
        // Nobody answered in time, which refuses this boot.
        Err(_) => tilo_failure::ignored!("no answer within the window is a refusal", Some(None)),
    }
}

/// The enrolment a boot's certificate is known by.
fn boot_cert_digest(cert: &DeviceCert) -> Option<[u8; 32]> {
    device_cert_signing_digest(cert).ok()
}

/// The slot as the boot screen names it, and as the person's own record of their answers keeps it.
pub(crate) fn slot_name(slot: &SlotId) -> String {
    format!("{}:{}", hex::encode(slot.volume_uuid), slot.keyslot)
}

/// The person's devices, as the daemon that holds their record lists them, and whether it keeps
/// their root.
fn read_ownership() -> Option<Ownership> {
    use tilo_user_identity::RevocationEntry;
    let answer = tilo_api::daemon::this_computer()
        .ok()?
        .act(
            tilo_device_identity::login::PERSONAL_DEVICES,
            serde_json::json!({}),
        )
        .ok()?;
    let person: [u8; 32] = hex::decode(answer.get("person")?.as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    let mut fleet = CertifiedFleet::new(person);
    for device in answer.get("devices")?.as_array()? {
        let cert = device
            .get("certificate")
            .and_then(serde_json::Value::as_str)
            .and_then(|cert| hex::decode(cert).ok())
            .and_then(|cert| postcard::from_bytes::<DeviceCert>(&cert).ok());
        if let Some(cert) = cert {
            let _ = fleet.certify(cert);
        }
    }
    for entry in answer
        .get("revocations")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let entry = entry
            .as_str()
            .and_then(|entry| hex::decode(entry).ok())
            .and_then(|entry| postcard::from_bytes::<RevocationEntry>(&entry).ok());
        if let Some(entry) = entry {
            let _ = fleet.revoke(entry);
        }
    }
    Some(Ownership {
        fleet,
        key_holder: answer.get("key_holder")?.as_bool()?,
    })
}

async fn read_client(recv: &mut RecvStream) -> Option<ClientFrame> {
    read_frame(recv).await
}

/// Read one length-prefixed frame of `T`, within a boot's answering time.
async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Option<T> {
    let mut prefix = [0_u8; 4];
    tokio::time::timeout(ANSWER_TIMEOUT, recv.read_exact(&mut prefix))
        .await
        .ok()?
        .ok()?;
    let mut body = vec![0_u8; frame_len(prefix).ok()?];
    tokio::time::timeout(ANSWER_TIMEOUT, recv.read_exact(&mut body))
        .await
        .ok()?
        .ok()?;
    // A frame that does not decode is not one this protocol knows: the caller
    // closes the connection.
    tilo_failure::ignored!(
        "a frame this protocol does not know is not a request",
        decode_frame(&body).ok()
    )
}

async fn write_sibling(send: &mut SendStream, frame: &SiblingFrame) -> bool {
    match encode_frame(frame) {
        Ok(bytes) => send.write_all(&bytes).await.is_ok(),
        Err(error) => {
            tracing::warn!(%error, "an unlock answer could not be encoded");
            false
        }
    }
}
