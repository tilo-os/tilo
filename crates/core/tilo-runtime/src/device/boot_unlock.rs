//! Enrolling this device's boot key for disk unlock
//! ([`SET_UP_UNLOCK`](tilo_device_identity::login::SET_UP_UNLOCK)).
//!
//! `tilo-auth sibling enrol` hands the daemon the boot key's request and the sibling half. The
//! person's root certifies the boot key through the device's one dispatch, as the person's
//! `certifyKey` ([`CERTIFY_KEY`]): where this device keeps their root it runs here, and otherwise
//! it waits as its asked record for a device of theirs that keeps it, where the person sees which
//! computer asks and for which key, and answers. Their yes answers the certificate and their
//! secret-sync public key; the daemon seals the half to that key here, so the half never leaves
//! this device in the clear, and publishes the enrolment in this device's own entry of their
//! devices, which every device of theirs carries and answers a boot from.
//!
//! Their no ends the enrolment declined, the disk as locked as it was ([`UNLOCK_DECLINED`]). A
//! device without the root that reaches none of theirs that keeps it is told so before anything
//! waits ([`NO_ROOT_DEVICE`]): nobody could answer.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::remote::asked::{AskedBy, AskedFacts, DECLINED, Fact, Named};
use tilo_actions::remote::invocation_record::Invocations;
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_device_identity::login::{
    CERTIFY_KEY, NO_ROOT_DEVICE, PROVIDER, RESOURCE, UNLOCK_DECLINED,
};
use tilo_unlock_protocol::published::{AutoApproval, UnlockEnrolment, UnlockFabricState};
use tilo_unlock_protocol::{
    APPROVAL_TIMEOUT_NS, EnrolRequest, EnrolResponse, HALF_LEN, Half, SiblingContact,
    seal_sibling_half,
};
use tilo_user_identity::{DeviceCert, verify_device_cert};
use tilo_workspace_service::workspaces::person_workspace::devices;

use super::asked::DeviceAsked;
use super::key_use::Refusal;
use super::ticket::Tickets;
use super::workspaces::ServiceWorkspaces;

/// The fact a device publishes about disk unlock under, in its own entry of the person's devices.
pub(crate) const UNLOCK_FACT: &str = "unlock";

/// What enrols a boot here: the device's certificate, its dispatch of its person's device
/// actions, and the workspaces it publishes the enrolment in. Installed once the person's services
/// run.
pub(crate) struct BootUnlock {
    pub(crate) device: DeviceCert,
    pub(crate) tickets: Arc<Tickets>,
    pub(crate) invocations: Arc<Invocations>,
    pub(crate) asked: Arc<DeviceAsked>,
    pub(crate) workspaces: Arc<ServiceWorkspaces>,
}

impl BootUnlock {
    /// Enrol the boot `args` names: answers `enrolled`, the enrolment response
    /// as postcard in hex.
    ///
    /// # Errors
    /// The arguments do not read, the person declined, none of their devices that keeps the root
    /// could be reached, or the certificate or the publishing failed.
    pub(crate) async fn enrol(&self, args: &Value) -> Result<Value, Refusal> {
        let request: EnrolRequest = hex_field(args, "request")
            .and_then(|bytes| postcard::from_bytes(&bytes).ok())
            .ok_or_else(|| malformed("the request is no enrolment request"))?;
        request
            .verify()
            .map_err(|error| malformed(format!("the enrolment request: {error}")))?;
        let half = hex_field(args, "half")
            .and_then(|bytes| <[u8; HALF_LEN]>::try_from(bytes.as_slice()).ok())
            .map(Half::from_bytes)
            .ok_or_else(|| malformed("the sibling half is 32 bytes in hex"))?;
        let cert = self.device.clone();
        let (person, this) = (cert.user_public_key, cert.device_public_key);
        let record = devices::of(self.workspaces.copies(), &person).await;
        let computer = record
            .listed(&person)
            .into_iter()
            .find(|listed| listed.cert.device_public_key == this)
            .map_or_else(
                || cert.device_label.clone(),
                |listed| listed.device.called(&listed.cert),
            );
        if !super::services::holds_root_for_person(person) {
            self.reach_a_root_device(&record, &person, this).await?;
        }
        let approval = request.unattended_chain.map(|chain| {
            AutoApproval::unsigned(person, this, vec![chain], tilo_clock::unix_now_ns())
        });
        let mut certify = serde_json::Map::new();
        certify.insert(
            "key".to_owned(),
            json!(hex::encode(request.boot_public_key)),
        );
        certify.insert("for".to_owned(), json!("boot"));
        certify.insert(
            "request".to_owned(),
            json!(hex::encode(
                postcard::to_allocvec(&request).map_err(|error| failed(error.to_string()))?
            )),
        );
        certify.insert("device".to_owned(), json!(hex::encode(this)));
        certify.insert("computer".to_owned(), json!(computer));
        if let Some(digest) = approval.as_ref().map(AutoApproval::digest) {
            let digest = digest.ok_or_else(|| failed("the unlock policy could not be encoded"))?;
            certify.insert(
                "approval_digest".to_owned(),
                Value::String(hex::encode(digest)),
            );
        }
        let certify = Value::Object(certify);
        let answer = self.certify(&cert, certify).await?;
        let enrolled = certified_unlock(&answer, &request, &half, this, approval)?;
        self.publish(&cert, &enrolled).await?;
        let response = EnrolResponse {
            cert: enrolled.enrolment.cert.clone(),
            siblings: enrolled.siblings,
        };
        tracing::info!(device = %hex::encode(this), "device unlock: enrolled");
        Ok(json!({
            "enrolled": hex::encode(postcard::to_allocvec(&response).map_err(|error| failed(error.to_string()))?),
        }))
    }

    /// Whether a device of the person's that keeps their root answers this one now: one is
    /// needed to ask them, and a request nobody can answer is refused before it waits.
    async fn reach_a_root_device(
        &self,
        record: &devices::Devices,
        person: &[u8; 32],
        this: [u8; 32],
    ) -> Result<(), Refusal> {
        for device in record.root_holders(person) {
            if device == this {
                continue;
            }
            match self.tickets.reach(device).await {
                Ok(()) => return Ok(()),
                Err(why) => tracing::info!(
                    device = %hex::encode(device),
                    %why,
                    "device unlock: a device that keeps the person's root did not answer"
                ),
            }
        }
        Err(Refusal {
            code: NO_ROOT_DEVICE,
            message: crate::i18n::t("boot-unlock-no-root-device"),
        })
    }

    /// The person's `certifyKey` with `args`, through this device's one dispatch as the person on
    /// this device: run here under their root, or asked of a device of theirs that keeps it.
    /// Followed until it ends, at most as long as a person is given to answer.
    async fn certify(&self, cert: &DeviceCert, args: Value) -> Result<Value, Refusal> {
        let Value::Object(arguments) = args else {
            return Err(failed("the certificate's request is no object"));
        };
        let caller = Caller::authenticated(cert.user_public_key, cert.device_public_key);
        let invocation = InvocationId::new();
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(RESOURCE, CERTIFY_KEY),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let workspace = tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key);
        let receipt = super::services::device_action(
            (&self.invocations, Some(&self.asked)),
            &self.tickets,
            &caller,
            request,
            &workspace,
        )
        .await
        .ok_or_else(|| failed("the person's certificate was not answered"))?
        .map_err(|error| failed(error.to_string()))?;
        let ended = match receipt.status {
            ReceiptStatus::Completed { output } => return Ok(output.value.unwrap_or(Value::Null)),
            ReceiptStatus::Failed { error } => return Err(failed(error.message)),
            ReceiptStatus::Rejected { error } => return Err(failed(error.to_string())),
            ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
                invocation,
                message,
            },
            ReceiptStatus::Accepted => self.followed(&caller, invocation).await?,
        };
        match ended {
            InvocationUpdate::Succeeded { output, .. } => Ok(output.value.unwrap_or(Value::Null)),
            InvocationUpdate::Cancelled { message, .. } if message.as_deref() == Some(DECLINED) => {
                Err(Refusal {
                    code: UNLOCK_DECLINED,
                    message: crate::i18n::t("boot-unlock-declined"),
                })
            }
            InvocationUpdate::Cancelled { message, .. } => {
                Err(failed(message.unwrap_or_else(|| {
                    "the certificate was stopped".to_owned()
                })))
            }
            InvocationUpdate::Failed { error, .. } => Err(failed(error.message)),
            other => Err(failed(format!("the certificate ended as {other:?}"))),
        }
    }

    /// How the call `invocation` ended, followed where the device records it.
    async fn followed(
        &self,
        caller: &Caller,
        invocation: InvocationId,
    ) -> Result<InvocationUpdate, Refusal> {
        let bound =
            Duration::from_nanos(APPROVAL_TIMEOUT_NS).saturating_add(Duration::from_secs(60));
        let deadline = tokio::time::Instant::now()
            .checked_add(bound)
            .ok_or_else(|| failed("the wait for the certificate's answer is out of range"))?;
        loop {
            let state = self
                .invocations
                .observe(caller, &invocation)
                .ok_or_else(|| failed("the person's certificate is no longer followed here"))?;
            if state.update.is_terminal() {
                return Ok(state.update);
            }
            let Some(change) = self.invocations.next_change(caller, &invocation) else {
                return Err(failed(
                    "the person's certificate is no longer followed here",
                ));
            };
            if tokio::time::timeout_at(deadline, change).await.is_err() {
                // Nobody answered in the time a person has: nothing waits for them any more.
                let _ = tilo_actions::remote::ActionProvider::cancel(
                    self.asked.as_ref(),
                    caller,
                    &invocation,
                )
                .await;
                return Err(Refusal {
                    code: UNLOCK_DECLINED,
                    message: crate::i18n::t("boot-unlock-unanswered"),
                });
            }
        }
    }

    /// Publish `enrolled` in this device's own entry of the person's devices, keeping what else
    /// it published about disk unlock there.
    async fn publish(&self, cert: &DeviceCert, enrolled: &Enrolled) -> Result<(), Refusal> {
        let person = cert.user_public_key;
        let key = hex::encode(cert.device_public_key);
        let record = devices::of(self.workspaces.copies(), &person).await;
        let mut state = record
            .devices
            .get(&tilo_resource::Id::from_raw(key))
            .and_then(|device| device.facts.get(UNLOCK_FACT))
            .and_then(|value| hex::decode(value).ok())
            .and_then(|bytes| postcard::from_bytes::<UnlockFabricState>(&bytes).ok())
            .unwrap_or_default();
        let enrolment = enrolled.enrolment.clone();
        state
            .enrolments
            .retain(|current| current.slot != enrolment.slot);
        state.enrolments.push(enrolment);
        if let Some(approval) = &enrolled.approval {
            state
                .auto_approvals
                .retain(|current| current.device_public_key != approval.device_public_key);
            state.auto_approvals.push(approval.clone());
        }
        // The answers to boots are the record's own, never part of the fact.
        state.decisions.clear();
        state
            .validate(&cert.device_public_key)
            .map_err(|error| failed(format!("the enrolment: {error}")))?;
        let value =
            hex::encode(postcard::to_allocvec(&state).map_err(|error| failed(error.to_string()))?);
        devices::keep_own_fact(self.workspaces.copies(), cert, UNLOCK_FACT, &value)
            .await
            .map_err(|error| failed(format!("the enrolment is not kept: {error}")))
    }
}

/// What the person is asked for a boot key's certificate `request` asks for: whether the
/// computer that asks may unlock its disk with their devices, which computer it is, the key as a
/// short fingerprint, and that it then starts unasked where it asked for that. `None` for any
/// other call.
pub(crate) fn asked_facts(request: &InvokeRequest) -> Option<AskedFacts> {
    if request.target.address.action != CERTIFY_KEY {
        return None;
    }
    let InvokeArgument::Json(args) = &request.argument else {
        return None;
    };
    if args.get("for").and_then(Value::as_str) != Some("boot") {
        return None;
    }
    let computer = args.get("computer").and_then(Value::as_str)?.to_owned();
    let device = args.get("device").and_then(Value::as_str)?.to_owned();
    let key = args.get("key").and_then(Value::as_str)?;
    let fingerprint = key
        .as_bytes()
        .chunks(4)
        .take(4)
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .collect::<Vec<_>>()
        .join(" ");
    let unattended = args.contains_key("approval_digest");
    let line = |value: String| Fact::new(String::new(), value);
    let mut facts = vec![
        line(crate::i18n::t("boot-unlock-detail")),
        Fact::new(crate::i18n::t("boot-unlock-computer"), computer.clone()),
        Fact::new(crate::i18n::t("boot-unlock-key"), fingerprint),
    ];
    if unattended {
        facts.push(line(crate::i18n::t("boot-unlock-unattended")));
    }
    Some(AskedFacts {
        by: AskedBy::Device(Named {
            id: device,
            name: computer.clone(),
        }),
        words: crate::i18n::t_args(
            "boot-unlock-asked",
            &[("computer", computer.as_str().into())],
        ),
        facts,
        destructive: unattended,
        ..AskedFacts::default()
    })
}

/// A boot key certified, its half sealed, and what is published of it.
struct Enrolled {
    enrolment: UnlockEnrolment,
    approval: Option<AutoApproval>,
    siblings: Vec<SiblingContact>,
}

/// What the root's `answer` certifies for `request`: the certificate, the half sealed to the
/// secret-sync key it names, the person's devices that carry it, and the signed acceptance of the
/// boot chain where one was asked for.
fn certified_unlock(
    answer: &Value,
    request: &EnrolRequest,
    half: &Half,
    this: [u8; 32],
    approval: Option<AutoApproval>,
) -> Result<Enrolled, Refusal> {
    let cert: DeviceCert = hex_field(answer, "cert")
        .and_then(|bytes| tilo_user_identity::canonical::from_canonical_bytes(&bytes).ok())
        .ok_or_else(|| failed("the person's root gave no certificate for the boot key"))?;
    verify_device_cert(&cert)
        .map_err(|error| failed(format!("the boot key's certificate: {error}")))?;
    if cert.device_public_key != request.boot_public_key {
        return Err(failed("the certificate names another key than the boot's"));
    }
    let secret_sync = hex_field(answer, "secret_sync")
        .and_then(|bytes| tilo_seal::PublicKey::from_slice(&bytes).ok())
        .ok_or_else(|| failed("the person's root named no key to seal the half to"))?;
    let sealed_half = seal_sibling_half(&secret_sync, &cert, half)
        .map_err(|error| failed(format!("the half could not be sealed: {error}")))?;
    let approval = match approval {
        Some(mut approval) => {
            approval.signature = hex_field(answer, "approval_signature")
                .ok_or_else(|| failed("the unlock policy was not signed"))?;
            approval
                .validate()
                .map_err(|error| failed(format!("the unlock policy: {error}")))?;
            Some(approval)
        }
        None => None,
    };
    let siblings = answer
        .get("siblings")
        .and_then(Value::as_array)
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
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                endpoint: String::new(),
            })
        })
        .collect();
    Ok(Enrolled {
        enrolment: UnlockEnrolment {
            cert,
            device_public_key: this,
            sealed_half,
            slot: request.slot,
            attestation_key: request.attestation_key.clone(),
            kept_by: this,
        },
        approval,
        siblings,
    })
}

/// The bytes of the hex field `name` of `value`.
fn hex_field(value: &Value, name: &str) -> Option<Vec<u8>> {
    value
        .get(name)
        .and_then(Value::as_str)
        .and_then(|text| hex::decode(text).ok())
}

fn malformed(message: impl Into<String>) -> Refusal {
    Refusal {
        code: tilo_device_identity::login::MALFORMED_ASK,
        message: message.into(),
    }
}

fn failed(message: impl Into<String>) -> Refusal {
    Refusal {
        code: "device-action",
        message: message.into(),
    }
}
