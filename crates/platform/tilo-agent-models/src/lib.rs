//! The models a person may pick for an agent's run, as a record every app
//! of theirs reads.

use serde::{Deserialize, Serialize};
use tilo_resource::{JsonSchema, Model, Refusal, SharedRecord, resource};

/// A model a person may pick for an agent's run: one model of an enabled
/// provider, named the way pickers show it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OfferedModel {
    /// The provider profile's id.
    pub profile: String,
    /// The provider's label.
    pub provider: String,
    /// The model's id at that provider.
    pub model: String,
    /// The model's label, its id without one.
    pub label: String,
}

tilo_resource::json_model!(OfferedModel);

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
