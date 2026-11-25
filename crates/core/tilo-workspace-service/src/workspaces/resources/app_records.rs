//! An app's own records, kept by the grant installing it gave it
//! ([`tilo_action_policy::roles::app_records_grant`]): when a write the app made by itself (a
//! trigger, a migration) is refused because that grant was revoked, the refusal is said in the
//! words a person reads, so the app tells them it can't keep its records rather than failing
//! where nobody looks.

use collab_types::store::wire::Command;
use tilo_action_policy::{ActionKind, Answer, Authority, Context, TreePath};
use tilo_resource_store::Stamp;

use super::super::Workspaces;

/// The message a refused write of an app's own records is said in.
const TAKEN_BACK: &str = "app-records-taken-back";

/// Whether `stamp` names `app` itself as the one who made the write: the app acting by itself,
/// with no person at it.
pub(super) fn by_app(stamp: &Stamp, app: &str) -> bool {
    matches!(stamp.invoked_by(), Some(tilo_actions::Actor::App(by)) if by.as_str() == app)
}

/// Whether any write of `command` is made by `app` itself ([`by_app`]).
pub(super) fn written_by_app(command: &Command, app: &str) -> bool {
    match command {
        Command::Put { stamp, .. }
        | Command::Remove { stamp, .. }
        | Command::Edit { stamp, .. }
        | Command::Checkpoint { stamp, .. }
        | Command::Undo { stamp, .. }
        | Command::Restore { stamp, .. } => by_app(stamp, app),
        Command::Batch(commands) => commands.iter().any(|command| written_by_app(command, app)),
        _ => false,
    }
}

/// An app's name as a person reads it: the last segment of its id, in sentence case.
fn app_name(app: &str) -> String {
    tilo_actions::field_type::label_of(app.rsplit('.').next().unwrap_or(app))
}

impl Workspaces {
    /// What to say of `error`, the workspace's refusal of a write `app` made by itself in
    /// `workspace`: when the app may not change its own records there, as the workspace's grants
    /// naming it decide (the same decision the write's admission made), that it can't keep its
    /// records and how a member gives that back; otherwise `error` as it is.
    pub(super) async fn app_write_refused(
        &self,
        workspace: &str,
        app: &str,
        error: String,
    ) -> String {
        let Ok(state) = self.workspace(workspace).await else {
            return error;
        };
        let policy = {
            let governed = state.lock().await;
            match governed.peer().governance().governed_policy() {
                Ok(policy) => policy,
                Err(_) => return error,
            }
        };
        let acting = tilo_actions::Acting::by_app(app);
        let keeps = Authority::of_acting(&acting, &policy, &[])
            .decide(
                &TreePath::workspace(workspace).app(app),
                ActionKind::Undoable,
                &Context::at_time(tilo_clock::wall()),
            )
            .answer
            == Answer::Allow;
        if keeps {
            return error;
        }
        let mut args = tilo_history::FluentArgs::new();
        args.set("app", app_name(app));
        crate::i18n::message(None, TAKEN_BACK, &args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_names_the_app_and_how_to_give_it_back() {
        let mut args = tilo_history::FluentArgs::new();
        args.set("app", app_name("so.tilo.clocks"));
        let said = crate::i18n::message(None, TAKEN_BACK, &args);
        assert!(said.contains("Clocks"), "{said}");
        assert!(said.contains("Permissions"), "{said}");
        let german = crate::i18n::message(Some("de-DE"), TAKEN_BACK, &args);
        assert_ne!(german, said, "the message is translated");
        assert!(german.contains("Clocks"), "{german}");
    }
}
