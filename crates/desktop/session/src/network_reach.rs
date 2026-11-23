//! What an app the person installed may reach, asked before it first starts in a workspace.
//!
//! The launcher lets an app reach only what its manifest declares and a network grant in the
//! workspace allows; the image's own apps reach what they declare
//! ([`launcher::egress_proxy::EgressDecision::resolve`]). So the first start of an installed app
//! whose network nobody in the workspace answered is held while the person is asked
//! ([`crate::apps::Apps::lend_questions`]). Allowing writes a network grant for each host it declares,
//! kept by the workspace, which every member sees and can revoke; the app starts either way.

use std::cell::Cell;
use std::rc::Rc;

use collab_types::caps::{Access, CapOverride, Scope};
use serde::{Deserialize, Serialize};
use tilo_action_policy::Grantee;
use tilo_desktop_model::WorkspaceId;
use tilo_service::{Asked, Question, Questions};

use crate::apps::ReachFirst;

/// The question's kind, which the portal sheet shows ([`crate::questions`]).
pub(crate) const KIND: &str = "app.network";

/// What the question shows: the hosts the app declares, `*` for any.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ReachQuestion {
    pub(crate) hosts: Vec<String>,
}

/// Ask the person whether the app `first` starts may reach what it declares.
pub(crate) fn ask(questions: &Questions, first: &ReachFirst) -> Asked {
    questions.ask(Question {
        handle: format!("{KIND}:{}:{}", first.workspace, first.app),
        kind: KIND.to_owned(),
        app_id: Some(first.app.clone()),
        label: crate::app_names::claimant(&first.app),
        details: serde_json::to_value(ReachQuestion {
            hosts: first.hosts.clone(),
        })
        .unwrap_or_default(),
    })
}

/// The grants allowing `first` 's app what it declares, as `person` gives them.
fn grants(first: &ReachFirst, person: &str) -> Vec<CapOverride> {
    first
        .hosts
        .iter()
        .map(|host| {
            CapOverride::grant(
                Grantee::person(person.to_owned()),
                Scope::NetHost(host.clone()),
                Access::Read,
            )
            .through(first.app.as_str())
        })
        .collect()
}

impl crate::Shared {
    /// Start every app whose question the person answered since the last pass: with the
    /// workspace's grants for what it declares once they stand, when the person allowed it.
    pub(crate) fn follow_reach(&self) {
        let Some(apps) = &self.launch else {
            return;
        };
        for (answer, first) in apps.answered_reach() {
            let Some(workspace) = answer.and_then(|_| WorkspaceId::parse(&first.workspace)) else {
                apps.start_reached(&first);
                continue;
            };
            let grants = grants(&first, &self.person);
            let held = Rc::new(Cell::new(Some(first)));
            let start = {
                let (apps, held) = (Rc::clone(apps), Rc::clone(&held));
                move || {
                    if let Some(first) = held.take() {
                        apps.start_reached(&first);
                    }
                }
            };
            let at = crate::governance::Governing {
                handle: &self.handle,
                person: &self.person,
                view: self.view.as_ref(),
                landings: &self.landings,
            };
            let landed = {
                let start = start.clone();
                move |landed: Result<(), String>| {
                    if let Err(reason) = landed {
                        tracing::warn!(%reason, "the workspace refused an app's network grant");
                    }
                    start();
                }
            };
            let edit = move |workspace: &mut tilo_desktop_model::Workspace, actor: &String| {
                let now = tilo_clock::unix_now();
                grants.iter().try_fold(
                    tilo_session_core::desktop::workspace_settings::PolicyEdit::default(),
                    |edit, cap| {
                        Ok(edit.and(workspace.grant_sandbox_as_at(actor, cap.clone(), now)?))
                    },
                )
            };
            // Refused at once, or nothing to write: the app starts as the workspace stands.
            match crate::governance::govern(&at, workspace, edit, landed) {
                Ok(true) => {}
                Ok(false) => start(),
                Err(refusal) => {
                    tracing::warn!(%refusal, "the person may not grant an app network here");
                    start();
                }
            }
        }
    }
}
