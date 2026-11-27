//! The device's search index, as the image model reads and writes it: the pictures it holds that
//! the person may read, and where what one shows is written, beside the entry the app that keeps
//! the picture published.
//!
//! Search's index is what every app publishes what it holds into, kept by the device's daemon. A
//! picture's app publishes one entry that names the picture's bytes (`IndexEntry::image`); the
//! image model reads it ([`tilo_workspace_records::search::SearchAction::Pictures`]) and adds the words it found
//! to that same entry ([`tilo_workspace_records::search::SearchAction::Describe`]), so one picture is one row,
//! found by its name, the text in it and what it shows, whatever app it is in, with no app
//! running.

use std::sync::Arc;
use std::time::Duration;

use crate::analysis::{AnalysisSink, Found, ImageRef, ImageSource};
use tilo_actions::remote::{ActionCallError, Caller, Pending};
use tilo_workspace_records::search::{Answers, Description, Picture, Word};

/// How long the index has to answer one ask.
const PATIENCE: Duration = Duration::from_secs(30);

/// The device's search index, asked as `caller`.
pub struct VisionIndex {
    pub search: Arc<dyn Answers>,
    pub caller: Caller,
}

impl ImageSource for VisionIndex {
    fn images(&self) -> Result<Vec<ImageRef>, String> {
        Ok(answered(self.search.pictures(&self.caller))?
            .into_iter()
            .map(|picture| ImageRef {
                resource: picture.resource,
                app: picture.app,
                workspace: picture.workspace,
                record: picture.record,
                image: picture.image,
            })
            .collect())
    }
}

impl AnalysisSink for VisionIndex {
    fn enriched(&self, image: &ImageRef, found: &Found) -> Result<(), String> {
        let finding = crate::finding(image, found);
        answered(
            self.search.describe(
                &self.caller,
                Description {
                    picture: Picture {
                        resource: image.resource.clone(),
                        app: image.app.clone(),
                        workspace: image.workspace.clone(),
                        record: image.record.clone(),
                        image: image.image.clone(),
                    },
                    title: finding.title,
                    words: finding
                        .fields
                        .into_iter()
                        .map(|(text, weight)| Word { text, weight })
                        .collect(),
                },
            ),
        )
    }
}

/// What the index answered, waited for on the analysis's own thread.
fn answered<T>(pending: Pending<Result<T, ActionCallError>>) -> Result<T, String> {
    match pending.wait_timeout(PATIENCE) {
        Ok(Some(answer)) => answer.map_err(|error| error.to_string()),
        Ok(None) => Err("the search index stopped before it answered".to_owned()),
        Err(_) => Err("the search index did not answer in time".to_owned()),
    }
}
