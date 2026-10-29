//! The component-by-component hardware report the installer shows before it
//! writes anything.
//!
//! The probe and its report schema are `imager_backend::support_report`,
//! shared with Tilo Imager, so both doors observe a machine the same way and
//! name the same components in the same words. This module owns only what the
//! installer draws: the sentence above the rows (`summary_text`) and
//! the rows themselves (`component_rows`), plus
//! the same facts as data (`component_facts`) for a caller with no screen.
//!
//! The report never decides the install and grants nothing. It is read once,
//! before the plan is written, and shown on Review where the person is
//! already reading what this computer will become. What a running system can
//! establish is narrow: it shows which devices exist and which drivers it
//! bound. It cannot show that the selected Tilo release ships the same
//! kernel, firmware and userspace service for them, so an observed component
//! is never reported as supported — only `imager-backend` marks a component
//! `Supported`, and only with evidence checked against that release.

use imager_backend::support_report::{
    Applicability, ComponentKind, ComponentSupport, ReportSource, SupportReport, SupportState,
};
use ui::tokens::typography::TextRole;
use ui::widget::list_row::ListRow;
use ui::widget::{Text, Widget};

use crate::setup::hardware_text::{
    component_icon, component_title, evidence_text, reason_text, remedy_text,
};
use crate::setup::strings::{t, t_args};

/// What the installer knows about this computer's components: `Checking`
/// until the probe returns, `Unavailable` when the machine cannot be
/// identified specifically enough for the shared report to validate.
#[derive(Clone, Debug, Default)]
pub(crate) enum HardwareReport {
    /// The probe has not returned yet.
    #[default]
    Checking,
    Ready(Box<SupportReport>),
    Unavailable,
}

impl HardwareReport {
    /// Read this computer. Runs on the caller's thread and touches nothing:
    /// the probe reads sysfs and `/proc`, never mounts, opens a block device
    /// for writing or asks the firmware to change anything.
    pub(crate) fn probe() -> Self {
        Self::from_report(SupportReport::probe())
    }

    /// Hold a report read elsewhere, under the same checks the probe's own
    /// reading passes.
    pub(crate) fn from_report(report: SupportReport) -> Self {
        if report.validate().is_err() || report.components.is_empty() {
            return Self::Unavailable;
        }
        Self::Ready(Box::new(report))
    }

    pub(crate) fn report(&self) -> Option<&SupportReport> {
        match self {
            Self::Ready(report) => Some(report),
            Self::Checking | Self::Unavailable => None,
        }
    }

    /// The summary a caller without a screen reads, or `None` before the
    /// probe has returned.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let Some(report) = self.report() else {
            return serde_json::json!({
                "state": match self {
                    Self::Checking => "checking",
                    Self::Unavailable | Self::Ready(_) => "unavailable",
                },
            });
        };
        let summary = report.summary();
        serde_json::json!({
            "state": "ready",
            "model": report.identity.label(),
            "source": match report.identity.source {
                ReportSource::LocalScan => "local-scan",
                ReportSource::Imported => "imported",
                ReportSource::DeclaredModel => "declared-model",
            },
            "kernel": report.kernel,
            "architecture": report.architecture,
            "summary": {
                "supported": summary.supported,
                "limited": summary.limited,
                "unsupported": summary.unsupported,
                "unverified": summary.unverified,
            },
            "blocks_install": report.blocks_install(),
            "components": report
                .components
                .iter()
                .map(component_facts)
                .collect::<Vec<_>>(),
        })
    }
}

/// The report's headline: what the probe established, in one sentence, and
/// what it does not.
pub(crate) fn summary_text(report: &SupportReport) -> String {
    let summary = report.summary();
    let counts: &[(&str, ui::i18n::ArgValue)] = &[
        ("supported", summary.supported.into()),
        ("limited", summary.limited.into()),
        ("unsupported", summary.unsupported.into()),
        ("unverified", summary.unverified.into()),
    ];
    let mut text = if summary.blocking > 0 {
        t_args("installer-hardware-summary-blocking", counts)
    } else {
        t_args("installer-hardware-summary", counts)
    };
    if report
        .components
        .iter()
        .any(|component| component.applicability != Applicability::ReleaseVerified)
    {
        text.push(' ');
        text.push_str(&t("installer-hardware-summary-observation"));
    }
    text
}

/// The headline row: the sentence above, or the state the probe is still in.
pub(crate) fn summary_row(report: &HardwareReport) -> Box<dyn Widget> {
    match report {
        HardwareReport::Checking => Box::new(
            Text::new(&t("installer-hardware-checking"))
                .with_id("installer.hardware.checking")
                .with_role(TextRole::Caption)
                .with_live(ui::a11y::Politeness::Polite),
        ),
        HardwareReport::Unavailable => Box::new(
            Text::new(&t("installer-hardware-unavailable"))
                .with_id("installer.hardware.unavailable")
                .with_role(TextRole::Caption),
        ),
        HardwareReport::Ready(report) => Box::new(
            Text::new(&summary_text(report))
                .with_id("installer.hardware.summary")
                .with_role(TextRole::Body)
                .with_live(ui::a11y::Politeness::Polite),
        ),
    }
}

/// Every component the probe covered, essential classes first: a person
/// reads what the desktop needs (graphics, display, startup, storage) before
/// what is nice to have (a fingerprint reader).
pub(crate) fn component_rows(report: &SupportReport) -> Vec<Box<dyn Widget>> {
    let mut components = report.components.iter().collect::<Vec<_>>();
    components.sort_by_key(|component| (order(component.kind), component.kind.key()));
    components.into_iter().map(component_row).collect()
}

/// The order rows appear in: the four classes a usable desktop needs, then
/// the rest in the order the report names them.
fn order(kind: ComponentKind) -> u8 {
    match kind {
        ComponentKind::Boot => 0,
        ComponentKind::Storage => 1,
        ComponentKind::Graphics => 2,
        ComponentKind::Display => 3,
        ComponentKind::Input => 4,
        ComponentKind::Power => 5,
        ComponentKind::Wifi => 6,
        ComponentKind::Ethernet => 7,
        ComponentKind::Audio => 8,
        ComponentKind::Microphone => 9,
        ComponentKind::Bluetooth => 10,
        ComponentKind::Camera => 11,
        ComponentKind::Fingerprint => 12,
        ComponentKind::Acpi => 13,
    }
}

fn component_row(component: &ComponentSupport) -> Box<dyn Widget> {
    let key = component.kind.key();
    let mut row = ListRow::new(
        format!("installer.hardware.{key}"),
        component_title(component.kind),
    )
    .icon(component_icon(component.kind))
    .secondary(reason_text(component))
    .two_line_secondary();
    if !component.name.trim().is_empty() {
        row = row.tooltip(component.name.clone());
    }
    row = match component.state {
        SupportState::Supported => row,
        SupportState::Limited => row.warning(),
        SupportState::Unsupported => row.error(),
        SupportState::Unverified => row.idle(),
    };
    let mut lines: Vec<Box<dyn Widget>> = vec![Box::new(row)];
    if let Some(remedy) = component.remedy {
        lines.push(Box::new(
            Text::new(&remedy_text(component.kind, remedy))
                .with_id(format!("installer.hardware.{key}.next"))
                .with_role(TextRole::Caption),
        ));
    }
    if component.applicability != Applicability::ReleaseVerified
        && component.state != SupportState::Supported
    {
        lines.push(Box::new(
            Text::new(&t("installer-hardware-scope-observation"))
                .with_id(format!("installer.hardware.{key}.scope"))
                .with_role(TextRole::Caption),
        ));
    }
    if lines.len() == 1 {
        return lines.remove(0);
    }
    let mut detail = ui::widget::Column::new()
        .with_id(format!("installer.hardware.{key}.detail"))
        .with_intrinsic_height()
        .with_gap(0.0);
    for line in lines {
        detail = detail.push(line);
    }
    Box::new(detail)
}

/// One component as data: the same class, state, reason, evidence and next
/// step the row draws, in the words the row shows.
pub(crate) fn component_facts(component: &ComponentSupport) -> serde_json::Value {
    serde_json::json!({
        "component": component.kind.key(),
        "name": component.name,
        "present": component.present,
        "state": component.state.key(),
        "applicability": match component.applicability {
            Applicability::ReleaseVerified => "release-verified",
            Applicability::ObservationOnly => "observation-only",
            Applicability::Unknown => "unknown",
        },
        "reason": reason_text(component),
        "evidence": component.evidence.iter().map(evidence_text).collect::<Vec<_>>(),
        "next": component.remedy.map(|remedy| remedy_text(component.kind, remedy)),
        "blocks_install": component.blocks_install(),
    })
}

#[cfg(test)]
mod tests;
