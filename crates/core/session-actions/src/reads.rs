//! The device's record API, asked through this device's session: each read and listing is a
//! [`RemoteActionMessage::ResourceRead`] the device answers whole, deciding it as it decides every
//! reader's.

use tilo_actions::remote::{
    ActionCallError, Caller, Pending, RemoteActionMessage, ResourceAddress, ResourceCopy,
    ResourceReadRequest, ResourceReads,
};

use super::SessionActions;

/// The device's record API, asked through the session: each read and listing goes to the device
/// whole, which decides it as it decides every reader's.
pub(super) struct SessionReads(pub(super) SessionActions);

/// How long a read asked through the session waits for the device to answer it.
const READ_ANSWERED: std::time::Duration = std::time::Duration::from_secs(30);

impl SessionReads {
    /// What the device answers `read`, as JSON.
    fn asked(&self, read: ResourceReadRequest) -> Result<serde_json::Value, ActionCallError> {
        let answered = self
            .0
            .read_record(read)
            .wait_timeout(READ_ANSWERED)
            .ok()
            .flatten()
            .unwrap_or(Err(ActionCallError::Unanswered))?;
        serde_json::from_str(&answered).map_err(|error| ActionCallError::Unavailable {
            reason: format!("the device's reading does not read: {error}"),
        })
    }
}

/// A read of `app`'s `record` in `workspace` as it stands now, or with `listing` what of
/// `workspace` the caller may read.
fn now_of(workspace: &str, app: &str, record: &str, listing: bool) -> ResourceReadRequest {
    ResourceReadRequest {
        workspace: workspace.to_owned(),
        app: app.to_owned(),
        record: record.to_owned(),
        joined: false,
        after: None,
        wait_ms: None,
        document: false,
        listing,
    }
}

impl ResourceReads for SessionReads {
    fn answer_read(
        self: std::sync::Arc<Self>,
        _caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        self.0
            .read_record(read)
            .map(move |result| RemoteActionMessage::ResourceReading {
                correlation,
                result,
            })
    }

    fn read(
        &self,
        _caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let readings = self.asked(now_of(
            &address.workspace,
            &address.app,
            &address.record,
            false,
        ))?;
        let record = readings
            .get("records")
            .and_then(serde_json::Value::as_array)
            .and_then(|records| records.first());
        match record {
            Some(record) if record.get("arriving").is_some() => Err(ActionCallError::Unavailable {
                reason: "the record is still on its way to the device".into(),
            }),
            Some(record) => Ok(record
                .get("value")
                .cloned()
                .map_or(ResourceCopy::Absent, ResourceCopy::Read)),
            None => Ok(ResourceCopy::Absent),
        }
    }

    fn list(
        &self,
        _caller: &Caller,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<ResourceAddress>, ActionCallError> {
        let listed = self.asked(now_of(workspace, app, "", true))?;
        Ok(listed
            .get("records")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|listed| {
                Some(ResourceAddress {
                    workspace: workspace.to_owned(),
                    app: listed.get("app")?.as_str()?.to_owned(),
                    record: listed.get("record")?.as_str()?.to_owned(),
                })
            })
            .collect())
    }
}
