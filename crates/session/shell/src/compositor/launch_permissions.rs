//! Permissions the app sandbox applies when an app starts, asked before it
//! starts.
//!
//! Most grants are brokered while the app runs: it asks, the portal prompts,
//! and the answer reaches it at once. `io-uring` cannot work that way. The
//! launcher writes the app's syscall filter before the app runs, and an app
//! whose `io_uring_setup` fails just falls back to `epoll` without asking
//! anyone. So the person is asked here, on the launch worker, when a
//! user-installed app that declares `io-uring` starts in a room that has
//! neither granted nor refused it. "Allow" lasts for this launch; "This
//! session" and "Always" become the room's grant, which the Permissions tab
//! shows and revokes like any other. An app the image ships is granted what
//! it declares by the launcher and is never asked.

use std::thread::ThreadId;

use collab_types::caps::{Access, CapOverride, LaunchGrants, Scope, Subject};
use tilo_desktop_model::capability::{Capability, GRANT_ONCE};

use crate::portal::{GrantStoreHandle, PromptOutcome, PromptQueueHandle};
use crate::shell::launcher::InstalledApp;
use crate::shell::state::RoomId;

/// The permission prompt, as the launch path asks it.
pub struct LaunchPermissions {
    pub grants: GrantStoreHandle,
    pub prompts: PromptQueueHandle,
    /// Wakes the compositor loop to show a queued prompt.
    pub wake: crate::session_runtime::Wake,
    /// The compositor's own thread, which shows the prompt and so must
    /// never wait for its answer.
    pub compositor: ThreadId,
}

/// Whether a launch has to ask for `io-uring`: the app declares it, the
/// image does not ship it, and the room's capabilities for it say nothing
/// either way (a grant is applied by the launcher, a restriction is the
/// room's answer already).
pub(crate) fn needs_io_uring_prompt(
    declared: bool,
    image_shipped: bool,
    launch: &LaunchGrants,
) -> bool {
    let device = Scope::Device(launcher::seccomp::IO_URING.to_string());
    declared && !image_shipped && !launch.sandbox.iter().any(|cap| cap.scope == device)
}

/// The capability one answer adds to this launch.
pub(crate) fn io_uring_launch_grant(app_id: &str, outcome: PromptOutcome) -> Option<CapOverride> {
    let scope = outcome.to_scope()?;
    Some(
        CapOverride::grant(
            Subject::app(app_id),
            Scope::Device(launcher::seccomp::IO_URING.to_string()),
            Access::Read,
        )
        .expiring(scope),
    )
}

fn declares_io_uring(app: &InstalledApp) -> bool {
    std::fs::read_to_string(&app.manifest_path)
        .ok()
        .and_then(|source| launcher::manifest::ManifestView::from_toml_str(&source).ok())
        .is_some_and(|manifest| manifest.declares_capability(launcher::seccomp::IO_URING))
}

impl LaunchPermissions {
    /// What to add to `launch` for `app` in `room` once the person answered.
    pub fn ask(&self, app: &InstalledApp, room: RoomId, launch: &LaunchGrants) -> Vec<CapOverride> {
        if !needs_io_uring_prompt(
            declares_io_uring(app),
            launcher::system_stats::image_shipped(&app.payload_root),
            launch,
        ) {
            return Vec::new();
        }
        if std::thread::current().id() == self.compositor {
            // Waiting here would hold the loop that shows the prompt. A
            // launch on the compositor thread starts without io_uring.
            tracing::info!(app = %app.package_id, "io-uring not asked on the compositor thread");
            return Vec::new();
        }
        let Some(caller) = self
            .grants
            .caller_in_room(&room.to_string(), &app.package_id)
        else {
            return Vec::new();
        };
        let answer = self
            .prompts
            .enqueue(caller.clone(), Capability::IoUring { scope: GRANT_ONCE });
        (self.wake)();
        // A prompt that is withdrawn answers no.
        let outcome = pollster::block_on(answer).unwrap_or(PromptOutcome::Deny);
        if let Some(scope) = outcome.to_scope()
            && scope != GRANT_ONCE
        {
            self.grants.insert(&caller, &Capability::IoUring { scope });
        }
        io_uring_launch_grant(&app.package_id, outcome)
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{Effect, Expiry};

    #[test]
    fn io_uring_is_asked_only_for_a_declared_undecided_user_installed_app() {
        let empty = LaunchGrants::default();
        assert!(needs_io_uring_prompt(true, false, &empty));
        assert!(!needs_io_uring_prompt(false, false, &empty), "undeclared");
        assert!(
            !needs_io_uring_prompt(true, true, &empty),
            "the image ships it"
        );

        let granted = LaunchGrants {
            sandbox: vec![
                io_uring_launch_grant("com.example.db", PromptOutcome::AllowAlways).unwrap(),
            ],
            ..LaunchGrants::default()
        };
        assert!(
            !needs_io_uring_prompt(true, false, &granted),
            "already granted"
        );
        let refused = LaunchGrants {
            sandbox: vec![CapOverride::new(
                Subject::app("com.example.db"),
                Scope::Device("io-uring".into()),
                Effect::Restrict,
                Access::Read,
            )],
            ..LaunchGrants::default()
        };
        assert!(
            !needs_io_uring_prompt(true, false, &refused),
            "the room refused it"
        );
    }

    #[test]
    fn an_answer_becomes_this_launchs_grant_with_its_lifetime() {
        assert!(io_uring_launch_grant("com.example.db", PromptOutcome::Deny).is_none());
        let once = io_uring_launch_grant("com.example.db", PromptOutcome::AllowOnce).unwrap();
        assert_eq!(once.scope, Scope::Device("io-uring".into()));
        assert_eq!(once.subject, Subject::app("com.example.db"));
        assert_eq!(once.expiry, Expiry::AfterUse);
        let always = io_uring_launch_grant("com.example.db", PromptOutcome::AllowAlways).unwrap();
        assert_eq!(always.expiry, Expiry::Never);
    }
}
