//! Opening an app the device holds but the workspace runs no version of: the person opening it
//! asks to use it in this workspace, so the open installs it first, as that person, through the
//! workspace's own `install` (the same governance check, the same history, undone and
//! uninstalled as every install is), then opens it as any open does.
//!
//! An app the workspace already runs has the grant installing it gave (`app-records:<app>`) in
//! the policy the open is decided by, and opens with no step more. Only an app without it looks
//! for the release this device's package store holds, and the install asks the workspace to
//! leave a version it already runs alone ([`IF_ABSENT`]). Where the workspace refuses the
//! install, the open ends failed with words the person reads, and nothing is installed.

use std::sync::Arc;
use std::time::Duration;

use tilo_action_policy::roles;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_desktop_model::Workspace;
use tilo_peer_state::workspace_membership::AppRelease;
use tilo_workspace_service::workspaces::IF_ABSENT;
#[cfg(not(test))]
use tilo_workspace_service::workspaces::held_release;
// A test's device holds the releases the test hands it, in place of a package store on disk.
#[cfg(test)]
use tests::held_release;

use super::super::host_action_admission::AdmittedHostAction;
use super::super::published_invocation::InvocationCaller;
use super::{AppBrokerState, LaunchScope};
use crate::services::{LaunchContent, LaunchPlace, LaunchableApplication};

/// How long a wait on the install's end goes without being woken before it looks again.
const FOLLOW_LOOK: Duration = Duration::from_secs(1);

/// The failure code of an open whose app the workspace runs no version of, and that could not
/// be installed for the person opening it.
pub(super) const NOT_ADDED: &str = "app_not_added";

/// The message of [`NOT_ADDED`], in the localisation catalogue.
const NOT_ADDED_MESSAGE: &str = "app-open-not-added";

/// An app's open, admitted, held until its app is installed: everything the launch it ends in
/// reads.
pub(super) struct HeldOpen {
    pub caller: Caller,
    pub origin: Option<InvocationCaller>,
    pub workspace: String,
    pub app: String,
    pub hash: String,
    pub scope: Option<LaunchScope>,
    pub author: Option<String>,
    pub place: LaunchPlace,
}

impl HeldOpen {
    /// Launch the app as the open admitted as `admitted` asked.
    pub(super) fn launch(
        &self,
        state: &Arc<AppBrokerState>,
        admitted: AdmittedHostAction,
    ) -> Result<u64, ActionCallError> {
        state.launch_application(
            (&self.caller, self.origin.as_ref(), &self.workspace),
            admitted,
            &self.app,
            LaunchContent {
                hash: &self.hash,
                caller: self.scope.as_ref(),
                author: self.author.as_deref(),
            },
            &self.place,
        )
    }
}

impl AppBrokerState {
    /// The release to install before `caller` opens `app` in `workspace`: the one this device's
    /// package store holds, where a person opens an app the workspace has not given its own
    /// records (it runs no version of it). `None` opens it as it is: an app the workspace runs,
    /// one Tilo did not package, or one the device holds no signed package of.
    pub(super) fn install_before_open(
        caller: &Caller,
        workspace: &Workspace,
        app: &LaunchableApplication,
    ) -> Option<AppRelease> {
        if app.unconfined || !caller.acting().is_person() {
            return None;
        }
        let given = roles::app_records_grant_id(&app.package_id);
        if workspace
            .policy
            .grants
            .iter()
            .any(|grant| grant.id == given)
        {
            return None;
        }
        held_release(&app.package_id)
    }

    /// Install `release` in the open's workspace as the person opening it, through the
    /// workspace's `install`, then launch the app as `admitted` asked; or, where the workspace
    /// refuses, end the open failed with words the person reads.
    pub(super) fn install_then_open(
        self: &Arc<Self>,
        release: &AppRelease,
        admitted: AdmittedHostAction,
        open: HeldOpen,
        name: String,
    ) {
        let mut argument = serde_json::Map::new();
        argument.insert("app".into(), release.app.clone().into());
        argument.insert("version".into(), release.version.clone().into());
        argument.insert("package".into(), release.package.clone().into());
        argument.insert("publisher".into(), release.publisher.clone().into());
        if !release.surface.is_empty() {
            argument.insert("surface".into(), release.surface.clone().into());
        }
        argument.insert(IF_ABSENT.into(), true.into());
        let install = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget::workspace_governance(
                &open.workspace,
                roles::governance::INSTALL_APP,
            ),
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        // The install is the person's own call, owned as the open was: a person at this device's
        // shell owns each of their calls by its own id.
        let origin = open.origin.as_ref().map(|origin| match origin {
            InvocationCaller::NativePerson {
                workspace,
                certificate,
                time,
                ..
            } => InvocationCaller::NativePerson {
                invocation: install.invocation,
                workspace: workspace.clone(),
                certificate: certificate.clone(),
                time: time.clone(),
            },
            other => other.clone(),
        });
        let receipt =
            self.action_invoke_with_origin(&open.caller, None, None, origin.as_ref(), &install);
        let state = Arc::clone(self);
        self.settled(open.caller.clone(), install.invocation, receipt, move |installed| {
            match installed {
                Ok(()) => {
                    if let Err(error) = open.launch(&state, admitted) {
                        tracing::warn!(app = %open.app, %error, "an app installed on its open did not launch");
                    }
                }
                Err(reason) => {
                    tracing::warn!(
                        app = %open.app,
                        workspace = %open.workspace,
                        %reason,
                        "opening an app the workspace does not run could not install it"
                    );
                    let message = not_added(
                        settings_store::appearance::preferred_language().as_deref(),
                        &name,
                    );
                    let failed = InvocationUpdate::Failed {
                        invocation: admitted.invocation,
                        error: ActionFailure {
                            code: NOT_ADDED.into(),
                            message,
                            details: Some(serde_json::json!({ "reason": reason })),
                            retryable: false,
                        },
                    };
                    if let Err(error) = state.complete_host_action(&admitted, failed) {
                        tracing::warn!(%error, "an open's refusal could not be recorded");
                    }
                }
            }
        });
    }

    /// Call `then` once the invocation `invocation` that `receipt` answers ended: `Ok` where it
    /// succeeded, else why it did not.
    fn settled(
        self: &Arc<Self>,
        caller: Caller,
        invocation: InvocationId,
        receipt: Pending<Result<InvocationReceipt, ActionCallError>>,
        then: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        let state = Arc::clone(self);
        receipt.then(move |receipt| {
            let status = match receipt {
                None => return then(Err("the install was not answered".into())),
                Some(Err(error)) => return then(Err(error.to_string())),
                Some(Ok(receipt)) => receipt.status,
            };
            match status {
                ReceiptStatus::Completed { .. } => return then(Ok(())),
                ReceiptStatus::Failed { error } => return then(Err(error.message)),
                ReceiptStatus::Rejected { error } => return then(Err(error.to_string())),
                ReceiptStatus::Cancelled { .. } => {
                    return then(Err("the install was cancelled".into()));
                }
                ReceiptStatus::Accepted => {}
            }
            let runtime = match tilo_rt::try_app() {
                Ok(runtime) => runtime,
                Err(error) => return then(Err(format!("nothing follows the install: {error}"))),
            };
            drop(runtime.spawn(async move {
                loop {
                    let change = state.action_next_change_after_now(&caller, &invocation);
                    match state.action_observe(&caller, &invocation) {
                        None => return then(Err("the install is no longer known".into())),
                        Some(observed) if observed.update.is_terminal() => {
                            return then(match observed.update {
                                InvocationUpdate::Succeeded { .. } => Ok(()),
                                InvocationUpdate::Failed { error, .. } => Err(error.message),
                                other => Err(format!("the install ended as {other:?}")),
                            });
                        }
                        Some(_) => {}
                    }
                    match change {
                        Some(change) => drop(change.await),
                        None => tilo_rt::sleep(FOLLOW_LOOK).await,
                    }
                }
            }));
        });
    }
}

/// What an open says, in `language`, when the workspace runs no version of the app `name` and
/// could not install it for the person opening it.
fn not_added(language: Option<&str>, name: &str) -> String {
    let mut args = tilo_history::FluentArgs::new();
    args.set("app", name.to_owned());
    tilo_workspace_service::i18n::message(language, NOT_ADDED_MESSAGE, &args)
}

#[cfg(test)]
mod tests {
    use super::{NOT_ADDED, not_added};
    use crate::collab_app_broker::AppBrokerState;
    use crate::services::{LaunchPlace, LaunchableApplication, LauncherServices, ProcessServices};
    use crate::test_support::{Role, Workspace, WorkspaceId, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::admitted::top_level;
    use tilo_actions::remote::{
        ActionProvider, Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending,
        RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER, workspace_governance_address,
        workspace_provider,
    };
    use tilo_actions::{ActionDescriptor, InvocationUpdate};
    use tilo_peer_state::workspace_membership::{AppPayload, AppRelease};
    use tilo_workspace_service::workspaces::providers::{Providers, Reach};

    const APP: &str = "so.tilo.lang.gallery";

    /// What the test's device holds of each app, as its package store would.
    static HELD: Mutex<Vec<AppRelease>> = Mutex::new(Vec::new());

    pub(super) fn held_release(app: &str) -> Option<AppRelease> {
        HELD.lock()
            .unwrap()
            .iter()
            .find(|release| release.app == app)
            .cloned()
    }

    /// The workspace's governance as its host answers it: installing an app is decided by the
    /// caller's role, and an Editor's is refused. Every install it applied is kept.
    struct Governance {
        workspace: String,
        owner: String,
        applied: Mutex<Vec<serde_json::Value>>,
    }

    impl tilo_actions::remote::Admits for Governance {
        const PROVIDER: &'static str = WORKSPACE_GOVERNANCE_PROVIDER;
        const APP: &'static str = WORKSPACE_GOVERNANCE_PROVIDER;

        fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
            let install = ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: workspace_governance_address(
                    &self.workspace,
                    tilo_action_policy::roles::governance::INSTALL_APP,
                ),
                resource_kind: "workspace.apps".into(),
                resource_label: "Apps".into(),
                action_label: "Run a version of an app".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Effect,
                contract: tilo_actions::ActionContract {
                    kind: tilo_action_policy::roles::GOVERNANCE_KIND,
                    ..tilo_actions::ActionContract::default()
                },
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            };
            top_level(vec![install])
        }

        fn start(
            &self,
            caller: &Caller,
            request: &InvokeRequest,
            _admitted: &Admitted,
        ) -> Result<Pending<Outcome>, String> {
            if caller.acting().person_id() != Some(self.owner.as_str()) {
                return Err("this member may not install apps in the workspace".into());
            }
            self.applied
                .lock()
                .unwrap()
                .push(serde_json::to_value(&request.argument).unwrap());
            Ok(Pending::ready(Ok(tilo_actions::ActionOutput::default())))
        }
    }

    /// A device that holds Gallery and keeps every launch it was asked for.
    #[derive(Default)]
    struct Launcher {
        launched: Mutex<Vec<String>>,
    }

    impl LauncherServices for Launcher {
        fn applications(&self) -> Vec<LaunchableApplication> {
            vec![LaunchableApplication {
                capabilities: Vec::new(),
                package_id: APP.into(),
                display_name: "Gallery".into(),
                unconfined: false,
                offers: Vec::new(),
                surface: None,
            }]
        }
        fn launch(
            &self,
            app: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &LaunchPlace,
        ) -> Result<(), String> {
            self.launched.lock().unwrap().push(app.to_owned());
            Ok(())
        }
        fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
            false
        }
    }

    /// The open's state once it ended, waiting on the broker's own change signal.
    fn ended(
        state: &Arc<AppBrokerState>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> InvocationUpdate {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut changes = state.invocation_changes();
        loop {
            if let Some(observed) = state.action_observe(caller, &request.invocation)
                && observed.update.is_terminal()
            {
                return observed.update;
            }
            runtime
                .block_on(async {
                    tokio::time::timeout(tilo_test_env::hang_bound(), changes.changed()).await
                })
                .expect("the open ends")
                .unwrap();
        }
    }

    // USER REQUIREMENT: a member of someone else's workspace, whose role doesn't let them add
    // apps, opens an app their computer has but the workspace doesn't run: they're told plainly
    // that its owners add apps and to ask one, and nothing is installed or granted.
    #[test]
    fn an_open_the_workspace_may_not_install_for_says_so_and_installs_nothing() {
        let owner = hex::encode([51; 32]);
        let editor = hex::encode([52; 32]);
        let mut workspace = Workspace::new(WorkspaceId::from_u128(52), "Team");
        workspace.policy.set_owner(owner.clone());
        workspace.policy.add_member(editor.clone(), Role::Editor);
        let workspace_id = workspace.id().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let tree = tree.shared();
        HELD.lock().unwrap().push(AppRelease {
            app: APP.into(),
            version: "1.0.0".into(),
            surface: String::new(),
            package: "a".repeat(64),
            publisher: "b".repeat(64),
            file: None,
            dev: false,
            pinned: false,
            payload: AppPayload::Tapp,
        });
        let governance = Arc::new(Governance {
            workspace: workspace_id.clone(),
            owner,
            applied: Mutex::new(Vec::new()),
        });
        let providers = Providers::default();
        providers.provide(
            WORKSPACE_GOVERNANCE_PROVIDER,
            WORKSPACE_GOVERNANCE_PROVIDER,
            Reach::ThisDevice,
            Arc::clone(&governance) as Arc<dyn ActionProvider + Send + Sync>,
        );
        let launcher = Arc::new(Launcher::default());
        let (state, _bridge_rx) = AppBrokerState::test_state(
            Some(tree.authority()),
            ProcessServices {
                providers: Some(providers),
                launcher: Some(Arc::clone(&launcher) as _),
                ..Default::default()
            },
        );
        let caller = Caller::local_person(editor).in_workspace(workspace_id.clone());
        let open = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: workspace_provider(&workspace_id),
                address: tilo_actions::ActionAddress::new(
                    format!("{}{APP}", tilo_actions::remote::APPLICATION_RESOURCE_PREFIX),
                    tilo_actions::remote::APPLICATION_OPEN_ACTION,
                ),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        state
            .action_invoke(&caller, None, None, &open)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the open is answered")
            .expect("the editor may open apps here");

        match ended(&state, &caller, &open) {
            InvocationUpdate::Failed { error, .. } => {
                assert_eq!(error.code, NOT_ADDED, "{error:?}");
                assert_eq!(error.message, not_added(None, "Gallery"), "{error:?}");
                assert!(error.message.contains("Ask an owner"), "{error:?}");
            }
            other => panic!("the open is refused: {other:?}"),
        }
        assert!(
            governance.applied.lock().unwrap().is_empty(),
            "the workspace applied no install"
        );
        assert!(
            launcher.launched.lock().unwrap().is_empty(),
            "nothing was launched"
        );
        let given = tilo_action_policy::roles::app_records_grant_id(APP);
        let held = tree.authority();
        assert!(
            held.decide_in_workspace(&workspace_id, |workspace| {
                workspace.is_some_and(|workspace| {
                    !workspace
                        .policy
                        .grants
                        .iter()
                        .any(|grant| grant.id == given)
                })
            })
            .unwrap(),
            "no grant gives Gallery its records"
        );
    }

    #[test]
    fn a_refused_open_names_the_app_and_who_adds_it() {
        let said = not_added(None, "Gallery");
        assert!(said.contains("Gallery") && said.contains("owner"), "{said}");
        assert!(
            !said.contains("app-open-not-added"),
            "the catalogue holds it: {said}"
        );
        let german = not_added(Some("de-DE"), "Gallery");
        assert_ne!(german, said, "the message is translated");
        assert!(
            german.contains("Gallery") && german.contains("Eigentümer"),
            "{german}"
        );
    }
}
