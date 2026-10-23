//! The models a person may pick for an agent's run, as a record every app
//! of theirs reads.

use tilo_resource::{Model, Refusal, SharedRecord, resource};

use crate::OfferedModel;

/// The models the person's provider settings offer for an agent's run, in
/// the order pickers list them. The session keeps it from those settings,
/// which hold endpoints and keys no app reads; an app's model picker reads
/// this instead, on every device of the person's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct OfferedModels {
    pub models: Vec<OfferedModel>,
}

#[resource(kind = "settings.offered-models", label = "Offered models")]
impl OfferedModels {
    /// Offer `models` for agents' runs, in place of what was offered.
    #[action]
    pub fn offer(&mut self, models: Vec<OfferedModel>) -> Result<(), Refusal> {
        self.models = models;
        Ok(())
    }
}

impl SharedRecord for OfferedModels {
    const OWNER: &'static str = "so.tilo.settings";
}
