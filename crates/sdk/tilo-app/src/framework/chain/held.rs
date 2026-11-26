//! The chains and child resources a window has opened.

use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::ActionInvoker;
use tilo_resource::{Refusal, Resource};
use ui_core::resource::DocumentId;

use super::{ASKED, ChainState, Chained, ResourceApi, WorkspaceResources, api};

/// The chains a window holds, by part type and chain.
type HeldChains = BTreeMap<(&'static str, String), Rc<ChainState>>;

/// The chains a window's views opened, by part type and chain: one per each;
/// and the child records they opened, by container.
#[derive(Default)]
pub(crate) struct Chains {
    pub(super) held: RefCell<HeldChains>,
    /// The child records held, by their container's address.
    children: RefCell<BTreeMap<String, Rc<dyn Any>>>,
    apis: RefCell<Vec<Rc<dyn ResourceApi>>>,
}

impl Chains {
    /// The chain of `P`, opened in `workspace` on first use.
    pub(crate) fn open<P: Chained>(
        &self,
        workspace: Option<&WorkspaceResources>,
        record: Option<&DocumentId>,
        base: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Rc<ChainState>> {
        // Beside the record the window shows: the app's own record of the
        // workspace, or the one per member where the app keeps those.
        let chain = match record {
            Some(record) => P::chain_beside(record),
            None if P::BESIDE_RESOURCE => return None,
            None => P::chain(),
        };
        if let Some(held) = self.opened(P::KIND, &chain) {
            return Some(held);
        }
        self.open_at::<P>(workspace?, &chain, base)
    }

    /// The chain of `P` that lives beside the record `base`
    /// ([`Chained::chain_beside`]), opened in `workspace` on first use.
    pub(crate) fn open_beside<P: Chained>(
        &self,
        workspace: Option<&WorkspaceResources>,
        base: &DocumentId,
        base_state: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Rc<ChainState>> {
        self.open_at::<P>(workspace?, &P::chain_beside(base), base_state)
    }

    /// The chain of `P` kept at `chain` in `workspace`, opened on first use.
    /// The base is one of the app's own record, or a record of a workspace it
    /// keeps beside its own (a chat, whose parts hold its messages).
    pub(crate) fn open_at<P: Chained>(
        &self,
        workspace: &WorkspaceResources,
        chain: &DocumentId,
        base_state: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Rc<ChainState>> {
        if let Some(held) = self.opened(P::KIND, chain) {
            return Some(held);
        }
        let state = Rc::new(ChainState::open::<P>(
            workspace.clone(),
            base_state,
            chain.clone(),
        )?);
        self.hold(chain, &state);
        Some(state)
    }

    /// The chain of parts of `kind` held for `chain`, if the views opened it.
    fn opened(&self, kind: &'static str, chain: &DocumentId) -> Option<Rc<ChainState>> {
        self.held
            .borrow()
            .get(&(kind, chain.as_str().to_owned()))
            .cloned()
    }

    /// Keep `state` for the views that ask for its chain again, published
    /// and answered beside the others.
    fn hold(&self, chain: &DocumentId, state: &Rc<ChainState>) {
        self.held.borrow_mut().insert(
            (state.part().kind, chain.as_str().to_owned()),
            Rc::clone(state),
        );
        self.apis
            .borrow_mut()
            .push(Rc::clone(state) as Rc<dyn ResourceApi>);
    }

    /// The children of `C` the parent lists, held in `workspace` on first use:
    /// published and answered beside the chains, through the same API.
    pub(crate) fn open_children<C: super::super::children::ChildResource>(
        &self,
        workspace: Option<&WorkspaceResources>,
        parent: impl Fn() -> Rc<<C::Parent as Resource>::State> + 'static,
    ) -> Option<Rc<super::super::children::ChildrenState<C>>> {
        let key = ui_core::resource::children_resource_id(&C::parent_resource(), C::KIND);
        if let Some(held) = self.children.borrow().get(&key) {
            return Rc::clone(held)
                .downcast::<super::super::children::ChildrenState<C>>()
                .ok();
        }
        let state = Rc::new(super::super::children::ChildrenState::<C>::open(
            workspace?.clone(),
            Rc::new(parent),
        )?);
        self.children
            .borrow_mut()
            .insert(key, Rc::clone(&state) as Rc<dyn Any>);
        self.apis
            .borrow_mut()
            .push(Rc::clone(&state) as Rc<dyn ResourceApi>);
        Some(state)
    }

    pub(crate) fn apis(&self) -> Vec<Rc<dyn ResourceApi>> {
        self.apis.borrow().clone()
    }

    /// Answer one published action of a chain: apply it to the part it
    /// changes, for whoever the host admitted to invoke it.
    pub(crate) fn answer(
        &self,
        me: Option<&tilo_resource::MemberId>,
        payload: &[u8],
        live: &mut tilo_ui_base::widget::Edits<'_>,
    ) -> Result<tilo_actions::ActionOutput, Refusal> {
        let request: super::super::resources::ResourceAction =
            tilo_actions::decode_json(payload).map_err(|error| Refusal::new(error.to_string()))?;
        let api = self
            .apis
            .borrow()
            .iter()
            .find(|api| api.id() == request.record)
            .cloned()
            .ok_or_else(|| Refusal::new(format!("no chain `{}` is open here", request.record)))?;
        let invoker = ui_core::invocation_scope::current_invoker()
            .or_else(|| me.cloned().map(ActionInvoker::member))
            .ok_or_else(|| Refusal::new("nobody changes a past version"))?;
        let asked = ASKED.with(|asked| asked.replace(request.answers.clone()));
        let output = api.apply_in_refused(&request.action, &invoker, live);
        ASKED.with(|asking| asking.replace(asked));
        let mut output = output?;
        // A verb declared to answer nothing answers nothing of its own
        // (it keeps only what the write made), which a caller
        // still needs: a create's made items are the ids of what it made, and
        // without them it cannot learn the id it created. It still names the
        // text it wrote: where it landed.
        if request.quiet {
            let made = api::only_made(output.value.take());
            output.value = match output.wrote() {
                Some(wrote) => tilo_actions::ActionOutput::noting_wrote(made, &wrote),
                None => made,
            };
        }
        Ok(output)
    }

    /// Answer a read of the chain `chain` ([`CHAIN_READ_TYPE`]) from the
    /// parts this device holds.
    pub(crate) fn read(&self, chain: &str, payload: &[u8]) -> Result<serde_json::Value, String> {
        let api = self
            .apis
            .borrow()
            .iter()
            .find(|api| api.id() == chain)
            .cloned()
            .ok_or_else(|| format!("no chain `{chain}` is open here"))?;
        api.read(payload)
    }
}
