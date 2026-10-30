//! Launch failure copy reads whole for every reason.

use super::*;

/// Every launch failure the person can be told reads whole: a message that
/// names the app gets its name, none shows a `{ $… }` placeable.
#[test]
fn every_launch_failure_message_is_filled() {
    // Exhaustive, so a new reason is listed here or this stops compiling.
    const fn listed(reason: LaunchFailure) {
        match reason {
            LaunchFailure::LauncherUnavailable
            | LaunchFailure::ProviderUnavailable
            | LaunchFailure::NotInstalled
            | LaunchFailure::NotAGuiApp
            | LaunchFailure::NotAManagedService
            | LaunchFailure::NotAuthorized
            | LaunchFailure::NoDisplay
            | LaunchFailure::NoWorkspace
            | LaunchFailure::ShellOwnedIntent
            | LaunchFailure::IdentityUnavailable
            | LaunchFailure::AppNotInWorkspace
            | LaunchFailure::DeviceNotAdmitted
            | LaunchFailure::AppAuthorshipRefused
            | LaunchFailure::RegistryUnreadable
            | LaunchFailure::HostAppRefused
            | LaunchFailure::AuthorityChanged
            | LaunchFailure::AuthorityTimedOut
            | LaunchFailure::SandboxSupervision
            | LaunchFailure::PreviousInstanceBusy
            | LaunchFailure::UpdateFailed
            | LaunchFailure::WaitingForDeveloper
            | LaunchFailure::SystemBehind
            | LaunchFailure::RoomRefused
            | LaunchFailure::Other => {}
        }
    }
    let reasons = [
        LaunchFailure::LauncherUnavailable,
        LaunchFailure::ProviderUnavailable,
        LaunchFailure::NotInstalled,
        LaunchFailure::NotAGuiApp,
        LaunchFailure::NotAManagedService,
        LaunchFailure::NotAuthorized,
        LaunchFailure::NoDisplay,
        LaunchFailure::NoWorkspace,
        LaunchFailure::ShellOwnedIntent,
        LaunchFailure::IdentityUnavailable,
        LaunchFailure::AppNotInWorkspace,
        LaunchFailure::DeviceNotAdmitted,
        LaunchFailure::AppAuthorshipRefused,
        LaunchFailure::RegistryUnreadable,
        LaunchFailure::HostAppRefused,
        LaunchFailure::AuthorityChanged,
        LaunchFailure::AuthorityTimedOut,
        LaunchFailure::SandboxSupervision,
        LaunchFailure::PreviousInstanceBusy,
        LaunchFailure::UpdateFailed,
        LaunchFailure::WaitingForDeveloper,
        LaunchFailure::SystemBehind,
        LaunchFailure::Other,
    ];
    let mut bodies = Vec::new();
    for reason in reasons {
        listed(reason);
        bodies.push(
            launch_event(LaunchEventKind::SpawnFailed {
                reason,
                detail: String::new(),
            })
            .failure_notice()
            .unwrap()
            .body,
        );
    }
    for stderr_tail in [
        "built for a different Tilo runtime",
        "error while loading shared libraries",
        "bwrap: x",
        ui::collaboration::ROUTE_JOIN_FAILED,
        ui::app::WINDOW_LOST,
        "",
    ] {
        for code in [None, Some(1)] {
            bodies.push(
                launch_event(LaunchEventKind::Failed {
                    code,
                    stderr_tail: stderr_tail.into(),
                })
                .failure_notice()
                .unwrap()
                .body,
            );
        }
    }
    for body in bodies {
        assert!(!body.contains("{"), "an unfilled message: {body}");
    }
    let refused = launch_event(LaunchEventKind::SpawnFailed {
        reason: LaunchFailure::AppAuthorshipRefused,
        detail: String::new(),
    })
    .failure_notice()
    .unwrap();
    assert_eq!(
        refused.body,
        "This workspace's host did not let you add Broken App."
    );
}
