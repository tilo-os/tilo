//! Deleting a workspace: one of the person's own, alone and held by no other device of theirs, so
//! deleting it here deletes it with everything in it.
//! The person answers a question first, wherever deleting was asked for (the workspace menu, the
//! workspace settings, an agent's action through the shell): the session asks it as an asked record
//! ([`crate::program_asks`]), which the shell's asking view draws like every other question, so
//! nothing of the workspace goes until the person says yes there. Only their yes takes the
//! workspace off the shell, which is what the person's list follows; this device's daemon then
//! deletes it where it is kept, and it goes out of this device's record.
//! A workspace its owner deleted somewhere else leaves this device the same way, and the person is
//! told who deleted it ([`Shared::follow_owner_deletions`]).

use std::future::Future as _;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_desktop_model::WorkspaceId;
use tilo_service::Question;

use crate::Shared;
use crate::engine_write::Polled;

/// The question's kind, as the person is asked it ([`crate::program_asks`]).
pub(crate) const KIND: &str = "workspace.delete";

/// What the question shows: the workspace, by the name the person knows it by.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DeleteQuestion {
    pub(crate) name: String,
}

impl Shared {
    /// Ask the person whether to delete the workspace `workspace`, called `name`: the question
    /// stands over what they are looking at, and nothing of the workspace goes until they say yes.
    /// Asking again about the same workspace replaces the question that stands.
    pub(crate) fn ask_to_delete_workspace(&self, workspace: WorkspaceId, name: &str) {
        let asked = self.asker.ask(Question {
            handle: format!("{KIND}:{workspace}"),
            kind: KIND.to_owned(),
            app_id: None,
            label: name.to_owned(),
            details: serde_json::to_value(DeleteQuestion {
                name: name.to_owned(),
            })
            .unwrap_or_default(),
        });
        *self.delete_workspace_ask.borrow_mut() = Some((asked, workspace));
    }

    /// Ask the shell to delete the workspace in front: the shell asks the person first, and only
    /// their answer deletes it.
    pub(crate) fn ask_shell_to_delete(&self) {
        if let Err(refusal) = self.ask_of_shell("delete-workspace", &Map::new()) {
            self.handle.acknowledge(refusal);
        }
    }

    /// Carry out the person's answer to the delete question: on yes the shell deletes the
    /// workspace, which is the only thing that deletes; declining it deletes nothing at all.
    pub(crate) fn follow_delete_workspace(&self) {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut waiting = self.delete_workspace_ask.borrow_mut();
        let Some((asked, workspace)) = waiting.as_mut() else {
            return;
        };
        let std::task::Poll::Ready(answer) = std::pin::Pin::new(asked).poll(&mut context) else {
            return;
        };
        let workspace = *workspace;
        waiting.take();
        drop(waiting);
        if answer.is_some() {
            self.delete_workspace(workspace);
        }
    }

    /// Delete the workspace `workspace` through the shell's own published action,
    /// as the person's answer to the question: what may be deleted, and the
    /// removal, are the shell's, and so is telling the session it happened
    /// ([`Self::deleted_workspace`]).
    fn delete_workspace(&self, workspace: WorkspaceId) {
        let args = Map::from_iter([
            ("workspace".to_owned(), Value::from(workspace.to_string())),
            // The person said yes to the question deleting asks.
            ("confirmed".to_owned(), Value::from(true)),
        ]);
        // Off the shell first: a shell that refused leaves the workspace whole,
        // so nothing of it is gone while it still stands.
        if let Err(refusal) = self.ask_of_shell("delete-workspace", &args) {
            self.handle.acknowledge(refusal);
        }
    }

    /// The shell took the workspace `workspace` , called `name` , off as deleted: this device's
    /// daemon deletes it where it is kept (`account/workspaces#delete`, the workspace service's
    /// `workspace.delete`), and its own copy with it. The shell
    /// asks this of whoever deleted it — the menu, the settings, an agent invoking the action — so
    /// a workspace deleted anywhere is deleted everywhere it was this device's.
    pub(crate) fn deleted_workspace(&self, workspace: WorkspaceId, name: &str) {
        // A question still standing about it is answered: dropping it takes it back.
        self.delete_workspace_ask.borrow_mut().take();
        let asked = self.view.as_ref().map(|view| {
            view.ask_account(
                "account/workspaces",
                "delete",
                Map::from_iter([("workspace".to_owned(), Value::from(workspace.to_string()))]),
            )
        });
        let deleted = Polled::new(
            Arc::clone(&self.wake_handle),
            async move {
                match asked {
                    Some(asked) => asked.await.map(drop),
                    None => Err("this session reaches no device daemon".to_owned()),
                }
            },
            || Err("the deletion stopped before it answered".to_owned()),
        );
        self.deletions
            .borrow_mut()
            .push((workspace, name.to_owned(), deleted));
    }

    /// Tell the person how each deletion the daemon answered went.
    pub(crate) fn follow_deletions(&self) {
        let mut answered = Vec::new();
        self.deletions
            .borrow_mut()
            .retain_mut(|(workspace, name, deleted)| match deleted.done() {
                Some(done) => {
                    answered.push((*workspace, std::mem::take(name), done));
                    false
                }
                None => true,
            });
        for (workspace, name, done) in answered {
            match done {
                Ok(()) => crate::directory::workspace_notice(
                    &*self.telling(),
                    (
                        "session-deleted-workspace",
                        Some("session-deleted-workspace-body"),
                    ),
                    &name,
                ),
                Err(refusal) => {
                    tracing::warn!(%workspace, %refusal, "a workspace was not deleted where it is kept");
                    self.handle.acknowledge(refusal);
                }
            }
        }
    }

    /// Take each workspace whose host said its owner deleted it off this session and out of this
    /// device's record, and tell the person: someone else deleted it, so it is news to them.
    pub(crate) fn follow_owner_deletions(&self) {
        let Some(sync) = &self.sync else {
            return;
        };
        for workspace in sync.take_deleted() {
            tracing::info!(%workspace, "a workspace its owner deleted leaves this device");
            let name = self.handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|held| held.id() == workspace)
                    .map(|held| held.name.clone())
            });
            // Told once: a word the host repeats finds nothing left to take away.
            let gone = self.take_off_shell(workspace);
            if gone && let Some(name) = name {
                crate::directory::workspace_notice(
                    &*self.telling(),
                    (
                        "session-owner-deleted-workspace",
                        Some("session-owner-deleted-workspace-body"),
                    ),
                    &name,
                );
            }
        }
    }

    /// Take `workspace`, which is gone already, off the shell; whether it was taken. Nobody is asked
    /// and nobody refuses: the person goes to the workspace beside it when they were in it. The
    /// shell is never left with none: where it was the only one, a new empty workspace takes its
    /// place first, as it does when the person deletes their last workspace here.
    fn take_off_shell(&self, workspace: WorkspaceId) -> bool {
        let only = |tree: &tilo_desktop_model::Layout| matches!(tree.workspaces.as_slice(), [only] if only.id() == workspace);
        if self.handle.read(only)
            && let Err(error) = self.ask_of_shell("new-workspace", &Map::new())
        {
            tracing::warn!(%error, %workspace, "a deleted workspace stays on the shell, which has no other");
            return false;
        }
        let mut tree = self.handle.read(Clone::clone);
        let Some(at) = tree
            .workspaces
            .iter()
            .position(|held| held.id() == workspace)
        else {
            return false;
        };
        if tree.workspaces.len() == 1 {
            return false;
        }
        let gone = tree.workspaces.remove(at);
        if gone.id() == tree.active
            && let Some(next) = tree
                .workspaces
                .get(at.min(tree.workspaces.len().saturating_sub(1)))
        {
            tree.active = next.id();
        }
        tree.active_strip.remove(&gone.id());
        for strip in &gone.strips {
            tree.active_group.remove(&strip.id);
        }
        self.handle.replace_tree(tree);
        true
    }
}
