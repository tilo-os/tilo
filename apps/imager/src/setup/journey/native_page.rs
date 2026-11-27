//! What the setup pages show: the Desktop install, the check of this
//! computer, and the Full Tilo install with its download, plan and blockers.

use hardware_report::SupportReport;
use tilo_ui_base::a11y::Politeness;
use ui::widget::{Banner, Column, ControlSize, Section, SectionHeader};

use super::native::{Handoff, NativeSetup};
use super::*;
use crate::resource::CATALOG;
use crate::setup::desktop_flow::{DesktopFailureKind, DesktopFlowState};
use crate::setup::internal_install_flow::{self as install_flow, HostCommand, RunState};
use crate::setup::staging::helper::Action as HelperAction;
use crate::setup::support_report_view::{self, Placement};
use tilo_ui_base::i18n::ArgValue;

/// A button of a setup page that presses `effect`, as every caller invokes it.
fn press(commands: &Commands, id: &str, key: &str, effect: InstallerPreviewEffect) -> Button {
    let commands = commands.clone();
    Button::new(id, &t(key))
        .control_size(ControlSize::Regular)
        .on_click(move || commands.borrow_mut().push(effect.clone()))
}

impl InstallerPreview {
    /// The page the review handed over to, with the way back the person
    /// takes once nothing it started is still running.
    pub(super) fn native_page(&self, native: &NativeSetup) -> Box<dyn Widget> {
        let back = self
            .button(
                "installer.back",
                "installer-preview-back",
                InstallerPreviewEffect::BackInPreview,
            )
            .role(ButtonRole::Plain)
            .with_enabled(native.navigation_allowed());
        let actions = native.nav(
            &self.commands,
            Row::new()
                .push(Box::new(back))
                .push(Box::new(Spacer::new())),
        );
        let notice = native
            .notice
            .as_ref()
            .map(|notice| Box::new(Banner::error(notice.clone())) as Box<dyn Widget>);
        GuidedPage::new(native.body(&self.commands))
            .cursor(self.cursor)
            .maybe_notice(notice)
            .actions(actions)
            .build()
    }
}

impl NativeSetup {
    pub(super) fn body(&self, commands: &Commands) -> Column {
        if self.handoff == Handoff::Check {
            let readiness = if self
                .support_report
                .as_ref()
                .is_some_and(SupportReport::blocks_install)
            {
                CATALOG.t("imager-desktop-unsupported-component")
            } else {
                self.internal_readiness_text()
            };
            let mut body = layout::stack()
                .push(Box::new(Text::new(&t("imager-setup-check-title"))))
                .push(Box::new(
                    Text::new(&readiness).with_live(Politeness::Polite),
                ));
            for section in self.hardware_report_sections() {
                body = body.push(section);
            }
            return body.push(Box::new(self.internal_blockers_card(commands)));
        }
        let mut body = layout::stack();
        if self.handoff == Handoff::Desktop {
            return body.push(Box::new(self.desktop_install_card()));
        }
        let offered = self.internal_install.offers_full_os();
        if offered {
            if let Some(run) = self.internal_run_card() {
                body = body.push(Box::new(run));
            }
            if let Some(resume) = self.internal_resume_card(commands) {
                body = body.push(Box::new(resume));
            }
            body = body
                .push(Box::new(self.internal_plan_card()))
                .push(Box::new(self.internal_blockers_card(commands)));
        }
        if self.support_report.is_some() {
            for section in self.hardware_report_sections() {
                body = body.push(section);
            }
        }
        body = body.push(Box::new(self.internal_download_card(commands)));
        if !offered {
            body = body.push(Box::new(
                Section::new(vec![Box::new(
                    Text::new(&self.internal_readiness_text()).with_live(Politeness::Polite),
                )])
                .with_header(SectionHeader::heading(
                    CATALOG.t("imager-section-before-installing"),
                )),
            ));
        }
        body
    }

    pub(super) fn hardware_report_sections(&self) -> Vec<Box<dyn Widget>> {
        match &self.support_report {
            Some(report) => support_report_view::sections(report, Placement::ThisComputer),
            None => vec![support_report_view::checking()],
        }
    }

    /// The page's ways forward after its way back: the install, its
    /// cancel or recheck, the restart, or the check's continue.
    fn nav(&self, commands: &Commands, nav: Row) -> Row {
        let primary = |id: &str, key: &str, effect: InstallerPreviewEffect, enabled: bool| {
            Box::new(
                press(commands, id, key, effect)
                    .role(ButtonRole::Primary)
                    .with_enabled(enabled),
            )
        };
        match self.handoff {
            Handoff::Check => nav.push(primary(
                "setup.check.continue",
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
                self.check_ready(),
            )),
            Handoff::Install if self.internal_install.offers_full_os() => {
                self.internal_nav(commands, nav)
            }
            Handoff::Install => nav.push(Box::new(
                Button::new(
                    "setup.internal.install-unavailable",
                    &t("imager-setup-internal-install"),
                )
                .control_size(ControlSize::Regular)
                .role(ButtonRole::Primary)
                .with_enabled(false),
            )),
            Handoff::Desktop => match self.desktop_flow.state() {
                DesktopFlowState::Ready(_) => nav.push(primary(
                    "setup.install",
                    "imager-setup-desktop-install",
                    InstallerPreviewEffect::InstallDesktop,
                    true,
                )),
                DesktopFlowState::Downloading(_) => nav.push(primary(
                    "setup.cancel-download",
                    "imager-setup-desktop-cancel",
                    InstallerPreviewEffect::CancelDesktopDownload,
                    true,
                )),
                DesktopFlowState::Failed { .. } | DesktopFlowState::Unavailable { .. } => {
                    nav.push(primary(
                        "setup.retry-desktop",
                        "imager-setup-desktop-recheck",
                        InstallerPreviewEffect::RecheckDesktop,
                        true,
                    ))
                }
                DesktopFlowState::Installed { .. } => nav,
                _ => nav.push(primary(
                    "setup.install",
                    "imager-setup-internal-installing",
                    InstallerPreviewEffect::InstallDesktop,
                    false,
                )),
            },
        }
    }

    fn desktop_install_card(&self) -> Section {
        let text = |value: &str| ArgValue::Text(value.to_owned());
        let phase = |key: &str, selection: &crate::setup::desktop_flow::DesktopSelection| {
            CATALOG.t_args(
                key,
                &[
                    ("version", text(&selection.release.version)),
                    ("host", text(&selection.host)),
                ],
            )
        };
        let detail = match self.desktop_flow.state() {
            DesktopFlowState::Checking => CATALOG.t("imager-desktop-checking"),
            DesktopFlowState::Unavailable { host, .. } => {
                CATALOG.t_args("imager-desktop-unavailable", &[("host", text(host))])
            }
            DesktopFlowState::Ready(selection) => phase(
                if selection.already_installed {
                    "imager-desktop-already-installed"
                } else {
                    "imager-desktop-ready"
                },
                selection,
            ),
            DesktopFlowState::Downloading(selection) => {
                phase("imager-desktop-downloading", selection)
            }
            DesktopFlowState::Authorizing(selection) => {
                phase("imager-desktop-authorizing", selection)
            }
            DesktopFlowState::Preparing(selection) => phase("imager-desktop-preparing", selection),
            DesktopFlowState::Installing(selection) => {
                phase("imager-desktop-installing", selection)
            }
            DesktopFlowState::Verifying(selection) => CATALOG.t_args(
                "imager-desktop-verifying",
                &[("host", text(&selection.host))],
            ),
            DesktopFlowState::Installed { selection, result } => CATALOG.t_args(
                "imager-desktop-installed",
                &[
                    ("version", text(&result.version)),
                    ("host", text(&selection.host)),
                ],
            ),
            DesktopFlowState::Failed { kind, message, .. } => {
                let what = CATALOG.t(match kind {
                    DesktopFailureKind::DownloadCancelled => "imager-desktop-download-cancelled",
                    DesktopFailureKind::AuthorizationCancelled => "imager-desktop-not-approved",
                    DesktopFailureKind::AuthorizationRefused => "imager-desktop-needs-admin",
                    DesktopFailureKind::WorkerLost => "imager-desktop-stopped-early",
                    DesktopFailureKind::Readiness => "imager-desktop-not-ready",
                    DesktopFailureKind::Download => "imager-desktop-download-failed",
                    DesktopFailureKind::NativeTransaction => "imager-desktop-install-failed",
                });
                CATALOG.t_args(
                    "imager-desktop-failed",
                    &[("what", ArgValue::Text(what)), ("detail", text(message))],
                )
            }
        };
        Section::new(vec![Box::new(
            layout::related()
                .push(Box::new(
                    Text::new(&detail)
                        .with_id("setup.desktop.status")
                        .with_live(Politeness::Polite),
                ))
                .push(Box::new(
                    Text::new(&t("installer-desktop-keep-existing")).dimmed(),
                )),
        )])
        .with_header(SectionHeader::heading(CATALOG.t("imager-section-desktop")))
    }

    pub(super) fn internal_download_card(&self, commands: &Commands) -> Section {
        let mut content = layout::related();
        let Some(flow) = &self.internal_download_flow else {
            let detail = self
                .internal_download_error
                .as_deref()
                .map_or_else(|| t("imager-desktop-cannot-download"), t);
            content = content.push(Box::new(
                Text::new(&detail)
                    .with_id("setup.internal.status")
                    .with_live(Politeness::Assertive),
            ));
            return Section::new(vec![Box::new(content)]).with_header(SectionHeader::heading(
                CATALOG.t("imager-section-full-download"),
            ));
        };
        let presentation = flow.presentation();
        let summary = if presentation.phase.is_none()
            && let Some(error) = flow.catalog_error()
        {
            t(error)
        } else {
            t(presentation.sentence_key)
        };
        content = content.push(Box::new(
            Text::new(&summary)
                .with_id("setup.internal.status")
                .with_live(Politeness::Polite),
        ));
        let mut actions = Row::new();
        let mut any = false;
        for (shown, id, key, effect) in [
            (
                flow.can_start(),
                "setup.internal.start",
                "imager-setup-action-start-download",
                InstallerPreviewEffect::StartInternalDownload,
            ),
            (
                flow.can_pause(),
                "setup.internal.pause",
                "imager-setup-action-pause-download",
                InstallerPreviewEffect::PauseInternalDownload,
            ),
            (
                flow.can_cancel(),
                "setup.internal.cancel",
                "imager-setup-action-cancel-download",
                InstallerPreviewEffect::CancelInternalDownload,
            ),
            (
                flow.can_resume(),
                "setup.internal.resume",
                "imager-setup-action-resume-download",
                InstallerPreviewEffect::ResumeInternalDownload,
            ),
            (
                flow.can_release(),
                "setup.internal.release",
                "imager-setup-action-release-download",
                InstallerPreviewEffect::ReleaseInternalDownload,
            ),
            (
                flow.can_discard(),
                "setup.internal.discard",
                "imager-setup-action-discard-download",
                InstallerPreviewEffect::DiscardInternalDownload,
            ),
        ] {
            if shown {
                actions = actions.push(Box::new(press(commands, id, key, effect)));
                any = true;
            }
        }
        if any {
            content = content.push(Box::new(actions));
        }
        Section::new(vec![Box::new(content)])
            .with_header(SectionHeader::heading(t(presentation.title_key)))
    }

    fn detail_row(text: Text, control: Option<Box<dyn Widget>>) -> Row {
        let mut row = Row::new().push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(Box::new(text)),
                )),
        ));
        if let Some(control) = control {
            row = row.push(control);
        }
        row
    }

    fn internal_plan_card(&self) -> Section {
        let install = &self.internal_install;
        let host = install_flow::host_arg(install.host());
        let memory = |bytes: u64| ArgValue::from(tilo_ui_base::format::memory(bytes));
        let pending_plan = install.pending().map(|record| &record.plan);
        let plan = pending_plan.map(Ok).or_else(|| {
            install
                .plan()
                .map(|plan| plan.as_ref().map_err(Clone::clone))
        });
        let summary = match plan {
            None => t("imager-setup-internal-plan-checking"),
            Some(Err(crate::backend::plan::PlanError::InsufficientSpace {
                needed_bytes,
                available_bytes,
            })) => t_args(
                "imager-setup-internal-plan-error-space",
                &[
                    ("needed", memory(needed_bytes)),
                    ("available", memory(available_bytes)),
                ],
            ),
            Some(Err(crate::backend::plan::PlanError::Unsupported(detail))) => t_args(
                "imager-setup-internal-plan-error-other",
                &[("detail", detail.into())],
            ),
            Some(Ok(plan)) => {
                let space = plan.shrink.as_ref().map_or_else(
                    || t("imager-setup-internal-plan-free"),
                    |shrink| {
                        t_args(
                            "imager-setup-internal-plan-shrink",
                            &[
                                ("host", host.into()),
                                (
                                    "shrink",
                                    memory(shrink.from_bytes.saturating_sub(shrink.to_bytes)),
                                ),
                                ("from", memory(shrink.from_bytes)),
                                ("to", memory(shrink.to_bytes)),
                            ],
                        )
                    },
                );
                [
                    space,
                    t_args(
                        "imager-setup-internal-plan-target",
                        &[("target", memory(install_flow::target_extent_bytes(plan)))],
                    ),
                    t_args(
                        "imager-setup-internal-plan-staging",
                        &[("staging", memory(install_flow::staging_bytes(plan)))],
                    ),
                ]
                .join("\n")
            }
        };
        let mut content = layout::stack().push(Box::new(
            Text::new(&summary)
                .with_id("setup.internal.plan")
                .with_live(Politeness::Polite),
        ));
        if pending_plan.is_none()
            && let Some((min, max)) = install.bounds_gib()
        {
            let editable = !install.is_running() && !install.can_restart();
            content = content
                .push(Box::new(
                    Text::new(&t("imager-setup-internal-size-label")).with_role(TextRole::Label),
                ))
                .push(Box::new(
                    NumberInput::from_state(install.target_state())
                        .with_range(min as f64, max as f64)
                        .with_step(1.0)
                        .with_suffix("GiB")
                        .with_id("setup.internal.target-size")
                        .with_accessible_name(t("imager-setup-internal-size-label"))
                        .with_enabled(editable),
                ))
                .push(Box::new(
                    Text::new(&t_args(
                        "imager-setup-internal-size-help",
                        &[
                            ("min", min.into()),
                            ("max", max.into()),
                            ("host", host.into()),
                        ],
                    ))
                    .dimmed(),
                ));
        }
        content = content.push(Box::new(Text::new(&t_args(
            "imager-setup-internal-expect",
            &[("host", host.into())],
        ))));
        if install_flow::BUILD_ALLOW != Some("1") {
            content = content.push(Box::new(
                Text::new(&t("imager-setup-internal-unverified"))
                    .with_id("setup.internal.unverified")
                    .dimmed(),
            ));
        }
        Section::titled(
            t("imager-setup-internal-plan-title"),
            vec![Box::new(content)],
        )
    }

    fn internal_blockers_card(&self, commands: &Commands) -> Section {
        let install = &self.internal_install;
        let idle_host = self.host_command_pending.is_none();
        let mut content = layout::stack();
        if install.blockers().is_empty() {
            content = content.push(Box::new(
                Text::new(&t("imager-setup-internal-blockers-none"))
                    .with_id("setup.internal.blockers"),
            ));
        }
        for blocker in install.blockers() {
            let remedy = match install.remedy(*blocker) {
                Some(HostCommand::FirmwareSettings { .. }) => Some((
                    "setup.internal.firmware-settings",
                    "imager-setup-internal-remedy-firmware",
                    InstallerPreviewEffect::InternalFirmwareSettings,
                )),
                Some(HostCommand::DisableFastStartup) => Some((
                    "setup.internal.disable-fast-startup",
                    "imager-setup-internal-remedy-fast-startup",
                    InstallerPreviewEffect::InternalDisableFastStartup,
                )),
                _ => None,
            };
            let control = remedy.map(|(id, key, effect)| {
                Box::new(press(commands, id, key, effect).with_enabled(idle_host))
                    as Box<dyn Widget>
            });
            content = content.push(Box::new(Self::detail_row(
                Text::new(&install_flow::blocker_text(*blocker, install.host()))
                    .with_id(format!(
                        "setup.internal.blocker.{}",
                        install_flow::blocker_key(*blocker)
                    ))
                    .with_live(Politeness::Polite),
                control,
            )));
        }
        content = content.push(Box::new(
            press(
                commands,
                "setup.internal.recheck",
                "imager-setup-internal-recheck",
                InstallerPreviewEffect::RecheckInternal,
            )
            .with_enabled(!install.is_probing() && !install.is_running()),
        ));
        Section::titled(
            t("imager-setup-internal-blockers-title"),
            vec![Box::new(content)],
        )
    }

    fn internal_resume_card(&self, commands: &Commands) -> Option<Section> {
        let install = &self.internal_install;
        let operation_id = install.pending()?.operation_id.clone();
        let enabled = !install.is_running();
        Some(Section::titled(
            t("imager-setup-internal-resume-title"),
            vec![Box::new(
                layout::stack()
                    .push(Box::new(
                        Text::new(&t("imager-setup-internal-resume"))
                            .with_id("setup.internal.resume"),
                    ))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(
                                press(
                                    commands,
                                    "setup.internal.retry",
                                    "imager-setup-internal-resume-retry",
                                    InstallerPreviewEffect::RetryInternal {
                                        operation_id: operation_id.clone(),
                                    },
                                )
                                .with_enabled(enabled),
                            ))
                            .push(Box::new(
                                press(
                                    commands,
                                    "setup.internal.remove",
                                    "imager-setup-internal-resume-remove",
                                    InstallerPreviewEffect::RemoveInternal { operation_id },
                                )
                                .with_enabled(enabled),
                            )),
                    )),
            )],
        ))
    }

    fn internal_run_card(&self) -> Option<Section> {
        let install = &self.internal_install;
        let host = install.host();
        let (title, detail, politeness) = match install.run_state() {
            RunState::Idle => return None,
            RunState::Running { progress, .. } => (
                t("imager-setup-internal-run-title"),
                progress.as_ref().map_or_else(
                    || t("imager-setup-internal-run-waiting"),
                    |progress| {
                        let step = t(&format!(
                            "imager-setup-internal-step-{}",
                            install_flow::step_key(progress.step)
                        ));
                        if progress.total_bytes == 0 {
                            t_args("imager-setup-internal-run-step", &[("step", step.into())])
                        } else {
                            t_args(
                                "imager-setup-internal-run-progress",
                                &[
                                    ("step", step.into()),
                                    (
                                        "done",
                                        tilo_ui_base::format::memory(progress.done_bytes).into(),
                                    ),
                                    (
                                        "total",
                                        tilo_ui_base::format::memory(progress.total_bytes).into(),
                                    ),
                                ],
                            )
                        }
                    },
                ),
                Politeness::Polite,
            ),
            RunState::Succeeded {
                action: HelperAction::Remove,
            } => (
                t("imager-setup-internal-run-title"),
                t("imager-setup-internal-removed"),
                Politeness::Polite,
            ),
            RunState::Succeeded { .. } => {
                let mut detail = t_args(
                    "imager-setup-internal-done",
                    &[("host", install_flow::host_arg(host).into())],
                );
                if install.restart_deferred() {
                    detail.push('\n');
                    detail.push_str(&t("imager-setup-internal-restart-deferred"));
                }
                (
                    t("imager-setup-internal-done-title"),
                    detail,
                    Politeness::Polite,
                )
            }
            RunState::Failed { failure, .. } => (
                t("imager-setup-internal-run-title"),
                install_flow::failure_text(failure, host),
                Politeness::Assertive,
            ),
        };
        Some(Section::titled(
            &title,
            vec![Box::new(
                Text::new(&detail)
                    .with_id("setup.internal.run")
                    .with_live(politeness),
            )],
        ))
    }

    fn internal_nav(&self, commands: &Commands, mut nav: Row) -> Row {
        let install = &self.internal_install;
        if install.can_restart() {
            if !install.restart_deferred() {
                nav = nav.push(Box::new(press(
                    commands,
                    "setup.internal.restart-later",
                    "imager-setup-internal-restart-later",
                    InstallerPreviewEffect::RestartInternalLater,
                )));
            }
            return nav.push(Box::new(
                press(
                    commands,
                    "setup.internal.restart",
                    "imager-setup-internal-restart-now",
                    InstallerPreviewEffect::RestartInternal,
                )
                .role(ButtonRole::Primary)
                .with_enabled(self.host_command_pending.is_none()),
            ));
        }
        let key = if install.is_running() {
            "imager-setup-internal-installing"
        } else if install.pending().is_some() {
            "imager-setup-internal-continue"
        } else {
            "imager-setup-internal-install"
        };
        let reviewed = install.reviewed_digest().ok().flatten().unwrap_or_default();
        nav.push(Box::new(
            press(
                commands,
                "setup.internal.install",
                key,
                InstallerPreviewEffect::InstallInternal { reviewed },
            )
            .role(ButtonRole::Primary)
            .with_enabled(
                install
                    .install_blocker(self.internal_download_flow.as_ref().is_some_and(
                        crate::setup::internal_download_flow::InternalDownloadFlow::is_ready,
                    ))
                    .is_ok()
                    && !self
                        .support_report
                        .as_ref()
                        .is_some_and(SupportReport::blocks_install),
            ),
        ))
    }
}
