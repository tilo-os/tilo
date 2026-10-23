//! The head of the live state, kept while what it is published from is
//! unchanged.

use std::rc::Rc;

use super::ResourceRoot;

impl ResourceRoot {
    /// The head of the live state, `state`, of a resource that may reuse
    /// its publication at `epoch`: the very head published last while what
    /// it is published from ([`tilo_resource::HeadInputs`]) is the same, so
    /// a change the head does not show (a keystroke in a paragraph, more of
    /// a long document read) neither publishes it again nor moves what is
    /// built over it.
    pub(super) fn live_head(
        &self,
        state: &dyn std::any::Any,
        epoch: &str,
    ) -> Rc<tilo_actions::Resource> {
        let at = (
            self.version(),
            ui_core::i18n::I18nManager::revision(),
            epoch.to_owned(),
        );
        if let Some(kept) = self
            .live_head
            .borrow()
            .as_ref()
            .filter(|kept| kept.at == at)
        {
            return Rc::clone(&kept.resource);
        }
        let id = self.dispatch.resource_id();
        let inputs = self.typed.head_inputs(state);
        let mut kept = self.live_head.borrow_mut();
        if let Some(kept) = kept.as_mut().filter(|kept| {
            kept.at.1 == at.1
                && kept.at.2 == at.2
                && kept.id == id
                && kept.label == self.label
                && Some(&kept.inputs) == inputs.as_ref()
        }) {
            kept.at = at;
            return Rc::clone(&kept.resource);
        }
        let _profile = tilo_resource::profiling::Phase::start("framework.resource_head");
        let Some(inputs) = inputs else {
            return Rc::new(self.typed.head(id, &self.label, state));
        };
        let resource = Rc::new(self.typed.head_of(id, &self.label, &inputs));
        *kept = Some(LiveHead {
            at,
            id: id.to_owned(),
            label: self.label.clone(),
            inputs,
            resource: Rc::clone(&resource),
        });
        resource
    }
}

/// The head of the live state ([`ResourceRoot::live_head`]) and what it was
/// published from.
pub(super) struct LiveHead {
    /// The live version, locale and epoch it was last found current at.
    at: ((u64, u64), u64, String),
    id: String,
    label: String,
    inputs: tilo_resource::HeadInputs,
    resource: Rc<tilo_actions::Resource>,
}
