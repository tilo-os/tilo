//! A person's immediate look uses their scheduled cursor and budget. Their assistant
//! can also read current findings within the grants of an existing private Chat turn.

use std::collections::BTreeSet;
use std::sync::Mutex;

use tilo_actions::remote::{ActionCallError, InvokeArgument, InvokeRequest};
use tilo_agent_session::bot::host_steps::{HostStepError, Instruction, StepCause};
use tilo_lock::MutexRecover as _;

use super::{Asked, Asker, Initiative, Owner, refused, unavailable};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Occasion {
    Scheduled,
    Requested,
}

/// Releases this person’s scan when the worker returns, including on refusal.
pub(super) struct ActiveLook<'a> {
    owner: &'a Owner,
    active: &'a Mutex<BTreeSet<Owner>>,
}

impl Drop for ActiveLook<'_> {
    fn drop(&mut self) {
        self.active.lock_recover().remove(self.owner);
    }
}

impl Initiative {
    pub(super) fn begin_look<'a>(
        &'a self,
        owner: &'a Owner,
    ) -> Result<ActiveLook<'a>, HostStepError> {
        let active = &self.inner.looking;
        if !active.lock_recover().insert(owner.clone()) {
            return Err(HostStepError::Unavailable(
                "a look is already in progress".into(),
            ));
        }
        Ok(ActiveLook { owner, active })
    }

    pub(super) async fn look_now(
        &self,
        asked: &Asked<'_>,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::workspace_initiative::LookRound, ActionCallError> {
        if !matches!(&request.argument, InvokeArgument::Json(arguments) if arguments.is_empty()) {
            return Err(ActionCallError::InvalidRequest {
                reason: "look-now takes no arguments".into(),
            });
        }
        let owner = asked.owner.clone();
        let person = hex::encode(owner.person);
        if !asked.can_look_now() {
            return Err(ActionCallError::NotPermitted);
        }
        if asked.asker == Asker::Assistant {
            use tilo_actions::remote::workspace_initiative::{Look, LookEnded};
            let kept = self.kept(&owner).map_err(unavailable)?;
            let mut result = self.look_round(asked).await?;
            let now = tilo_clock::unix_now_ms_i64();
            let access = super::look::Access::current(&self.inner.workspaces, &owner)
                .await
                .map_err(refused)?;
            if asked.caller.roster_created_at().is_some()
                && !access.signing.matches_caller(asked.caller)
            {
                return Err(ActionCallError::NotPermitted);
            }
            let gathered = super::look::gather(
                &self.inner.workspaces,
                &owner,
                &access,
                Some(asked.read_as()),
                &kept.spending_event(&owner),
                &kept.seen,
                now,
            )
            .await
            .map_err(unavailable)?;
            access
                .check(&self.inner.workspaces, &owner)
                .await
                .map_err(refused)?;
            // This turn advances no cursor, incurs no model cost and does not
            // add its narrower snapshot to the person's saved observations.
            result.looks = vec![Look {
                id: request.invocation.0.to_string(),
                at_ms: now,
                found: gathered.found,
                ended: LookEnded::Told,
                why: None,
                cost_micros: 0,
            }];
            return Ok(result);
        }
        let instruction = Instruction {
            look_grant: None,
            person,
            workspace: owner.workspace.clone(),
            instruction: "Look at the workspace now and propose concrete tasks for today.".into(),
            allow: Vec::new(),
            permits: Vec::new(),
            note: serde_json::Value::Null,
            cause: StepCause {
                installation: self
                    .kept(&owner)
                    .map_err(unavailable)?
                    .spending_event(&owner),
                invocation: request.invocation,
                event: None,
            },
        };
        let initiative = self.clone();
        tokio::task::spawn_blocking(move || {
            initiative.look(
                &owner,
                (instruction, Occasion::Requested),
                &*initiative.inner.requested_steps,
            )
        })
        .await
        .map_err(|error| unavailable(error.to_string()))?
        .map(drop)
        .map_err(|error| match error {
            HostStepError::Unavailable(why) => unavailable(why),
            HostStepError::Refused(why) => refused(why),
        })?;
        self.look_round(asked).await
    }
}
