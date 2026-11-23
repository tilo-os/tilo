//! The person's speakers, published as `session:speakers` (`device.speakers`).
//!
//! The reading is what Settings' speakers page shows — the person's devices with their outputs and
//! microphones, the groups they form, each group's calibration run — and the verbs are the
//! commands the page sends, carried out by the speaker service on the device fabric
//! ([`tilo_speaker_service::settings`]). A tilolang view reads this resource and calls its actions,
//! so Settings' page, the shell and an agent change the same groups, and nothing holds a copy of
//! them.

use std::cell::RefCell;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_speaker_service::published::{SpeakerEffect, Speakers};

use crate::i18n::t;
use crate::platform::fabric::SharedLink;

/// The resource the person's speakers are published under.
pub(crate) const RESOURCE: &str = "session:speakers";

/// The speakers as a declared type: the reading, and the verbs that change the groups.
#[derive(tilo_resource::Row)]
#[row(kind = "session.speakers", label = "Speakers")]
pub(crate) struct SpeakersRoot;

/// The speakers as the session serves them: the fabric the groups live in, and the reading last
/// reported, so a pass that changed nothing is no revision.
pub(crate) struct Served {
    fabric: SharedLink,
    published: RefCell<Option<Speakers>>,
}

impl Served {
    /// The service publishing what `fabric` says of the person's speakers.
    pub(crate) fn new(fabric: SharedLink) -> Self {
        Self {
            fabric,
            published: RefCell::new(None),
        }
    }

    /// The reading as it stands now.
    fn current(&self) -> Speakers {
        tilo_speaker_service::published::reading(self.fabric.as_ref())
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<SpeakerEffect>(
            tilo_resource::LiveRow::typed::<SpeakersRoot>(RESOURCE, t("session-speakers"))
                .into_resource(),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| {
            let published = self.published.borrow().clone();
            reading(&published.unwrap_or_else(|| self.current()))
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        let effect = decode::<SpeakerEffect>(&self.resources(), resource, action, arguments)?;
        let command = effect.command().map_err(ApiError::InvalidArguments)?;
        match tilo_speaker_service::settings::handle(&self.fabric, command) {
            runtime_collab::SpeakerReply::Done { .. } => {
                // The next reader reads the groups as the command left them.
                self.published.borrow_mut().take();
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
            runtime_collab::SpeakerReply::Rejected { reason } => Err(ApiError::Failed(reason)),
            runtime_collab::SpeakerReply::Snapshot { .. } => {
                // Only a snapshot command answers with a snapshot, and none of the verbs is one.
                Err(ApiError::Failed(
                    "the speakers answered a question nothing asked".to_owned(),
                ))
            }
        }
    }

    fn poll(&mut self) -> bool {
        let now = self.current();
        let mut published = self.published.borrow_mut();
        if published.as_ref() == Some(&now) {
            return false;
        }
        *published = Some(now);
        true
    }
}

/// The speakers as a view of another language reads them (`device.speakers`).
#[must_use]
pub(crate) fn surface() -> tilo_actions::surface::Surface {
    tilo_speaker_service::published::surface()
}
