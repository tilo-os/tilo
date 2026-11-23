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

/// Notes behind a workspace that fails the way a real one does:
/// - `lost`: once set, its count of the next answers to an admission are lost after the action
///   was admitted, as an answer is when the session carrying it is taken over. The same
///   invocation asked again is answered what it is and not done again, as every provider keeps
///   its invocations.
/// - `mute`: the apps' catalogues go unanswered, as a workspace that just started does while it
///   opens the documents a listing reads: a path is not resolved, and nothing is invoked.
/// - `unserved`: an installed app that is not running is refused a start as not serving this
///   caller yet, as a workspace just made does.
#[derive(Clone)]
struct Faulty {
    notes: Notes,
    lost: Arc<Mutex<Option<u32>>>,
    admitted: Arc<Mutex<HashSet<InvocationId>>>,
    /// The actions done, by name, in order.
    done: Arc<Mutex<Vec<String>>>,
    mute: Arc<Mutex<bool>>,
    held: Arc<Mutex<Vec<Unanswered>>>,
    unserved: Arc<Mutex<bool>>,
}

/// A catalogue page asked and held unanswered.
type Unanswered = tilo_actions::remote::Completer<Result<CataloguePage, ActionCallError>>;

impl Faulty {
    fn new(notes: Notes) -> Self {
        Self {
            notes,
            lost: Arc::default(),
            admitted: Arc::default(),
            done: Arc::default(),
            mute: Arc::default(),
            held: Arc::default(),
            unserved: Arc::default(),
        }
    }
}

impl ActionProvider for Faulty {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let of_an_app = query
            .provider
            .as_deref()
            .is_some_and(|provider| tilo_actions::remote::provider_workspace(provider).is_none());
        if of_an_app && *self.mute.lock().unwrap() {
            let (unanswered, answer) = tilo_actions::remote::pending();
            self.held.lock().unwrap().push(unanswered);
            return answer;
        }
        self.notes.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if request.target.address.action == "start" && *self.unserved.lock().unwrap() {
            let refused = ActionCallError::NotReady;
            let error = tilo_actions::ActionFailure {
                code: refused.code().to_owned(),
                message: refused.to_string(),
                details: None,
                retryable: false,
            };
            let failed = InvocationUpdate::Failed { invocation, error };
            return Pending::ready(Ok(InvocationState::new(invocation, failed).receipt()));
        }
        if self.lost.lock().unwrap().is_none() {
            return self.notes.invoke(caller, request);
        }
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
            let losing = lost.is_some_and(|left| left > 0);
            *lost = lost.map(|left| left.saturating_sub(1));
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
    fn a_call_whose_answer_was_lost_or_refused_for_now_is_asked_again_by_its_id;

    phase an_action_whose_answer_was_lost_is_asked_again_and_done_once;
    // The delete was done, but its answer was lost twice: the call is answered that it
    // succeeded, not that it failed, and the delete was done once.
    {
        let notes = Faulty::new(Notes::new(true));
        *notes.lost.lock().unwrap() = Some(2);
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

    phase a_write_whose_path_went_unanswered_is_told_its_id_and_what_to_do;
    // A write whose path the workspace did not resolve in time is told so typed, with what
    // to do and the invocation id to do it under; asked again under that id once the
    // workspace answers, it is done.
    {
        let opening = Faulty::new(Notes::new(true));
        *opening.mute.lock().unwrap() = true;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async {
                let server = agent(&opening);
                let rename = json!({"path": "notes/Todo#rename", "args": {"title": "Later"}});
                let (refused, failed) = answer(&server, "tilo_do", rename.clone()).await;
                assert!(failed, "{refused}");
                assert_eq!(refused["status"], "failed", "{refused}");
                assert_eq!(refused["error"]["code"], "unanswered", "{refused}");
                let next = refused["error"]["next_steps"][0].as_str().unwrap_or_default();
                assert!(next.contains("invocation_id"), "{refused}");
                let id = refused["invocation_id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("it names its invocation: {refused}"))
                    .to_owned();
                assert!(
                    opening.notes.invoked.lock().unwrap().is_empty(),
                    "nothing ran before its path was resolved"
                );

                *opening.mute.lock().unwrap() = false;
                let mut again = rename;
                again["invocation_id"] = json!(id);
                let (done, failed) = answer(&server, "tilo_do", again).await;
                assert!(!failed, "{done}");
                assert_eq!(done["status"], "ok", "{done}");
                let renamed: Vec<String> = opening
                    .notes
                    .invoked
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|request| request.target.address.action == "rename")
                    .map(|request| request.invocation.0.to_string())
                    .collect();
                assert_eq!(renamed, [id], "done once, under the id it was told");
            });
    }

    phase a_write_whose_app_the_workspace_did_not_start_yet_is_told_not_ready_with_its_id;
    // A write in an app the workspace would not start yet, as it does not serve this caller
    // yet, is told so as not ready, with its invocation id; asked again under that id once the
    // workspace serves it, it starts the app and is done.
    {
        let unserved = Faulty::new(Notes::new(false));
        *unserved.unserved.lock().unwrap() = true;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async {
                let server = agent(&unserved);
                let read = json!({"path": "notes#read"});
                let (refused, failed) = answer(&server, "tilo_do", read.clone()).await;
                assert!(failed, "{refused}");
                assert_eq!(refused["error"]["code"], "not_ready", "{refused}");
                assert!(
                    refused["error"]["next_steps"][0].is_string(),
                    "it is told what to do: {refused}"
                );
                let id = refused["invocation_id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("it names its invocation: {refused}"))
                    .to_owned();

                *unserved.unserved.lock().unwrap() = false;
                let mut again = read;
                again["invocation_id"] = json!(id);
                let (done, failed) = answer(&server, "tilo_do", again).await;
                assert!(!failed, "{done}");
                assert_eq!(done["status"], "ok", "{done}");
            });
    }
}
