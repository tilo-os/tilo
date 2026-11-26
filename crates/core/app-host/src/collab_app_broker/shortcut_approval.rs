//! What a triggered shortcut run is admitted on: the approval its saver signed.
//!
//! Nobody is at a screen when a shortcut's trigger fires, and the host that runs it may be any
//! device of the workspace: a server, or a computer someone else signed in to. The run's proof
//! is then the shortcut as its saver saved it, signed by their device (or their agent's grant).
//! It proves only that shortcut: every call the run makes is admitted when it is the run itself
//! or one of the approved steps, with the same action, the same fixed resource and the literal
//! arguments the saver wrote, and each admission reads the workspace again, so an approval that
//! was edited, revoked or whose saver left the workspace stops proving anything at once.

use super::{AppBrokerState, BrokerActor, InvocationCaller, InvocationOwner};
use std::collections::BTreeMap;
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument};
use tilo_automation::{Binding, Selector, Shortcut, Step, StepKind};

/// A saved shortcut as its saver approved it, proven when the run started.
#[derive(Clone)]
pub(super) struct ShortcutApproval {
    workspace: String,
    /// The shortcut exactly as it was proven, with its proof.
    shortcut: Shortcut,
    saver: tilo_peer_state::workspace::Saver,
    /// The saver's root key.
    root: [u8; 32],
    /// The certificate of the key that signed the approval: the saver's device's, or their
    /// agent's grant.
    certificate: tilo_user_identity::DeviceCert,
    time: Option<crate::policy_clock::PolicyTime>,
}

/// The action a call reaches, as an admission resolved it: its app, resource kind, resource and
/// action.
pub(super) type Reached<'a> = (&'a str, &'a str, &'a str, &'a str);

impl ShortcutApproval {
    pub(super) fn workspace(&self) -> &str {
        &self.workspace
    }

    pub(super) const fn time(&self) -> Option<&crate::policy_clock::PolicyTime> {
        self.time.as_ref()
    }

    /// The shortcut's id.
    pub(super) fn shortcut(&self) -> &str {
        &self.shortcut.id
    }

    /// Who owns what the run does: the saver, by their root, the key that signed the approval
    /// and the agent of theirs that saved it, where one did.
    pub(super) fn owner(&self) -> InvocationOwner {
        InvocationOwner::AuthenticatedLocal {
            root: self.root,
            device: self.saver.key,
            agent: self.saver.agent.clone(),
        }
    }

    /// The certificate of the key that signed the approval.
    pub(super) const fn certificate(&self) -> &tilo_user_identity::DeviceCert {
        &self.certificate
    }

    /// The agent of the saver's that saved the shortcut, where one did.
    pub(super) fn agent(&self) -> Option<&str> {
        self.saver.agent.as_deref()
    }

    /// Who the approval runs as: the saver, or the agent of theirs that saved it.
    pub(super) fn actor(&self) -> tilo_action_policy::Actor {
        self.saver
            .agent
            .as_ref()
            .map_or_else(
                || tilo_action_policy::MemberId::person(self.saver.person.clone()),
                |agent| {
                    tilo_action_policy::MemberId::agent(agent.as_str(), self.saver.person.clone())
                },
            )
            .into()
    }

    /// Who the approval is decided as: the saver, or their agent acting for them.
    pub(super) fn decided_as(&self) -> BrokerActor {
        let person = self.saver.person.clone();
        match &self.saver.agent {
            Some(agent) => BrokerActor::agent(agent.as_str(), person.clone()).for_person(person),
            None => BrokerActor::person(person),
        }
    }

    /// The caller a continuation of the run is admitted as: the saver, running this shortcut.
    pub(super) fn caller(&self) -> Caller {
        Caller::local_person(self.saver.person.clone())
            .in_workspace(self.workspace.clone())
            .running_shortcut(self.shortcut.id.clone())
    }

    /// Whether `actor` is the one the approval runs as: the saver, or the agent of theirs that
    /// saved it.
    fn runs_as(&self, actor: &tilo_action_policy::Actor) -> bool {
        actor.person_id() == Some(self.saver.person.as_str())
            || self.saver.agent.as_ref().is_some_and(|agent| {
                actor.member_id()
                    == Some(&tilo_action_policy::MemberId::agent(
                        agent.as_str(),
                        self.saver.person.clone(),
                    ))
            })
    }

    /// Whether the approval covers a call of `reached` with `argument`: the run itself, or one
    /// of its steps, with the same action on the same app and kind, the resource a fixed step
    /// names, and every argument the step writes as a constant. What a step binds from the
    /// run's inputs, its variables or an earlier step's output is what the saver approved it
    /// to take.
    ///
    /// # Errors
    ///
    /// Why the call is not one the saver approved.
    pub(super) fn covers(
        &self,
        (app, kind, resource, action): Reached<'_>,
        argument: &InvokeArgument,
    ) -> Result<(), String> {
        let run = self.shortcut.action_address();
        if app == tilo_automation::SHORTCUTS_APP_ID
            && run.resource == resource
            && run.action == action
        {
            return Ok(());
        }
        let empty = serde_json::Map::new();
        let arguments = match argument {
            InvokeArgument::None => &empty,
            InvokeArgument::Json(arguments) => arguments,
            InvokeArgument::Text(_) => {
                return Err(format!(
                    "\u{201c}{}\u{201d} calls its steps with named arguments, not text",
                    self.shortcut.name
                ));
            }
        };
        let mut steps = Vec::new();
        invoke_steps(&self.shortcut.steps, &mut steps);
        let approved = steps.iter().any(|(key, selector, args)| {
            key.app == app
                && key.kind == kind
                && key.action == action
                && match selector {
                    Selector::Fixed(fixed) => fixed == resource,
                    Selector::FromBinding(_) | Selector::FirstOfKind(_) => true,
                }
                && arguments_covered(args, arguments)
        });
        if approved {
            Ok(())
        } else {
            Err(format!(
                "\u{201c}{}\u{201d} as its saver approved it has no step that does {action} on {resource} with these arguments",
                self.shortcut.name
            ))
        }
    }
}

/// Every step of `steps` that invokes an action, in branches and loops too.
fn invoke_steps<'a>(
    steps: &'a [Step],
    out: &mut Vec<(
        &'a tilo_actions::ActionKey,
        &'a Selector,
        &'a BTreeMap<String, Binding>,
    )>,
) {
    for step in steps {
        match &step.kind {
            StepKind::Invoke {
                action,
                resource,
                args,
                ..
            } => out.push((action, resource, args)),
            StepKind::If {
                then, otherwise, ..
            } => {
                invoke_steps(then, out);
                invoke_steps(otherwise, out);
            }
            StepKind::ForEach { body, .. } => invoke_steps(body, out),
            StepKind::SetVar { .. }
            | StepKind::Ask { .. }
            | StepKind::Ai { .. }
            | StepKind::Task { .. }
            | StepKind::Read { .. } => {}
        }
    }
}

/// Whether `arguments` are inside what `bindings` approve: no argument the step does not name
/// (beside what the call is made from, which its host checks the caller may read), and every
/// constant the step writes, as written.
fn arguments_covered(
    bindings: &BTreeMap<String, Binding>,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    arguments.iter().all(|(name, value)| {
        name == tilo_actions::params::FROM_ARGUMENT
            || bindings
                .get(name)
                .is_some_and(|binding| value_covered(binding, value))
    }) && bindings.iter().all(|(name, binding)| {
        !matches!(binding, Binding::Literal(_)) || arguments.contains_key(name)
    })
}

fn value_covered(binding: &Binding, value: &serde_json::Value) -> bool {
    match binding {
        Binding::Literal(literal) => literal == value,
        Binding::Object(members) => value.as_object().is_some_and(|object| {
            object.iter().all(|(name, value)| {
                members
                    .get(name)
                    .is_some_and(|member| value_covered(member, value))
            }) && members.iter().all(|(name, member)| {
                !matches!(member, Binding::Literal(_)) || object.contains_key(name)
            })
        }),
        Binding::List(items) => value.as_array().is_some_and(|values| {
            values.len() == items.len()
                && items
                    .iter()
                    .zip(values)
                    .all(|(item, value)| value_covered(item, value))
        }),
        Binding::Input(_) | Binding::Var(_) | Binding::StepOutput { .. } | Binding::Template(_) => {
            true
        }
    }
}

impl AppBrokerState {
    /// The approval `shortcut`, as saved in `workspace`, gives a triggered run of it by
    /// `caller`: its saver's proof, standing now, with `caller` its saver or the agent of
    /// theirs that saved it. Built only from the shortcut as the workspace keeps it.
    ///
    /// # Errors
    ///
    /// The proof does not stand, `caller` is not who saved it, or the saver no longer may.
    pub(super) fn shortcut_approval(
        &self,
        caller: &Caller,
        workspace: &str,
        shortcut: &Shortcut,
    ) -> Result<InvocationCaller, ActionCallError> {
        let time = self
            .registered_policy_time()
            .map_err(ActionCallError::unavailable)?;
        let (_, now) = self
            .checked_policy_time(time.as_ref())
            .map_err(ActionCallError::unavailable)?;
        let revocations = self
            .membership
            .as_ref()
            .and_then(|membership| membership.workspace_revocations(workspace))
            .ok_or(ActionCallError::NotPermitted)?;
        let saver = tilo_peer_state::workspace::proven(workspace, shortcut, &revocations, now)
            .ok_or(ActionCallError::NotPermitted)?;
        let root = hex::decode(&saver.person)
            .ok()
            .and_then(|root| <[u8; 32]>::try_from(root).ok())
            .ok_or(ActionCallError::NotPermitted)?;
        // The key that signed it, as its proof certifies it: an agent's grant beside the
        // agent's name, else the device's own certificate.
        let certificate = shortcut
            .saved_proof
            .as_ref()
            .map(|proof| {
                proof
                    .device_cert
                    .get("grant")
                    .unwrap_or(&proof.device_cert)
                    .clone()
            })
            .and_then(|certificate| {
                tilo_failure::ignored!(
                    "a saved shortcut's proof that does not decode is no proof at all",
                    serde_json::from_value::<tilo_user_identity::DeviceCert>(certificate).ok()
                )
            })
            .filter(|certificate| {
                certificate.user_public_key == root && certificate.device_public_key == saver.key
            })
            .ok_or(ActionCallError::NotPermitted)?;
        let approval = ShortcutApproval {
            workspace: workspace.to_owned(),
            shortcut: shortcut.clone(),
            saver,
            root,
            certificate,
            time,
        };
        self.current_shortcut_approval(
            &approval,
            workspace,
            BrokerActor::for_caller(caller).actor(),
        )
        .map_err(|reason| ActionCallError::Refused { reason })?;
        Ok(InvocationCaller::Shortcut(Box::new(approval)))
    }

    /// Who owns a call `actor` makes in `workspace` under `approval`, as it stands now: the
    /// shortcut is still saved exactly as approved, its proof still stands against the
    /// workspace's revocations, its saver is still a member, and `actor` is who it runs as.
    ///
    /// # Errors
    ///
    /// Which of those no longer stands.
    pub(super) fn current_shortcut_approval(
        &self,
        approval: &ShortcutApproval,
        workspace: &str,
        actor: &tilo_action_policy::Actor,
    ) -> Result<InvocationOwner, String> {
        let (_, now) = self.checked_policy_time(approval.time.as_ref())?;
        let name = &approval.shortcut.name;
        if approval.workspace != workspace {
            return Err(format!(
                "\u{201c}{name}\u{201d} was approved for another workspace"
            ));
        }
        if !approval.runs_as(actor) {
            return Err(format!(
                "\u{201c}{name}\u{201d} runs only as the person who saved it"
            ));
        }
        let membership = self
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?;
        let (current, member) = membership.decide_in_workspace(workspace, |kept| {
            let kept = kept.ok_or("this host keeps no such workspace")?;
            let current = kept
                .shortcuts
                .iter()
                .find(|saved| saved.id() == approval.shortcut.id)
                .and_then(|saved| {
                    tilo_failure::ignored!(
                        "a saved shortcut that does not decode is not the one kept",
                        saved.decode().ok()
                    )
                });
            Ok::<_, String>((current, kept.policy.is_member(&approval.saver.person)))
        })??;
        let unchanged = current.as_ref().is_some_and(|current| {
            current.saved_proof == approval.shortcut.saved_proof
                && current.saved_signing_digest(workspace)
                    == approval.shortcut.saved_signing_digest(workspace)
        });
        if !unchanged {
            return Err(format!(
                "\u{201c}{name}\u{201d} was changed or removed after its saver approved it"
            ));
        }
        if !member {
            return Err(format!(
                "the person who saved \u{201c}{name}\u{201d} is no longer a member of the workspace"
            ));
        }
        let revocations = membership
            .workspace_revocations(workspace)
            .ok_or("this host keeps no revocations of the workspace")?;
        if tilo_peer_state::workspace::proven(workspace, &approval.shortcut, &revocations, now)
            .as_ref()
            != Some(&approval.saver)
        {
            return Err(format!(
                "the approval of \u{201c}{name}\u{201d} no longer stands: the device that signed it was revoked or its certificate lapsed"
            ));
        }
        Ok(approval.owner())
    }

    /// [`Self::current_shortcut_approval`] for a call of `reached` with `argument`, which the
    /// approval also has to cover ([`ShortcutApproval::covers`]).
    pub(super) fn shortcut_call_owner(
        &self,
        approval: &ShortcutApproval,
        workspace: &str,
        actor: &tilo_action_policy::Actor,
        (reached, argument): (Reached<'_>, &InvokeArgument),
    ) -> Result<InvocationOwner, ActionCallError> {
        let owner = self
            .current_shortcut_approval(approval, workspace, actor)
            .map_err(|reason| ActionCallError::Refused { reason })?;
        approval.covers(reached, argument).map_err(|reason| {
            tracing::warn!(%reason, shortcut = approval.shortcut(), "a shortcut's call outside its approval was refused");
            ActionCallError::Refused { reason }
        })?;
        Ok(owner)
    }
}
