//! Receipts from the relay's retained source, with projections confined in the
//! existing app worker. This never invokes document writes on another person's
//! behalf or enrolls a caller in a channel.
use std::sync::Arc;

use ed25519_dalek::Signer as _;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::feedback::{FeedbackAdmission, FeedbackAnswer, FeedbackQuery};
use tilo_actions::remote::{ActionHostError, Caller};
use tilo_desktop::feedback::Destination;
use tilo_desktop::feedback::transport::{
    CanonicalDestination, CanonicalReceiptSource, Enrollment, ReceiptSigner, ReceiptSigningRequest,
};
use ui::feedback::{Error, Receipt, Submission};

use super::{AppCatalogue, DocumentRoute, RoomMember, RoomRecordsLink, reads, record_of};

fn unavailable(reason: &(impl ToString + ?Sized)) -> ActionHostError {
    ActionHostError::Unavailable {
        reason: reason.to_string(),
    }
}

pub(super) fn answer(
    link: &Arc<dyn RoomRecordsLink>,
    catalogue: AppCatalogue,
    workspace: u64,
    caller: Caller,
    mut query: FeedbackQuery,
) -> Result<FeedbackAnswer, ActionHostError> {
    let destination = Destination {
        room: query.destination.room.clone(),
        provider: query.destination.provider.clone(),
        channel: query.destination.channel.clone(),
    };
    destination
        .validate()
        .map_err(|error| unavailable(&error))?;
    let route = record_of(&destination.provider, &destination.room)
        .filter(|route| {
            route.app_id == "so.tilo.chat"
                && route.record.is_empty()
                && route.workspace_id().ok() == Some(workspace)
        })
        .ok_or_else(|| unavailable("feedback source mismatch"))?;
    if caller.room().is_some_and(|room| room != destination.room) {
        return Err(unavailable("feedback caller room mismatch"));
    }
    let caller = caller.in_room(destination.room.clone());
    let policy = link
        .view_read_policy(workspace, &caller, &route.app_id, &route.record)
        .filter(|policy| policy.room == route.cluster_id)
        .ok_or_else(|| unavailable("certified feedback admission unavailable"))?;
    let member = link.member();
    let cert = &member.endpoint.host_cert;
    if cert.user_public_key != link.user_public_key()
        || cert.device_public_key != member.key.verifying_key().to_bytes()
    {
        return Err(unavailable("feedback owner identity mismatch"));
    }
    let owner = Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_room(destination.room.clone());
    let enrollment = Enrollment {
        destination: destination.clone(),
        owner: owner.clone(),
        reporter: caller.clone(),
        permissions: policy.permissions(),
    };
    let source = RetainedSource {
        link: link.clone(),
        catalogue,
        workspace,
        caller: caller.clone(),
        route,
    };
    // Prove the configured destination exists through its registered Read door.
    source
        .read(
            &ActionAddress::new(
                ui::feedback::destination::item(
                    ui::feedback::destination::CHANNELS,
                    &destination.channel,
                ),
                ui::feedback::destination::READ_DESTINATION,
            ),
            &serde_json::Map::new(),
        )
        .map_err(|error| unavailable(&error))?;
    let receipt = if let Some(value) = query.submission.take() {
        let submission: Submission =
            serde_json::from_value(value).map_err(|error| unavailable(&error))?;
        let signer = OwnerSigner {
            owner: owner.clone(),
            member,
        };
        let mut service = CanonicalDestination::from_source(
            owner,
            destination,
            Box::new(source),
            Box::new(signer),
        )
        .map_err(|error| unavailable(&error))?;
        // `None` while the report or its picture is still on its way here
        // (the host asks the room for the picture once the report lands):
        // the reporter stays admitted and asks again.
        service
            .receipt_when_kept(&submission, &enrollment)
            .map_err(|error| unavailable(&error))?
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| unavailable(&error))?
    } else {
        None
    };
    let fresh = link
        .view_read_policy(workspace, &caller, "so.tilo.chat", "")
        .filter(|fresh| {
            fresh.room == policy.room
                && fresh.standing == policy.standing
                && fresh.sandbox == policy.sandbox
        })
        .ok_or_else(|| unavailable("feedback admission retired"))?;
    Ok(FeedbackAnswer {
        admission: admission(query, &enrollment, fresh)?,
        receipt,
    })
}

fn admission(
    query: FeedbackQuery,
    enrollment: &Enrollment,
    fresh: super::ViewReadPolicy,
) -> Result<FeedbackAdmission, ActionHostError> {
    let owner = &enrollment.owner;
    let caller = &enrollment.reporter;
    Ok(FeedbackAdmission {
        destination: query.destination,
        owner_user: hex::encode(
            owner
                .user_public_key()
                .ok_or_else(|| unavailable("owner root unavailable"))?,
        ),
        owner_device: hex::encode(
            owner
                .device_public_key()
                .ok_or_else(|| unavailable("owner device unavailable"))?,
        ),
        reporter_user: hex::encode(
            caller
                .user_public_key()
                .ok_or_else(|| unavailable("reporter root unavailable"))?,
        ),
        reporter_device: hex::encode(
            caller
                .device_public_key()
                .ok_or_else(|| unavailable("reporter device unavailable"))?,
        ),
        standing: serde_json::to_value(fresh.standing).map_err(|error| unavailable(&error))?,
        sandbox: serde_json::to_value(fresh.sandbox).map_err(|error| unavailable(&error))?,
        unix_time: fresh.unix_time,
    })
}

struct RetainedSource {
    link: Arc<dyn RoomRecordsLink>,
    catalogue: AppCatalogue,
    workspace: u64,
    caller: Caller,
    route: DocumentRoute,
}
impl RetainedSource {
    fn read(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let mut read = reads::CanonicalRead::prepare(
            &self.catalogue,
            self.link.as_ref(),
            self.workspace,
            &self.caller,
            &self.route,
        )?;
        read.invoke(
            self.link.as_ref(),
            self.workspace,
            &self.caller,
            &self.route,
            address,
            arguments,
        )
        .map_err(|_| "registered feedback read refused".to_owned())?
        .value
        .ok_or_else(|| "feedback projection absent".into())
    }
}
impl CanonicalReceiptSource for RetainedSource {
    fn namespace(&self) -> (String, String) {
        (self.route.app_id.clone(), self.route.cluster_id.clone())
    }
    fn current_report(&self, submission: &Submission) -> Result<(Receipt, String), Error> {
        // Each check reads the retained source afresh; a worker's disposable
        // cache or the reporter's local dispatch cannot establish durability.
        let source = Self {
            link: self.link.clone(),
            catalogue: self.catalogue.clone(),
            workspace: self.workspace,
            caller: self.caller.clone(),
            route: self.route.clone(),
        };
        let author = hex::encode(
            self.caller
                .user_public_key()
                .ok_or(Error::ReceiptMismatch)?,
        );
        let arguments = serde_json::Map::from_iter([(
            "expectedAuthor".into(),
            serde_json::Value::String(author.clone()),
        )]);
        let value = source
            .read(
                &ActionAddress::new(
                    ui::feedback::destination::item(
                        ui::feedback::destination::REPORTS,
                        &submission.id,
                    ),
                    ui::feedback::destination::READ_RECEIPT,
                ),
                &arguments,
            )
            .map_err(|_| Error::NotDurable)?;
        let receipt =
            serde_json::from_value(value.get("content").cloned().ok_or(Error::NotDurable)?)?;
        Ok((receipt, author))
    }
    fn keeps_content(&self, hash: &str) -> Result<bool, Error> {
        // Only bytes the host's own store keeps, verified against their
        // address, witness a report's screenshot.
        Ok(self
            .link
            .content_blob(self.workspace, hash)
            .is_some_and(|bytes| blake3::hash(&bytes).to_hex().as_str() == hash))
    }
}
struct OwnerSigner {
    owner: Caller,
    member: RoomMember,
}
impl ReceiptSigner for OwnerSigner {
    fn owner(&self) -> &Caller {
        &self.owner
    }
    fn sign(&mut self, request: &ReceiptSigningRequest) -> Result<Vec<u8>, Error> {
        Ok(self.member.key.sign(request.digest()).to_bytes().to_vec())
    }
}
