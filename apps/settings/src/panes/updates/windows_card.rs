//! The Windows support card on the Updates pane: the story the pane keeps
//! for it, told in the card the person sees.

use std::sync::Arc;

use ui::widget::{ProgressBar, Row};

use super::{
    El, Landed, Landing, Settings, SettingsSection, UpdatesEffect, UpdatesModel, ViewCx,
    WindowsReadiness, WindowsSupportAction, block, button, note, row, rows, section, t, t_args,
    thing_row, windows_card_title, windows_explanation,
};

use ui::tilo_resource::Effect as _;

impl UpdatesModel {
    /// The Windows support card's title and lines, in order, as the person
    /// reads them. Nothing (not even the title) on a system whose image does
    /// not offer the extension: there is no choice to make there.
    #[cfg(test)]
    pub fn windows_support_card_lines(&self) -> Vec<String> {
        let mut out = vec![windows_card_title(), windows_explanation()];
        out.push(format!(
            "{}: {}",
            t("upd-status"),
            self.windows_status_line()
        ));
        if let Some(fraction) = self.windows_support.progress_fraction() {
            out.push(t_args(
                "upd-progress",
                &[("percent", format!("{:.0}", fraction * 100.0).into())],
            ));
        }
        for (label, _, _) in self.windows_buttons() {
            out.push(label);
        }
        out
    }

    pub(super) fn windows_status_line(&self) -> String {
        // A request the helper has not picked up yet has no progress and no
        // actions; say it is starting rather than repeat the offer.
        if self.windows_pending.is_some()
            && self.windows_support.offered_actions().is_empty()
            && matches!(self.windows_support, WindowsReadiness::NeedsInstall(_))
        {
            return t("upd-windows-starting");
        }
        match &self.windows_support {
            WindowsReadiness::Ready => t("upd-windows-installed"),
            WindowsReadiness::NeedsInstall(disclosure) => {
                // The size comes from this image's own catalogue entry, so the
                // figure is the one that will actually be downloaded.
                t_args(
                    "upd-windows-not-installed",
                    &[("size", disclosure.sentence().into())],
                )
            }
            WindowsReadiness::Installing(_) => match self.windows_support.progress_label() {
                Some((label, step, steps)) => match self.windows_support.progress_megabytes() {
                    Some((done, total)) => t_args(
                        "upd-codecs-step-bytes",
                        &[
                            ("label", label.into()),
                            ("done", done.to_string().into()),
                            ("total", total.to_string().into()),
                            ("step", step.to_string().into()),
                            ("steps", steps.to_string().into()),
                        ],
                    ),
                    None => t_args(
                        "upd-codecs-step",
                        &[
                            ("label", label.into()),
                            ("step", step.to_string().into()),
                            ("steps", steps.to_string().into()),
                        ],
                    ),
                },
                None => t("upd-windows-downloading"),
            },
            WindowsReadiness::WaitingForNetwork => t("upd-windows-waiting"),
            WindowsReadiness::Failed(error, _) => {
                t_args("upd-windows-failed", &[("error", error.as_str().into())])
            }
            WindowsReadiness::NotOffered => t("upd-windows-not-offered"),
        }
    }

    /// (label, control id, action) of the buttons the card offers.
    pub(super) fn windows_buttons(&self) -> Vec<(String, &'static str, WindowsSupportAction)> {
        if self.windows_pending.is_some() {
            return Vec::new();
        }
        self.windows_support
            .offered_actions()
            .into_iter()
            .map(|action| {
                let label = match (action, &self.windows_support) {
                    (WindowsSupportAction::Install, WindowsReadiness::Failed(..)) => {
                        t("upd-codecs-try-again")
                    }
                    (WindowsSupportAction::Install, _) => t("upd-codecs-install"),
                    (WindowsSupportAction::Remove, WindowsReadiness::WaitingForNetwork) => {
                        t("upd-codecs-cancel")
                    }
                    _ => t("upd-codecs-remove"),
                };
                let control = match action {
                    WindowsSupportAction::Install => "settings.updates.windows.install",
                    _ => "settings.updates.windows.remove",
                };
                (label, control, action)
            })
            .collect()
    }

    pub(super) fn windows_card(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let mut buttons = Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_align_items(ui::taffy::AlignItems::Center);
        let offered = self.windows_buttons();
        let any = !offered.is_empty();
        for (label, control_id, action) in offered {
            let primary = action == WindowsSupportAction::Install;
            let button = button(cx, control_id, &label, Self::windows_effect(action))
                .control_size(ui::widget::ControlSize::Small);
            buttons = buttons.push(Box::new(if primary {
                button.role(ui::widget::ButtonRole::Primary)
            } else {
                button
            }));
        }
        let mut body = vec![
            note(&windows_explanation()),
            row(Box::new(thing_row(
                "settings.updates.windows.status",
                &t("upd-status"),
                &[self.windows_status_line()],
                any.then(|| Box::new(buttons) as El),
            ))),
        ];
        if let Some(fraction) = self.windows_support.progress_fraction() {
            body.push(block(Box::new(
                ProgressBar::new(fraction).with_label(t("upd-installing-windows")),
            )));
        }
        section(&windows_card_title(), rows(body))
    }

    pub(super) fn change_windows_support(&mut self, action: WindowsSupportAction) {
        if self.windows_pending.is_some() {
            self.operation_failed(
                Self::windows_effect(action).id(),
                "Windows support is already being changed",
            );
            self.changed = true;
            return;
        }
        let runner = Arc::clone(&self.windows_support_runner);
        self.notice = None;
        self.windows_events = Some(Landing::start(move || runner(action)));
        self.windows_pending = Some(action);
        self.changed = true;
    }

    pub(super) const fn windows_effect(action: WindowsSupportAction) -> UpdatesEffect {
        match action {
            WindowsSupportAction::Install => UpdatesEffect::InstallWindowsSupport,
            _ => UpdatesEffect::RemoveWindowsSupport,
        }
    }

    pub(super) fn drain_windows_events(&mut self) {
        let (Some(events), Some(action)) = (self.windows_events.as_ref(), self.windows_pending)
        else {
            return;
        };
        let result = match events.poll::<Result<(), String>>() {
            Landed::Done(result) => result,
            Landed::Pending => return,
            Landed::Lost => Err("the Windows support worker stopped".into()),
        };
        self.windows_events = None;
        self.windows_pending = None;
        self.windows_support = (self.windows_support_provider)();
        let operation = Self::windows_effect(action);
        match result {
            Ok(()) => {
                self.notice = None;
                self.operation_succeeded(operation.id(), "Windows support changed");
            }
            Err(error) => {
                self.notice = Some(t_args(
                    "upd-notice-windows",
                    &[("error", error.as_str().into())],
                ));
                self.operation_failed(
                    operation.id(),
                    format!("Windows support could not be changed: {error}"),
                );
            }
        }
        self.changed = true;
    }
}
