//! What a chain offers the toolkit: its id, the actions it hands out and the
//! ones it carries out, and the parts a step wrote into.

use std::any::Any;

use tilo_actions::{ActionError, ActionInvoker};

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

    fn publish_shared(&self) -> tilo_ui_base::published::SharedResource {
        ChainState::publish_shared(self)
    }

    fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        ChainState::search(self)
    }

    fn items_named(&self, name: &str) -> Vec<tilo_actions::ResourceSpec> {
        ChainState::items_named(self, name)
    }

    fn item(&self, id: &str) -> Option<tilo_actions::ResourceSpec> {
        ChainState::item(self, id)
    }

    fn read_over(&self, item: &str) -> Option<Result<serde_json::Value, String>> {
        ChainState::read_over(self, item)
    }

    fn past_page_len(&self) -> usize {
        ChainState::past_page_len(self)
    }

    fn past_page_window(&self, offset: usize, take: usize) -> Vec<tilo_actions::ResourceSpec> {
        ChainState::past_page_window(self, offset, take)
    }

    fn listing_arriving(&self, filled: bool) -> bool {
        ChainState::listing_arriving(self, filled)
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
        self.apply_answering_refused(action, invoker)
            .map_err(tilo_resource::Refusal::into_reason)
    }

    /// A chain answers a caller that decides on the refusal rather than reads
    /// it (a message the mailbox holds already): the code an action gave is
    /// kept, whichever half of the answer — a part's prepare or its verb —
    /// gave it.
    fn apply_answering_refused(
        &self,
        action: &tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, tilo_resource::Refusal> {
        if action.type_id == PART_EDIT_TYPE {
            let edit = part_edit(&action.payload).map_err(tilo_resource::Refusal::new)?;
            let value = self
                .apply_history(&edit, invoker)
                .map_err(tilo_resource::Refusal::from)?;
            return Ok(tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            });
        }
        // A write the window's person asks through the API is their step, as
        // one their own surface asks is: Undo takes it back.
        match (self.part().decode)(action).map_err(tilo_resource::Refusal::new)? {
            part::Decoded::Group(group) => {
                let created = match self
                    .apply_group_now(&group, invoker)
                    .map_err(tilo_resource::Refusal::from)?
                {
                    Write::Done(written, created) => {
                        self.wrote(invoker, written, true);
                        created
                    }
                    Write::Arriving => return Err(arriving(self)),
                };
                Ok(answering(None, &created))
            }
            part::Decoded::One(action) => {
                self.answered.borrow_mut().take();
                let created = match self
                    .apply_now(&*action, invoker)
                    .map_err(tilo_resource::Refusal::from)?
                {
                    Write::Done(written, created) => {
                        self.wrote(invoker, written, (self.part().is_step)(&*action));
                        created
                    }
                    Write::Arriving => return Err(arriving(self)),
                };
                Ok(answering(self.answered.borrow_mut().take(), &created))
            }
        }
    }
}

/// Why an API's write made nothing: what it changes has not arrived from the
/// workspace, so its caller asks again once it has ([`tilo_actions::api::NOT_READY`]).
/// The newest part is asked for as it is refused, so the next ask finds it.
fn arriving(chain: &ChainState) -> tilo_resource::Refusal {
    chain.want_head();
    tilo_resource::Refusal::new(tilo_actions::api::NOT_READY_REASON)
}

impl ChainState {
    pub(crate) fn id(&self) -> &str {
        &self.core.id
    }

    /// Who reads and writes the parts here.
    pub(crate) fn reader(&self) -> &tilo_resource::MemberId {
        &self.core.workspace.me
    }

    /// Answer a read of `item` that names no item a part keeps but something
    /// the kind derives over every part held ([`Chained::read_over`]): the
    /// messages of a folder, which is what a read of the resource keeping the
    /// chain beside it falls back to. `None` when the kind derives nothing of
    /// it.
    pub(super) fn read_over(&self, item: &str) -> Option<Result<serde_json::Value, String>> {
        let part = self.part();
        let parts = self.parts();
        let typed = (part.typed)(&parts);
        (part.read_over)(&*(self.base)(), &*typed, item)
    }

    /// Write `action` into its part now, as [`Self::apply_as`] does, or say
    /// its part is on its way (`Ok(Write::Arriving)`) with nothing kept to be
    /// written later: a write asked through the API answers its caller, never
    /// whoever holds the chain when the part arrives.
    fn apply_now(&self, action: &dyn Any, invoker: &ActionInvoker) -> Result<Write, ActionError> {
        // Which part is newest is on its way: the write waits for it, and
        // asks for nothing else first (reading the list of waiting writes
        // here would ask the workspace about that instead).
        if self.head_arriving() {
            return Ok(Write::Arriving);
        }
        // Behind the writes already waiting: they are applied in the order
        // they were asked, and this one is not written ahead of them.
        if !self.unsent().is_empty() {
            return Ok(Write::Arriving);
        }
        self.try_apply(action, invoker)
    }

    /// [`Self::apply_now`] for a group: its members written as one, or
    /// nothing while a part they change is on its way.
    fn apply_group_now(
        &self,
        group: &[Box<dyn Any>],
        invoker: &ActionInvoker,
    ) -> Result<Write, ActionError> {
        if self.head_arriving() || !self.unsent().is_empty() {
            return Ok(Write::Arriving);
        }
        self.try_apply_group(group, invoker)
    }

    /// Ask the store's workspace for the chain's newest part, by the name
    /// that stands for it ([`ui_core::resource::DocumentId::chain_head`]): the
    /// devices serving the workspace answer with the part, which the chain
    /// holds once it arrives.
    fn want_head(&self) {
        self.core.workspace.store.want(self.core.chain.chain_head());
        self.core.looked.set(None);
    }
}

/// What a write whose verb declares no answer still tells its caller: the ids
/// it minted and the items it made
/// ([`tilo_actions::contract::MADE_FIELDS`]), never the words of an answer its
/// verb does not declare. `None` when it made nothing.
pub(super) fn only_made(value: Option<serde_json::Value>) -> Option<serde_json::Value> {
    let Some(serde_json::Value::Object(fields)) = value else {
        return None;
    };
    let made: serde_json::Map<String, serde_json::Value> = fields
        .into_iter()
        .filter(|(field, _)| tilo_actions::contract::MADE_FIELDS.contains(&field.as_str()))
        .collect();
    (!made.is_empty()).then_some(serde_json::Value::Object(made))
}
