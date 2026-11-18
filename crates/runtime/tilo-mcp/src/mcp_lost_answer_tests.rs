//! An action whose answer was lost on its way: the provider ran it, and the caller heard
//! nothing or that it went unanswered. The call asks again under the same invocation and is
//! answered what the action did; the action does it once.
use super::*;
use crate::agent::tests::{Notes, agent, answer};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Here,
    InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_actions::{InvocationId, InvocationUpdate};

/// Notes whose next answers to an admission are lost after the action was admitted, as an
/// answer is when the session carrying it is taken over. The same invocation asked again is
/// answered what it is and not done again, as every provider keeps its invocations.
#[derive(Clone)]
struct Losing {
    notes: Notes,
    lost: Arc<Mutex<u32>>,
    admitted: Arc<Mutex<HashSet<InvocationId>>>,
    /// The actions done, by name, in order.
    done: Arc<Mutex<Vec<String>>>,
}

impl Losing {
    fn new(lost: u32) -> Self {
        Self {
            notes: Notes::new(true),
            lost: Arc::new(Mutex::new(lost)),
            admitted: Arc::default(),
            done: Arc::default(),
        }
    }
}

impl ActionProvider for Losing {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.notes.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if !self.admitted.lock().unwrap().insert(invocation) {
            let accepted = InvocationUpdate::Accepted { invocation };
            return Pending::ready(Ok(InvocationState::new(invocation, accepted).receipt()));
        }
        self.done
            .lock()
            .unwrap()
            .push(request.target.address.action.clone());
        let admitted = self.notes.invoke(caller, request);
        let lost = {
            let mut lost = self.lost.lock().unwrap();
            let losing = *lost > 0;
            *lost = lost.saturating_sub(1);
            losing
        };
        if lost {
            return Pending::ready(Err(ActionCallError::Unanswered));
        }
        admitted
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.notes.observe(caller, id)
    }

    fn here(&self, caller: &Caller) -> Option<Vec<Here>> {
        self.notes.here(caller)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.notes.cancel(caller, id)
    }

    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.notes.resource_search()
    }
}

tilo_test_env::scenario! {
    fn an_action_whose_answer_was_lost_answers_what_it_did;

    phase it_is_asked_again_and_done_once;
    // The delete was done, but its answer was lost twice: the call is answered that it
    // succeeded, not that it failed, and the delete was done once.
    {
        let notes = Losing::new(2);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
            let (answered, failed) =
                answer(&agent(&notes), "tilo_do", json!({"path": "notes/Todo#delete"})).await;
            assert!(!failed, "{answered}");
            assert_eq!(answered["status"], "ok", "{answered}");
            let done = notes.done.lock().unwrap().clone();
            assert_eq!(
                done.iter().filter(|action| *action == "delete").count(),
                1,
                "{done:?}"
            );
        });
    }
}
