//! What a click on a control does when the window is part of an app's API: the effect it asks the
//! session for, and nothing when the window must carry it out itself.

use super::{Intent, LifecycleAction, Notice, WindowsApp, strings};
use crate::actions;

impl WindowsApp {
    /// Ask the host for `effect` as the live row `row` offers it, or on the manager when
    /// there is no row. False when there is no session to ask: the caller performs the effect here,
    /// so a click never waits on a request nobody will take.
    pub(super) fn queue_live_control(
        &mut self,
        row: Option<&str>,
        effect: &actions::WindowsEffect,
    ) -> bool {
        if !self.provider.borrow_mut().action_native_controls() {
            return false;
        }
        let Some(dispatcher) = &self.dispatcher else {
            // A window that is not part of the app's API (a capture, an offline window) has no
            // session to approve anything: what it was asked for happens in this window instead.
            return false;
        };
        match row {
            Some(row) => dispatcher.effect_on(row, effect),
            None => dispatcher.effect(effect),
        }
        self.notice = Some(Notice::info(strings::t("windows-request-sent")));
        true
    }

    /// Turn a control's intent into the effect search and agents invoke.
    pub(super) fn queue_live_control_intent(&mut self, intent: Intent) -> bool {
        use actions::WindowsEffect as E;
        if !self.provider.borrow_mut().action_native_controls() {
            return false;
        }
        let appliance = match intent {
            Intent::ChooseIso => Some(E::ImportIso { path: None }),
            Intent::Recovery => Some(E::Recovery),
            Intent::Lifecycle(LifecycleAction::Start) => Some(E::Start),
            Intent::Lifecycle(LifecycleAction::Restart) => Some(E::Restart),
            Intent::Lifecycle(LifecycleAction::Suspend) => Some(E::Suspend),
            Intent::Lifecycle(LifecycleAction::Resume) => Some(E::Resume),
            Intent::Lifecycle(LifecycleAction::Shutdown) => Some(E::Shutdown),
            Intent::ConnectActivation => Some(E::TemporaryNetwork),
            Intent::ConnectUpdate => Some(E::UpdateNetwork),
            Intent::RevokeConnection => Some(E::RevokeNetwork),
            _ => None,
        };
        if let Some(effect) = appliance {
            return self.queue_live_control(Some(actions::APPLIANCE_ROW), &effect);
        }
        match intent {
            Intent::Install => return self.queue_live_control(None, &E::Install { path: None }),
            Intent::ConfirmCapsule => {
                return self.queue_live_control(
                    Some(actions::PENDING_INSTALL_ROW),
                    &E::InstallPrivateCapsule,
                );
            }
            Intent::ConfirmVm => {
                return self.queue_live_control(
                    Some(actions::PENDING_INSTALL_ROW),
                    &E::InstallWindowsAppliance,
                );
            }
            Intent::InstallSupport => {
                return self.queue_live_control(None, &E::InstallWindowsSupport);
            }
            _ => {}
        }
        let Some(app) = self.selected_app() else {
            return false;
        };
        let row = crate::search::app_resource_id(&app.id);
        let (id, capsule) = (app.id.clone(), app.capsule.clone());
        let effect = match intent {
            Intent::OpenFile => E::OpenFile {
                app: id,
                path: None,
            },
            Intent::Launch => E::Launch { app: id },
            Intent::Stop => E::Stop { app: id },
            Intent::VmFallback => E::LaunchVmFallback { app: id },
            Intent::Publish => E::Publish { app: id },
            Intent::Retry => E::RetryInstall { capsule },
            Intent::Escalate => E::EscalateInstall { capsule },
            Intent::RemoveLauncher => E::RemoveLauncher { app: id },
            Intent::Uninstall => E::Uninstall { app: id },
            Intent::DeleteData => E::DeleteData { capsule },
            _ => return false,
        };
        self.queue_live_control(Some(&row), &effect)
    }
}
