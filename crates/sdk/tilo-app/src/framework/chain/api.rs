//! What a chain offers the toolkit: its id, the actions it hands out and the
//! ones it carries out, and the parts a step wrote into.

use tilo_actions::ActionInvoker;

use super::super::history::PART_EDIT_TYPE;
use super::super::made::answering;
use super::super::resources::ResourceApi;
use super::{ChainState, Write, part, part_edit};

impl ResourceApi for ChainState {
    fn id(&self) -> &str {
        &self.core.id
    }

    fn editable(&self) -> bool {
        true
    }

    fn revision(&self) -> u64 {
        ChainState::revision(self)
    }

    fn publish(&self) -> tilo_actions::ResourceSpec {
        ChainState::publish(self)
    }

    fn publish_head(&self) -> tilo_actions::ResourceSpec {
        ChainState::publish_head(self)
    }

    fn publish_shared(&self) -> ui_core::published::SharedResource {
        ChainState::publish_shared(self)
    }

    fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        ChainState::search(self)
    }

    fn items_named(&self, name: &str) -> Vec<tilo_actions::ResourceSpec> {
        ChainState::items_named(self, name)
    }

    fn read(&self, payload: &[u8]) -> Result<serde_json::Value, String> {
        ChainState::read(self, payload)
    }

    fn apply_encoded(
        &self,
        action: &tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<(), String> {
        self.apply_answering(action, invoker).map(drop)
    }

    fn apply_answering(
        &self,
        action: &tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, String> {
        if action.type_id == PART_EDIT_TYPE {
            let edit = part_edit(&action.payload)?;
            let value = self
                .apply_history(&edit, invoker)
                .map_err(|error| error.to_string())?;
            return Ok(tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            });
        }
        match (self.part().decode)(action)? {
            part::Decoded::Group(group) => {
                let created = match self
                    .try_apply_group(&group, invoker)
                    .map_err(|error| error.to_string())?
                {
                    Write::Done(_, created) => created,
                    Write::Arriving => Vec::new(),
                };
                Ok(answering(None, &created))
            }
            part::Decoded::One(action) => {
                self.answered.borrow_mut().take();
                // One that waits for its part creates its items once it is written.
                let created = self
                    .apply_as(&*action, invoker, &mut None)
                    .map_err(|error| error.to_string())?
                    .map(|(_, created)| created)
                    .unwrap_or_default();
                Ok(answering(self.answered.borrow_mut().take(), &created))
            }
        }
    }
}
