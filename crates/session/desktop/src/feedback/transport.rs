//! Feedback over an existing authenticated session and canonical Chat owner.
//! No constructor enrolls a device, grants a capability or chooses a room.
use std::rc::Rc;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_actions::remote::{Caller, CallerPrincipal};
use tilo_app::feedback::{Error, FeedbackHost, MaskedScreenshot, Receipt, ReviewText, Submission};
use ui::permissions::{CapDecision, PermissionContext, ResourceRef, Verb};
use ui::record::{RecordId, RecordStore};

use super::{Destination, DurableStore, PrivateFeedback, StorageKey};

const DOMAIN: &[u8] = b"tilo-feedback-canonical-receipt-v1\0";
const CHAT_PACKAGE: &str = "so.tilo.chat";

/// Current enrollment, sampled by the existing authenticated transport.
/// This is intentionally not deserializable from a request or launch config.
#[derive(Clone)]
pub struct Enrollment {
    pub destination: Destination,
    pub owner: Caller,
    pub reporter: Caller,
    pub permissions: PermissionContext,
    /// The actual publication requirement. Browser Splice requires Write;
    /// permission to invoke a Comment action does not supply it.
    pub publication: Verb,
}

impl Enrollment {
    fn pin(&self) -> Result<Pin, Error> {
        self.destination.validate()?;
        if self.owner.room() != Some(self.destination.room.as_str())
            || self.reporter.room() != Some(self.destination.room.as_str())
        {
            return Err(Error::ReceiptMismatch);
        }
        let identity = |caller: &Caller| -> Result<Identity, Error> {
            let CallerPrincipal::Person(person) = caller.principal() else {
                return Err(Error::Invalid("certified person required"));
            };
            let user = *caller.user_public_key().ok_or(Error::ReceiptMismatch)?;
            let device = *caller.device_public_key().ok_or(Error::ReceiptMismatch)?;
            if *person != hex::encode(user) {
                return Err(Error::ReceiptMismatch);
            }
            VerifyingKey::from_bytes(&device).map_err(|_| Error::ReceiptMismatch)?;
            Ok(Identity { user, device })
        };
        Ok(Pin {
            destination: self.destination.clone(),
            owner: identity(&self.owner)?,
            reporter: identity(&self.reporter)?,
            publication: self.publication,
        })
    }

    fn admitted_pin(&self) -> Result<(Pin, PermissionContext), Error> {
        let pin = self.pin()?;
        let permissions = self
            .permissions
            .at_current_time()
            .map_err(|_| Error::Invalid("feedback policy time unavailable"))?;
        if self.permissions.role().is_none()
            || self.permissions.user().0 != hex::encode(pin.reporter.user)
            || !matches!(self.publication, Verb::Comment | Verb::Write)
        {
            return Err(Error::Invalid("reporter admission required"));
        }
        Ok((pin, permissions))
    }

    fn authorize_startup(&self) -> Result<Pin, Error> {
        let (pin, permissions) = self.admitted_pin()?;
        let resource = ResourceRef::Action {
            app: CHAT_PACKAGE.into(),
            resource: format!("chat.channel:{}", self.destination.channel),
            action: "submit-feedback".into(),
        };
        if permissions.decide(&resource, Verb::Read) != CapDecision::Allow {
            return Err(Error::Invalid("feedback destination not authorized"));
        }
        Ok(pin)
    }

    fn authorize(&self) -> Result<Pin, Error> {
        let pin = self.authorize_startup()?;
        let (_, permissions) = self.admitted_pin()?;
        let resource = ResourceRef::Action {
            app: CHAT_PACKAGE.into(),
            resource: format!("chat.channel:{}", self.destination.channel),
            action: "submit-feedback".into(),
        };
        for verb in [Verb::Comment, self.publication] {
            if permissions.decide(&resource, verb) != CapDecision::Allow {
                return Err(Error::Invalid("feedback destination not authorized"));
            }
        }
        Ok(pin)
    }

    fn authorize_receipt(&self, submission: &Submission) -> Result<Pin, Error> {
        let (pin, permissions) = self.admitted_pin()?;
        let resource = ResourceRef::Action {
            app: CHAT_PACKAGE.into(),
            resource: format!("chat.feedback:{}", submission.id),
            action: "read-feedback-receipt".into(),
        };
        if permissions.decide(&resource, Verb::Read) != CapDecision::Allow {
            return Err(Error::Invalid("feedback receipt not authorized"));
        }
        Ok(pin)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    user: [u8; 32],
    device: [u8; 32],
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Pin {
    destination: Destination,
    owner: Identity,
    reporter: Identity,
    publication: Verb,
}

/// Wire acknowledgment signed only after the canonical owner's current save.
/// Deserializing this value does not authenticate it; Host verifies its signature.
#[derive(Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedReceipt {
    receipt: Receipt,
    signature: Vec<u8>,
}

impl SignedReceipt {
    /// Decode bounded wire data; authenticity is still checked by Host.
    /// # Errors
    /// Refuses oversized or malformed acknowledgments and non-signatures.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            receipt: Receipt,
            signature: Vec<u8>,
        }
        if bytes.len() > 4096 {
            return Err(Error::Full);
        }
        let wire: Wire = serde_json::from_slice(bytes)?;
        if wire.signature.len() != 64 {
            return Err(Error::ReceiptMismatch);
        }
        Ok(Self {
            receipt: wire.receipt,
            signature: wire.signature,
        })
    }
}

/// Created only by the canonical owner after current durable record validation.
/// Apps cannot manufacture a signing request from a draft or arbitrary bytes.
pub struct ReceiptSigningRequest {
    digest: [u8; 32],
}

impl ReceiptSigningRequest {
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// Purpose-specific access to the owner's existing signing service. Private
/// keys remain in that service; do not export them into the UI or browser.
pub trait ReceiptSigner {
    fn owner(&self) -> &Caller;
    /// # Errors
    /// Refuses unavailable or retired owner signing authority.
    fn sign(&mut self, request: &ReceiptSigningRequest) -> Result<Vec<u8>, Error>;
}

/// The exact retained record and app's pure registered report projection.
/// Projection is never a durability witness; `CanonicalDestination` checks
/// current owner status around it and the signing service.
pub struct CanonicalRecord {
    pub records: RecordStore,
    pub record: RecordId,
    pub project: ReportProjection,
}

pub type ReportProjection =
    fn(&ui::automerge::AutoCommit, &Submission) -> Result<(Receipt, String), Error>;

fn digest(pin: &Pin, receipt: &Receipt) -> Result<blake3::Hash, Error> {
    let mut hash = blake3::Hasher::new();
    hash.update(DOMAIN);
    hash.update(&serde_json::to_vec(&(pin, receipt))?);
    Ok(hash.finalize())
}

/// Existing session IO. Enrollment must come from current verified admission,
/// including revocation, expiry and exact destination policy, never the payload.
/// Enqueue uses the registered submit-feedback action as the certified reporter,
/// its ordinary admitted publication and submission.id as business identity.
pub trait Transport {
    /// # Errors
    /// Refuses missing, disconnected or retired certified admission.
    fn enrollment(&mut self) -> Result<Enrollment, Error>;
    /// # Errors
    /// Preserves normal registered action/publication refusals.
    fn enqueue(&mut self, submission: &Submission) -> Result<(), Error>;
    /// # Errors
    /// Refuses unavailable or invalid canonical owner responses.
    fn receipt(&mut self, submission: &Submission) -> Result<Option<SignedReceipt>, Error>;
}

type EnrollmentRead = Box<dyn FnMut() -> Result<Enrollment, Error>>;
type ActionSend =
    Box<dyn FnMut(&tilo_actions::remote::RemoteActionTarget, &Submission) -> Result<(), Error>>;
type ReceiptRead = Box<dyn FnMut(&Submission) -> Result<Option<SignedReceipt>, Error>>;

/// Bridge to the runtime's existing certified session/action IO. The send
/// callback enqueues normal semantic dispatch, rather than re-entering another
/// component from inside the composer's effect. Receipt IO carries the signed
/// canonical acknowledgment unchanged; the Host authenticates it.
pub struct SessionTransport {
    enrollment: EnrollmentRead,
    send: ActionSend,
    receipt: ReceiptRead,
    pin: Option<Pin>,
}

impl SessionTransport {
    pub fn new(enrollment: EnrollmentRead, send: ActionSend, receipt: ReceiptRead) -> Self {
        Self {
            enrollment,
            send,
            receipt,
            pin: None,
        }
    }
}

impl Transport for SessionTransport {
    fn enrollment(&mut self) -> Result<Enrollment, Error> {
        let enrollment = (self.enrollment)()?;
        let pin = enrollment.authorize_startup()?;
        if self.pin.as_ref().is_some_and(|retained| *retained != pin) {
            return Err(Error::ReceiptMismatch);
        }
        self.pin = Some(pin);
        Ok(enrollment)
    }
    fn enqueue(&mut self, submission: &Submission) -> Result<(), Error> {
        let enrollment = self.enrollment()?;
        enrollment.authorize()?;
        if enrollment.destination != submission.destination {
            return Err(Error::ReceiptMismatch);
        }
        let target = tilo_actions::remote::RemoteActionTarget {
            provider: enrollment.destination.provider,
            address: tilo_actions::ActionAddress::new(
                format!("chat.channel:{}", enrollment.destination.channel),
                "submit-feedback",
            ),
        };
        (self.send)(&target, submission)
    }
    fn receipt(&mut self, submission: &Submission) -> Result<Option<SignedReceipt>, Error> {
        (self.receipt)(submission)
    }
}

/// Existing focused-surface consent/masking and content custody. Raw screenshots
/// or whole-desktop capture must never be implemented by this adapter.
pub trait Capture {
    /// # Errors
    /// Refuses unavailable or protected opt-in context.
    fn context(&mut self) -> Result<Vec<ReviewText>, Error>;
    /// # Errors
    /// Refuses missing consent or protected/unknown/changed geometry.
    fn capture(&mut self) -> Result<MaskedScreenshot, Error>;
    /// # Errors
    /// Refuses masked content that has not durably committed locally.
    fn persist(&mut self, image: &MaskedScreenshot) -> Result<(), Error>;
}

/// Explicit description-only binding until a qualified capture adapter exists.
pub struct NoCapture;
impl Capture for NoCapture {
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        Err(Error::CaptureProtected)
    }
    fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
        Err(Error::CaptureProtected)
    }
    fn persist(&mut self, _: &MaskedScreenshot) -> Result<(), Error> {
        Err(Error::CaptureProtected)
    }
}

/// Production receipt service at the destination owner's existing runtime.
/// The runtime supplies its retained canonical owner, not a browser replica,
/// and its already-certified device key. No report selects these objects.
pub struct CanonicalDestination {
    owner: Caller,
    destination: Destination,
    source: CanonicalRecord,
    signer: Box<dyn ReceiptSigner>,
}

impl CanonicalDestination {
    /// Bind only an existing canonical Chat record and authenticated owner.
    /// # Errors
    /// Refuses missing owner identity, signer mismatch and foreign namespaces.
    pub fn new(
        owner: Caller,
        destination: Destination,
        source: CanonicalRecord,
        signer: Box<dyn ReceiptSigner>,
    ) -> Result<Self, Error> {
        destination.validate()?;
        if owner.room() != Some(destination.room.as_str())
            || owner.user_public_key().is_none()
            || owner.device_public_key().is_none()
            || owner != *signer.owner()
            || source.records.package_and_home() != (CHAT_PACKAGE.into(), destination.room.clone())
        {
            return Err(Error::ReceiptMismatch);
        }
        Ok(Self {
            owner,
            destination,
            source,
            signer,
        })
    }

    /// Current canonical durability and attributed content, then a signed ack.
    /// Must run on the record owner's turn; there is no callback/yield between
    /// current status/read and signing. A local action result is not an ack.
    /// # Errors
    /// Refuses admission, attribution, payload, namespace and durability failures.
    pub fn receipt(
        &mut self,
        submission: &Submission,
        enrollment: &Enrollment,
    ) -> Result<SignedReceipt, Error> {
        submission.validate()?;
        let pin = enrollment.authorize_receipt(submission)?;
        // Saved report metadata does not prove custody of referenced content.
        // Attachment acknowledgments need the owner's durable content witness.
        if submission.feedback.screenshot.is_some() {
            return Err(Error::NotDurable);
        }
        if enrollment.owner != self.owner
            || self.signer.owner() != &self.owner
            || submission.destination != self.destination
            || pin.destination != self.destination
        {
            return Err(Error::ReceiptMismatch);
        }
        let (receipt, author) = self.current_report(submission)?;
        if author != hex::encode(pin.reporter.user) || receipt.digest != submission.digest()? {
            return Err(Error::ReceiptMismatch);
        }
        let request = ReceiptSigningRequest {
            digest: *digest(&pin, &receipt)?.as_bytes(),
        };
        let signature = self.signer.sign(&request)?;
        VerifyingKey::from_bytes(&pin.owner.device)
            .map_err(|_| Error::ReceiptMismatch)?
            .verify_strict(
                request.digest(),
                &Signature::from_slice(&signature).map_err(|_| Error::ReceiptMismatch)?,
            )
            .map_err(|_| Error::ReceiptMismatch)?;
        if self.signer.owner() != &self.owner || enrollment.authorize_receipt(submission)? != pin {
            return Err(Error::ReceiptMismatch);
        }
        // Signing-service callbacks cannot turn an old Saved state into an ack.
        let (current, current_author) = self.current_report(submission)?;
        if current != receipt || current_author != author {
            return Err(Error::ReceiptMismatch);
        }
        Ok(SignedReceipt { receipt, signature })
    }

    fn current_report(&self, submission: &Submission) -> Result<(Receipt, String), Error> {
        if self
            .source
            .records
            .save_status(&self.source.record)
            .map_err(|_| Error::NotDurable)?
            != ui::record::RecordSaveStatus::Saved
        {
            return Err(Error::NotDurable);
        }
        self.source
            .records
            .read(&self.source.record, |doc| {
                let scope = ui::record::room_scope(doc);
                if scope
                    .as_deref()
                    .is_some_and(|room| room != self.destination.room)
                    || (!self.source.record.is_package_primary() && scope.is_none())
                {
                    return Err(Error::ReceiptMismatch);
                }
                (self.source.project)(doc, submission)
            })
            .ok_or(Error::NotDurable)?
    }
}

/// Non-serializable startup custody supplied by the existing enrollment owner.
/// A launch URL or an application can neither construct nor replace its transport.
pub struct Startup {
    enrollment: Enrollment,
    transport: Box<dyn Transport>,
    capture: Box<dyn Capture>,
}

impl Startup {
    pub fn reporter(&self) -> &Caller {
        &self.enrollment.reporter
    }

    /// Validate the supplied enrollment against the existing session.
    /// # Errors
    /// Refuses absent authority or a mismatched current route.
    pub fn new(
        enrollment: Enrollment,
        mut transport: Box<dyn Transport>,
        capture: Box<dyn Capture>,
    ) -> Result<Self, Error> {
        let pin = enrollment.authorize_startup()?;
        if transport.enrollment()?.authorize_startup()? != pin {
            return Err(Error::ReceiptMismatch);
        }
        Ok(Self {
            enrollment,
            transport,
            capture,
        })
    }

    /// Install into the desktop's actual profile slot; replacement is refused.
    /// # Errors
    /// Refuses replacement, changed authority and undurable/unrecoverable storage.
    pub fn install(
        self,
        store: Rc<dyn DurableStore>,
        slot: &mut Option<PrivateFeedback>,
    ) -> Result<(), Error> {
        if slot.is_some() {
            return Err(Error::Conflict);
        }
        let destination = self.enrollment.destination.clone();
        let host = Host::open(store.clone(), self)?;
        *slot = Some(PrivateFeedback::new(store, destination, Box::new(host))?);
        Ok(())
    }
}

/// Authenticated send adapter; the profile pin survives reconnect and restart.
struct Host {
    pin: Pin,
    transport: Box<dyn Transport>,
    capture: Box<dyn Capture>,
    store: Rc<dyn DurableStore>,
    key: StorageKey,
    revision: Option<durable_store::Revision>,
    _lease: durable_store::Lease,
}

impl Host {
    fn open(store: Rc<dyn DurableStore>, mut startup: Startup) -> Result<Self, Error> {
        use durable_store::WriteOp;
        let pin = startup.enrollment.authorize_startup()?;
        if startup.transport.enrollment()?.authorize_startup()? != pin {
            return Err(Error::ReceiptMismatch);
        }
        let namespace = blake3::hash(&serde_json::to_vec(&pin.destination)?);
        let key = StorageKey::package(format!("so.tilo.feedback-authority.{}", namespace.to_hex()));
        let lease = store.lease(&key)?;
        store.flush(Some(&key))?;
        let revision = match store.read(&key)? {
            Some(stored) => {
                if stored.bytes.len() > 4096 {
                    return Err(Error::Full);
                }
                if serde_json::from_slice::<Pin>(&stored.bytes)? != pin {
                    return Err(Error::ReceiptMismatch);
                }
                None
            }
            None => Some(store.write(&key, WriteOp::Replace(serde_json::to_vec(&pin)?), None)?),
        };
        Ok(Self {
            pin,
            transport: startup.transport,
            capture: startup.capture,
            store,
            key,
            revision,
            _lease: lease,
        })
    }

    fn current(&mut self, submission: Option<&Submission>, receipt: bool) -> Result<(), Error> {
        if let Some(revision) = self.revision
            && self.store.durability(&self.key, revision) != durable_store::Durability::Durable
        {
            return Err(Error::NotDurable);
        }
        let enrollment = self.transport.enrollment()?;
        let current = if receipt {
            enrollment.authorize_receipt(submission.ok_or(Error::ReceiptMismatch)?)?
        } else if submission.is_some() {
            enrollment.authorize()?
        } else {
            enrollment.authorize_startup()?
        };
        if current != self.pin
            || submission.is_some_and(|submission| submission.destination != self.pin.destination)
        {
            return Err(Error::ReceiptMismatch);
        }
        Ok(())
    }
}

impl FeedbackHost for Host {
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        self.current(None, false)?;
        let context = self.capture.context()?;
        self.current(None, false)?;
        Ok(context)
    }
    fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
        self.current(None, false)?;
        let image = self.capture.capture()?;
        self.current(None, false)?;
        Ok(image)
    }
    fn persist_screenshot(&mut self, image: &MaskedScreenshot) -> Result<(), Error> {
        self.current(None, false)?;
        self.capture.persist(image)?;
        self.current(None, false)
    }
    fn send(&mut self, submission: &Submission) -> Result<(), Error> {
        submission.validate()?;
        self.current(Some(submission), false)?;
        self.transport.enqueue(submission)?;
        self.current(Some(submission), false)
    }
    fn receipt(&mut self, submission: &Submission) -> Result<Option<Receipt>, Error> {
        self.current(Some(submission), true)?;
        let Some(signed) = self.transport.receipt(submission)? else {
            return Ok(None);
        };
        self.current(Some(submission), true)?;
        let signature =
            Signature::from_slice(&signed.signature).map_err(|_| Error::ReceiptMismatch)?;
        VerifyingKey::from_bytes(&self.pin.owner.device)
            .map_err(|_| Error::ReceiptMismatch)?
            .verify_strict(digest(&self.pin, &signed.receipt)?.as_bytes(), &signature)
            .map_err(|_| Error::ReceiptMismatch)?;
        if signed.receipt.id != submission.id
            || signed.receipt.destination != self.pin.destination
            || signed.receipt.digest != submission.digest()?
        {
            return Err(Error::ReceiptMismatch);
        }
        Ok(Some(signed.receipt))
    }
}

#[cfg(test)]
mod tests;
