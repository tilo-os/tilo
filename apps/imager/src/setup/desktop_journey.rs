//! Device-local Desktop projection. No paths or secrets.
use super::actions::SetupEffect;
use super::desktop_flow::{DesktopFlow, DesktopFlowState};
use tilo_actions::ResourcePresentation;
use ui::framework::LiveRow;
use ui::tilo_resource::Effect;

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

pub fn commands(d: &DesktopProjection) -> [(&'static str, &'static str, bool); 3] {
    [
        (
            SetupEffect::INSTALL_DESKTOP.id,
            "imager-setup-desktop-install",
            d.actions.install,
        ),
        (
            SetupEffect::CANCEL_DESKTOP_DOWNLOAD.id,
            "imager-setup-desktop-cancel",
            d.actions.cancel,
        ),
        (
            SetupEffect::RECHECK_DESKTOP.id,
            "imager-setup-desktop-recheck",
            d.actions.recheck,
        ),
    ]
}
pub fn row(d: &DesktopProjection) -> LiveRow {
    let mut row = LiveRow::new(
        "imager.setup.desktop",
        "imager.setup.desktop",
        super::strings::t("imager-setup-desktop-heading"),
    )
    .presentation(ResourcePresentation {
        detail: Some(d.phase.into()),
        badge: None,
        preview: None,
    });
    for (id, label, enabled) in commands(d) {
        let effect = SetupEffect::decode(id, &serde_json::Map::new())
            .expect("declared parameter-free effect");
        let mut spec = effect.bound();
        spec.type_mut().label = super::strings::t(label);
        if !enabled {
            spec = spec.unavailable(
                "desktop_command_unavailable",
                super::strings::t("imager-setup-desktop-command-unavailable"),
            );
        }
        row = row.operation(spec);
    }
    row
}
