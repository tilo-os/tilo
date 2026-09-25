//! Borrow the closed legacy join DTO before allocating its owned counterpart
//! (the same bounded decode as the native onboarding carrier).
//! The legacy discriminants, field order and exact signed bytes remain unchanged.
use anyhow::{Result, ensure};
use qos_peer_state::join::*;
use qos_user_identity::{DeviceCapabilities, DeviceCert};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub(super) struct Certificate<'a> {
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    #[serde(borrow)]
    device_label: &'a str,
    capabilities: DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
    #[serde(borrow)]
    signature: &'a [u8],
}
impl Certificate<'_> {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.device_label.len() <= 4096 && self.signature.len() == 64,
            "certificate exceeds bound"
        );
        Ok(())
    }
    pub(super) fn heap(&self) -> usize {
        self.device_label.len()
    }
    pub(super) fn into_owned(self) -> DeviceCert {
        DeviceCert {
            user_public_key: self.user_public_key,
            device_public_key: self.device_public_key,
            device_label: self.device_label.to_owned(),
            capabilities: self.capabilities,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
            signature: self.signature.try_into().expect("preflighted signature"),
        }
    }
}
#[derive(Serialize, Deserialize)]
pub(super) enum Request<'a> {
    Inspect {
        #[serde(borrow)]
        signed_invitation: &'a [u8],
    },
    Challenge(#[serde(borrow)] Challenge<'a>),
    Complete(#[serde(borrow)] Complete<'a>),
    MembershipChallenge(#[serde(borrow)] MemberChallenge<'a>),
    MembershipComplete(#[serde(borrow)] MemberComplete<'a>),
}
#[derive(Serialize, Deserialize)]
pub(super) struct Challenge<'a> {
    #[serde(borrow)]
    signed_invitation: &'a [u8],
    #[serde(borrow)]
    device_cert: Certificate<'a>,
    request_id: [u8; 32],
}
#[derive(Serialize, Deserialize)]
pub(super) struct Complete<'a> {
    #[serde(borrow)]
    proof: Proof<'a>,
}
#[derive(Serialize, Deserialize)]
struct Proof<'a> {
    #[serde(borrow)]
    challenge: Join<'a>,
    #[serde(borrow)]
    signature: &'a [u8],
}
#[derive(Serialize, Deserialize)]
struct Join<'a> {
    version: u32,
    #[serde(borrow)]
    signed_invitation: &'a [u8],
    #[serde(borrow)]
    room: &'a str,
    workspace_id: u64,
    authority_device_public_key: [u8; 32],
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    request_id: [u8; 32],
    nonce: [u8; 32],
    expires_at_ns: u64,
}
#[derive(Serialize, Deserialize)]
pub(super) struct MemberChallenge<'a> {
    #[serde(borrow)]
    room: &'a str,
    workspace_id: u64,
    #[serde(borrow)]
    device_cert: Certificate<'a>,
    request_id: [u8; 32],
}
#[derive(Serialize, Deserialize)]
pub(super) struct MemberComplete<'a> {
    #[serde(borrow)]
    proof: MemberProof<'a>,
}
#[derive(Serialize, Deserialize)]
struct MemberProof<'a> {
    #[serde(borrow)]
    challenge: Member<'a>,
    #[serde(borrow)]
    signature: &'a [u8],
}
#[derive(Serialize, Deserialize)]
struct Member<'a> {
    version: u32,
    #[serde(borrow)]
    room: &'a str,
    workspace_id: u64,
    authority_device_public_key: [u8; 32],
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    request_id: [u8; 32],
    nonce: [u8; 32],
    expires_at_ns: u64,
}
struct Exact<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for Exact<'_> {
    type Output = ();
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.try_extend(&[byte])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        if !self.0.starts_with(bytes) {
            return Err(postcard::Error::SerializeBufferFull);
        }
        self.0 = &self.0[bytes.len()..];
        Ok(())
    }
    fn finalize(self) -> postcard::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(postcard::Error::SerializeBufferFull)
        }
    }
}
fn decode<'a, T: Deserialize<'a> + Serialize>(bytes: &'a [u8]) -> Result<T> {
    ensure!(
        bytes.len() <= qos_relay::delivery_v3::onboarding_wire::MAX_REQUEST_BYTES,
        "join input exceeds bound"
    );
    let value: T = postcard::from_bytes(bytes)?;
    postcard::serialize_with_flavor(&value, Exact(bytes))?;
    Ok(value)
}
pub(super) fn certificate(bytes: &[u8]) -> Result<Certificate<'_>> {
    ensure!(bytes.len() <= 4096, "certificate exceeds bound");
    let certificate: Certificate<'_> = decode(bytes)?;
    certificate.validate()?;
    Ok(certificate)
}
impl<'a> Request<'a> {
    pub(super) fn decode(bytes: &'a [u8]) -> Result<Self> {
        let value: Self = decode(bytes)?;
        match &value {
            Self::Inspect { .. } => {}
            Self::Challenge(v) => v.device_cert.validate()?,
            Self::Complete(v) => {
                ensure!(
                    v.proof.signature.len() == 64,
                    "join signature exceeds bound"
                );
                room(v.proof.challenge.room)?;
            }
            Self::MembershipChallenge(v) => {
                room(v.room)?;
                v.device_cert.validate()?;
            }
            Self::MembershipComplete(v) => {
                ensure!(
                    v.proof.signature.len() == 64,
                    "membership signature exceeds bound"
                );
                room(v.proof.challenge.room)?;
            }
        }
        Ok(value)
    }
    pub(super) fn heap(&self) -> usize {
        match self {
            Self::Inspect { signed_invitation } => signed_invitation.len(),
            Self::Challenge(v) => v.signed_invitation.len() + v.device_cert.heap(),
            Self::Complete(v) => {
                v.proof.challenge.signed_invitation.len() + v.proof.challenge.room.len()
            }
            Self::MembershipChallenge(v) => v.room.len() + v.device_cert.heap(),
            Self::MembershipComplete(v) => v.proof.challenge.room.len(),
        }
    }
    pub(super) fn into_owned(self) -> JoinTransportRequest {
        match self {
            Self::Inspect { signed_invitation } => JoinTransportRequest::Inspect {
                signed_invitation: signed_invitation.to_vec(),
            },
            Self::Challenge(v) => JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: v.signed_invitation.to_vec(),
                device_cert: v.device_cert.into_owned(),
                request_id: v.request_id,
            }),
            Self::Complete(v) => {
                let c = v.proof.challenge;
                JoinTransportRequest::Complete(JoinCompleteRequest {
                    proof: JoinProof {
                        challenge: JoinChallenge {
                            version: c.version,
                            signed_invitation: c.signed_invitation.to_vec(),
                            room: c.room.to_owned(),
                            workspace_id: c.workspace_id,
                            authority_device_public_key: c.authority_device_public_key,
                            user_public_key: c.user_public_key,
                            device_public_key: c.device_public_key,
                            request_id: c.request_id,
                            nonce: c.nonce,
                            expires_at_ns: c.expires_at_ns,
                        },
                        signature: v.proof.signature.try_into().expect("preflighted signature"),
                    },
                })
            }
            Self::MembershipChallenge(v) => {
                JoinTransportRequest::MembershipChallenge(MembershipChallengeRequest {
                    room: v.room.to_owned(),
                    workspace_id: v.workspace_id,
                    device_cert: v.device_cert.into_owned(),
                    request_id: v.request_id,
                })
            }
            Self::MembershipComplete(v) => {
                let c = v.proof.challenge;
                JoinTransportRequest::MembershipComplete(MembershipCompleteRequest {
                    proof: MembershipProof {
                        challenge: MembershipChallenge {
                            version: c.version,
                            room: c.room.to_owned(),
                            workspace_id: c.workspace_id,
                            authority_device_public_key: c.authority_device_public_key,
                            user_public_key: c.user_public_key,
                            device_public_key: c.device_public_key,
                            request_id: c.request_id,
                            nonce: c.nonce,
                            expires_at_ns: c.expires_at_ns,
                        },
                        signature: v.proof.signature.try_into().expect("preflighted signature"),
                    },
                })
            }
        }
    }
}
fn room(value: &str) -> Result<()> {
    let mut canonical = [0; 36];
    ensure!(
        value.len() == 36
            && uuid::Uuid::parse_str(value)?
                .hyphenated()
                .encode_lower(&mut canonical)
                == value,
        "noncanonical join room"
    );
    Ok(())
}
