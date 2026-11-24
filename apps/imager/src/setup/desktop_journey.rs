//! Device-local Desktop projection. No paths or secrets.
use super::actions::SetupEffect;
use super::desktop_flow::{DesktopFlow, DesktopFlowState};
use tilo_actions::ResourcePresentation;
use tilo_resource::{self, ActionRef, RowBindings};
use tilo_resource::{EffectStatus, LiveRow};

/// The Desktop preparation journey, as guided setup publishes it: a row
/// offering to install, cancel or recheck what the projection says.
#[derive(tilo_resource::Row)]
#[row(kind = "imager.setup.desktop", label = "Desktop", effects = SetupEffect)]
pub(crate) struct DesktopJourney;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct DesktopProjection {
    /// Progress is an observed phase; the helper does not publish byte counts.
    pub phase: &'static str,
    pub release_id: Option<String>,
    pub refusal: Vec<String>,
    pub actions: DesktopActions,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct DesktopActions {
    pub install: bool,
    pub cancel: bool,
    pub recheck: bool,
}
pub fn project(flow: &DesktopFlow, reviewed: bool) -> DesktopProjection {
    use DesktopFlowState as S;
    let phase = match flow.state() {
        S::Checking => "checking",
        S::Unavailable { .. } => "unavailable",
        S::Ready(_) => "ready",
        S::Downloading(_) => "downloading",
        S::Authorizing(_) => "authorizing",
        S::Preparing(_) => "preparing",
        S::Installing(_) => "installing",
        S::Verifying(_) => "verifying",
        S::Installed { .. } => "installed",
        S::Failed { .. } => "failed",
    };
    let refusal = match flow.state() {
        S::Unavailable { reasons, .. } => reasons.clone(),
        S::Failed { message, .. } => vec![message.clone()],
        _ => Vec::new(),
    };
    DesktopProjection {
        phase,
        refusal,
        release_id: flow.selection().map(|s| s.release.id.clone()),
        actions: DesktopActions {
            install: reviewed && matches!(flow.state(), S::Ready(_)),
            cancel: matches!(flow.state(), S::Downloading(_)),
            recheck: !flow.has_pending_operation()
                && matches!(
                    flow.state(),
                    S::Ready(_) | S::Unavailable { .. } | S::Failed { .. }
                ),
        },
    }
}

pub fn commands(d: &DesktopProjection) -> [(ActionRef<SetupEffect, ()>, &'static str, bool); 3] {
    [
        (
            SetupEffect::INSTALL_DESKTOP,
            "imager-setup-desktop-install",
            d.actions.install,
        ),
        (
            SetupEffect::CANCEL_DESKTOP_DOWNLOAD,
            "imager-setup-desktop-cancel",
            d.actions.cancel,
        ),
        (
            SetupEffect::RECHECK_DESKTOP,
            "imager-setup-desktop-recheck",
            d.actions.recheck,
        ),
    ]
}

/// The commands this Desktop journey cannot start now, by the row's effects,
/// with the reason a person reads.
pub fn row_status(d: &DesktopProjection) -> Vec<EffectStatus> {
    commands(d)
        .into_iter()
        .filter(|(_, _, open)| !open)
        .map(|(command, _, _)| {
            EffectStatus::unavailable(
                command,
                "desktop_command_unavailable",
                super::strings::t("imager-setup-desktop-command-unavailable"),
            )
        })
        .collect()
}

pub fn row(d: &DesktopProjection) -> LiveRow {
    let mut bindings = RowBindings::new();
    for (command, _, _) in commands(d) {
        bindings = bindings.offer(command);
    }
    let mut row = LiveRow::typed_with::<DesktopJourney>(
        "imager.setup.desktop",
        super::strings::t("imager-setup-desktop-heading"),
        &bindings,
    )
    .presentation(ResourcePresentation {
        detail: Some(d.phase.into()),
        badge: None,
        preview: None,
    });
    for (command, label, _) in commands(d) {
        row = row.labelled(command, super::strings::t(label));
    }
    row
}
