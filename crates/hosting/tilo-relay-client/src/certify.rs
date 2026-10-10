//! Client side of relay-mediated device certification
//! ([`tilo_relay_protocol::certify`]).
//!
//! [`request_certification`] is what a device without a certificate runs:
//! it presents its self-signed request and waits for the host to hand over
//! a root-signed certificate. [`Approver`] is what a root-holding device runs
//! to list and answer pending requests.

use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream};
use iroh_base::EndpointAddr;
use tilo_relay_protocol::auth::{self, Hello, HelloAck};
use tilo_relay_protocol::certify::{
    ApproverDecision, ApproverReply, ApproverRequest, CERTIFY_ALPN, CertifyOpen, CertifyReply,
    MAX_CERTIFY_FRAME_BYTES, PendingAgent, PendingCertification,
};
use tilo_user_identity::{
    CertificationPolicy, CertificationRequest, DeviceCert, RevocationList, verify_device_cert_at,
    verify_issued_cert,
};

use crate::frame::{read_frame, write_frame};
use crate::signer::DeviceSigner;
use crate::signer::signed_ack2;

/// Why a certification or approver session failed.
#[derive(Debug, thiserror::Error)]
pub enum CertifyError {
    /// Iroh dial / bi-stream.
    #[error("iroh: {0}")]
    Iroh(String),
    /// Postcard encode/decode.
    #[error("codec: {0}")]
    Codec(String),
    /// Auth check failed.
    #[error("auth: {0}")]
    Auth(#[from] tilo_relay_protocol::auth::AuthError),
    /// I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The device signer refused or failed.
    #[error("signer: {0}")]
    Sign(String),
}

/// How a certification request ended.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "returned once per certification, never kept in bulk"
)]
pub enum CertificationOutcome {
    /// A checked, root-signed certificate for the requesting device.
    Issued {
        /// The device's certificate.
        cert: DeviceCert,
        /// The host's certificate, to pin for later sessions.
        host_cert: DeviceCert,
    },
    /// An approver declined, or the host refused.
    Denied(String),
    /// No approver answered in time.
    Expired,
}

fn codec(error: &impl ToString) -> CertifyError {
    CertifyError::Codec(error.to_string())
}

fn iroh(error: &impl ToString) -> CertifyError {
    CertifyError::Iroh(error.to_string())
}

async fn read<T: serde::de::DeserializeOwned>(recv: &mut RecvStream) -> Result<T, CertifyError> {
    postcard::from_bytes(&read_frame(recv, MAX_CERTIFY_FRAME_BYTES).await?)
        .map_err(|error| codec(&error))
}

async fn write<T: serde::Serialize + Sync>(
    send: &mut SendStream,
    value: &T,
) -> Result<(), CertifyError> {
    write_frame(
        send,
        &postcard::to_allocvec(value).map_err(|error| codec(&error))?,
    )
    .await?;
    Ok(())
}

/// Ask the host at `host` to certify the device that signed `request`, and
/// wait for the answer. `on_pending` receives the pairing code once the host
/// has queued the request; show it so the person approving can compare.
///
/// The certificate is checked here against the exact request and the
/// browser [`CertificationPolicy`], and the host certificate must be a valid
/// host certificate of the same user: a host cannot hand back a certificate
/// for another key, another user or wider capabilities.
///
/// # Errors
///
/// Returns [`CertifyError`] when dialing or framing fails, or when the
/// returned certificates do not verify.
pub async fn request_certification(
    endpoint: &Endpoint,
    host: EndpointAddr,
    request: CertificationRequest,
    on_pending: impl FnOnce(&str),
) -> Result<CertificationOutcome, CertifyError> {
    let connection = endpoint
        .connect(host, CERTIFY_ALPN)
        .await
        .map_err(|error| iroh(&error))?;
    let (mut send, mut recv) = connection.open_bi().await.map_err(|error| iroh(&error))?;
    write(&mut send, &CertifyOpen::Request(request.clone())).await?;
    let mut reply: CertifyReply = read(&mut recv).await?;
    if let CertifyReply::Pending { pairing_code } = &reply {
        on_pending(pairing_code);
        reply = read(&mut recv).await?;
    }
    connection.close(0u32.into(), b"");
    match reply {
        CertifyReply::Issued { cert, host_cert } => {
            let now = tilo_clock::unix_now_ns();
            verify_issued_cert(
                &request,
                &cert,
                &CertificationPolicy::browser(),
                &RevocationList::new(),
                now,
            )
            .map_err(|error| codec(&format!("issued certificate refused: {error}")))?;
            verify_device_cert_at(&host_cert, now)
                .map_err(|error| codec(&format!("host certificate refused: {error}")))?;
            if host_cert.user_public_key != request.user_public_key
                || !host_cert.capabilities.can_act_as_host
            {
                return Err(codec(&"host certificate is not a host of this user"));
            }
            Ok(CertificationOutcome::Issued { cert, host_cert })
        }
        CertifyReply::Denied { reason } => Ok(CertificationOutcome::Denied(reason)),
        CertifyReply::Expired => Ok(CertificationOutcome::Expired),
        CertifyReply::Pending { .. } => Err(codec(&"host sent a second pending reply")),
    }
}

/// An authenticated approver session with a host.
pub struct Approver {
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

impl Approver {
    /// Open an approver session. The host must chain to `own_cert`'s user
    /// root; the host refuses devices without `can_admit_devices`.
    ///
    /// # Errors
    ///
    /// Returns [`CertifyError`] when dialing, framing or mutual
    /// authentication fails.
    pub async fn connect(
        endpoint: &Endpoint,
        host: EndpointAddr,
        own_cert: DeviceCert,
        signer: &dyn DeviceSigner,
        revocations: &RevocationList,
    ) -> Result<Self, CertifyError> {
        let user = own_cert.user_public_key;
        let connection = endpoint
            .connect(host, CERTIFY_ALPN)
            .await
            .map_err(|error| iroh(&error))?;
        let (mut send, mut recv) = connection.open_bi().await.map_err(|error| iroh(&error))?;
        let mut client_nonce = [0u8; 32];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut client_nonce);
        write(
            &mut send,
            &CertifyOpen::Approver(Hello {
                cert: own_cert,
                client_nonce,
            }),
        )
        .await?;
        let ack: HelloAck = read(&mut recv).await?;
        auth::verify_hello_ack(
            &ack,
            &user,
            revocations,
            tilo_clock::unix_now_ns(),
            &client_nonce,
        )?;
        write(
            &mut send,
            &signed_ack2(signer, &ack.server_nonce)
                .await
                .map_err(CertifyError::Sign)?,
        )
        .await?;
        Ok(Self {
            connection,
            send,
            recv,
        })
    }

    async fn call(&mut self, request: &ApproverRequest) -> Result<ApproverReply, CertifyError> {
        write(&mut self.send, request).await?;
        read(&mut self.recv).await
    }

    /// Every request currently waiting for an approver.
    ///
    /// # Errors
    ///
    /// Returns [`CertifyError`] when the session fails.
    pub async fn list_pending(&mut self) -> Result<Vec<PendingCertification>, CertifyError> {
        match self.call(&ApproverRequest::ListPending).await? {
            ApproverReply::Pending(pending) => Ok(pending),
            ApproverReply::Refused { reason } => Err(codec(&reason)),
            ApproverReply::Decided | ApproverReply::AgentProposals(_) => {
                Err(codec(&"unexpected approver reply"))
            }
        }
    }

    /// What every pending agent request proposes: shown beside the request
    /// and approved with [`ApproverDecision::IssueAgent`].
    ///
    /// # Errors
    ///
    /// Returns [`CertifyError`] when the session fails.
    pub async fn list_agent_proposals(&mut self) -> Result<Vec<PendingAgent>, CertifyError> {
        match self.call(&ApproverRequest::AgentProposals).await? {
            ApproverReply::AgentProposals(pending) => Ok(pending),
            ApproverReply::Refused { reason } => Err(codec(&reason)),
            ApproverReply::Decided | ApproverReply::Pending(_) => {
                Err(codec(&"unexpected approver reply"))
            }
        }
    }

    /// Answer one request. `Ok(Refused)` means the host did not accept the
    /// decision (an out-of-policy certificate, or the requester left).
    ///
    /// # Errors
    ///
    /// Returns [`CertifyError`] when the session fails.
    pub async fn decide(
        &mut self,
        request_digest: [u8; 32],
        decision: ApproverDecision,
    ) -> Result<ApproverReply, CertifyError> {
        self.call(&ApproverRequest::Decide {
            request_digest,
            decision,
        })
        .await
    }

    /// End the session.
    pub fn close(mut self) {
        let _ = self.send.finish();
        self.connection.close(0u32.into(), b"approver done");
    }
}
