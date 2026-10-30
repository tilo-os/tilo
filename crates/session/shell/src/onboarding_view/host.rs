//! The setup app as this view hosts it: mounted in the shell's process, lent
//! the seat, shown the page the ceremony stands on, and asked for nothing
//! but what its person did.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use tilo_onboarding::{OnboardingEffect, OnboardingSeat, Page, Secrets, Step};
use ui::theme::Theme;
use ui::widget::{AppModel, AppWidget, ModelPath, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::WindowEvent;

use super::{
    AcknowledgementKind, KEY_BACK, KEY_BACKUP_ANOTHER, KEY_BACKUP_BROWSE, KEY_BACKUP_CONTINUE,
    KEY_BEGIN, KEY_CHOOSE_NEARBY, KEY_CHOOSE_RECOVERY_KEY, KEY_CHOOSE_RESTORE, KEY_CODECS_CONTINUE,
    KEY_CODECS_INSTALL, KEY_CODECS_SKIP, KEY_CONNECT_BROWSE, KEY_CONNECT_CANCEL,
    KEY_CONNECT_NEARBY, KEY_CONNECT_RETRY, KEY_CREATE, KEY_EXISTING, KEY_FINISH, KEY_IMPORT,
    KEY_PREPARED_IMPORT, KEY_RECOVERY_KEY_CLOSE, KEY_RECOVERY_KEY_CONFIRM,
    KEY_RECOVERY_KEY_RESTORE, KEY_RECOVERY_KEY_SHOW, KEY_TOUR_BACK, KEY_TOUR_NEXT, KEY_TOUR_SKIP,
    KEY_TOUR_START, OnboardingView, PickerPurpose,
};

/// The instance the app is mounted as: past every tab id, so setup never
/// names a window.
const SETUP_INSTANCE: u64 = u64::MAX - 0x0005_e70b;

/// The mounted app and where it is laid out.
pub(super) struct Hosted {
    seat: OnboardingSeat,
    app: Option<Box<dyn AppWidget>>,
    /// Mounting failed once: setup is not mounted again on every frame.
    refused: bool,
    node: Option<NodeId>,
}

impl Default for Hosted {
    fn default() -> Self {
        Self::new(Secrets::default())
    }
}

impl Hosted {
    pub(super) fn new(secrets: Secrets) -> Self {
        Self {
            seat: OnboardingSeat::new(secrets, Some(runtime_identity::unknown_recovery_words)),
            app: None,
            refused: false,
            node: None,
        }
    }

    pub(super) fn seat(&self) -> &OnboardingSeat {
        &self.seat
    }

    /// Mount the app, once. It is this device's own: its actions reach only
    /// the person at this device, and no room publishes them.
    fn mount(&mut self) {
        if self.app.is_some() || self.refused {
            return;
        }
        let seat = self.seat.clone();
        let mounted = crate::shell::in_process::mount_sheet(
            tilo_onboarding::definition(),
            &crate::shell::state::local_user_id(),
            "this-device",
            "onboarding",
            SETUP_INSTANCE,
            |cx| cx.lend(seat),
        )
        .and_then(|component| {
            tilo_desktop::resolver::app_tile(component, None, |_| {
                crate::shell::wake_compositor();
            })
        });
        match mounted {
            Ok(app) => self.app = Some(app),
            Err(error) => {
                self.refused = true;
                tracing::error!(%error, "setup could not be mounted");
            }
        }
    }

    pub(super) fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> Option<NodeId> {
        self.mount();
        self.node = None;
        let app = self.app.as_mut()?;
        // Setup looks and moves as the shell around it does: the app takes
        // the shell's palette from the theme, and here its text size, UI
        // scale and reduced motion, which an app otherwise keeps its own.
        if let Some(embedded) = app
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<ui::app::EmbeddedApp>())
        {
            let differs = |a: f32, b: f32| (a - b).abs() > f32::EPSILON;
            if differs(embedded.app().text_scale(), theme.text_scale) {
                embedded.app_mut().set_text_scale(theme.text_scale);
            }
            if differs(embedded.app().ui_scale(), theme.ui_scale) {
                embedded.app_mut().set_ui_scale(theme.ui_scale);
            }
        }
        app.set_motion_reduced(theme.reduce_motion);
        let node = app.build_layout(taffy, theme, node_map);
        if let Ok(mut style) = taffy.style(node).cloned() {
            style.size = taffy::Size::from_percent(1.0, 1.0);
            let _ = taffy.set_style(node, style);
        }
        self.node = Some(node);
        Some(node)
    }

    pub(super) fn apply_layout(&mut self, taffy: &TaffyTree<()>, origin: Point) {
        if let (Some(app), Some(node)) = (self.app.as_mut(), self.node) {
            app.apply_layout(taffy, node, origin);
        }
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.node.is_none() {
            return;
        }
        if let Some(app) = self.app.as_mut() {
            app.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    pub(super) fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A key right after setup opens can arrive before it is laid out.
        if let Some(app) = self.app.as_mut() {
            let node = self.node.unwrap_or(ctx.node_id);
            let path = ctx.path;
            app.handle_event(event, &mut ctx.reborrow(path, node));
        }
    }

    /// Run the app's pump: what it was asked lands in the seat.
    pub(super) fn update(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        self.app.as_mut().is_some_and(|app| app.update(doc, path))
    }

    pub(super) fn widget(&self) -> Option<&dyn Widget> {
        self.node?;
        self.app.as_deref().map(|app| app as &dyn Widget)
    }

    pub(super) fn widget_mut(&mut self) -> Option<&mut (dyn Widget + '_)> {
        self.node?;
        self.app.as_deref_mut().map(|app| app as &mut dyn Widget)
    }

    pub(super) fn needs_redraw(&self) -> bool {
        self.app.as_ref().is_some_and(|app| app.needs_redraw())
    }

    /// The app's own root widget, as its last frame composed it: what a test
    /// reaches into, as the app's own tests would in its window.
    #[cfg(test)]
    pub(super) fn root_mut(&mut self) -> Option<&mut dyn Widget> {
        self.app
            .as_mut()?
            .as_any_mut()?
            .downcast_mut::<ui::app::EmbeddedApp>()?
            .component_root_mut()
    }

    /// Where the app's control `id` is in the host's space, as its last frame
    /// laid it out: what a pointer presses to reach it.
    #[cfg(test)]
    pub(super) fn control_rect(&mut self, id: &str) -> Option<vello::kurbo::Rect> {
        let app = self
            .app
            .as_mut()?
            .as_any_mut()?
            .downcast_mut::<ui::app::EmbeddedApp>()?;
        let place = vello::kurbo::Affine::translate(Widget::get_rect(app).origin().to_vec2());
        let control = ui::widget::find_test_id(app.component_root_mut()?, id)?;
        Some(place.transform_rect_bbox(control.get_rect()))
    }
}

impl OnboardingView {
    /// The secret buffers the app's fields write into: this view's own.
    pub(super) fn secrets(&self) -> Secrets {
        Secrets {
            connect_passphrase: self.connect_passphrase.clone(),
            backup_passphrase: self.backup_passphrase.clone(),
            backup_confirmation: self.backup_confirmation.clone(),
            recovery_words: self.recovery_words.clone(),
            key_answers: self.key_answers.clone(),
        }
    }

    /// The page the ceremony stands on.
    #[allow(
        clippy::too_many_lines,
        reason = "one plain field per fact the setup page shows"
    )]
    pub(super) fn page(&self) -> Page {
        let migration = self.migration_active().then(|| tilo_onboarding::Migration {
            transfer: self.migration_transfer(),
            status: self.migration_status().unwrap_or_default(),
            needs_password: self.migration_needs_password(),
            cancellable: self.migration_cancellable(),
        });
        #[cfg(target_os = "linux")]
        let peer = self
            .peer
            .as_ref()
            .map(crate::onboarding_peer::Peer::verification_code);
        #[cfg(not(target_os = "linux"))]
        let peer = None;
        Page {
            screen: self.flow.screen(),
            context: self.context,
            recovery_only: self.recovery_only,
            prepared: self.prepared_pending.then(|| tilo_onboarding::Prepared {
                working: self.prepared_working(),
                peer,
            }),
            migration,
            portable_backup_only: self.portable_backup_only,
            identity_ready_page: self.identity_ready_page,
            joined_member: self.joined_member,
            codec_page: self.codec_page,
            codecs: self.codecs_page(),
            tour_page: self.tour_page,
            total_steps: self.total_steps(),
            acknowledged: self.flow.acknowledged(),
            can_create: self.flow.can_create(),
            busy: !self.identity_idle(),
            error: self.error.clone(),
            progress_error: self.progress_error.clone(),
            expected_identity: self.expected_identity.map(hex::encode),
            recovery_problem: self.recovery_problem.map(|problem| match problem {
                crate::onboarding::IdentityProblem::KeyringUnavailable => {
                    tilo_onboarding::IdentityProblem::KeyringUnavailable
                }
                crate::onboarding::IdentityProblem::RecordMissing => {
                    tilo_onboarding::IdentityProblem::RecordMissing
                }
                crate::onboarding::IdentityProblem::ChainInvalid => {
                    tilo_onboarding::IdentityProblem::ChainInvalid
                }
            }),
            nearby: tilo_onboarding::Nearby {
                active: self.nearby_active(),
                status: self.nearby_status.clone(),
                failure: self
                    .nearby_failure
                    .as_ref()
                    .map(|failure| (failure.title(), failure.message())),
                code: self.nearby_code.clone(),
            },
            connect_path: self.connect_path.get(),
            recovery_check: self.recovery_check.clone(),
            recovery_words_total: runtime_identity::RECOVERY_WORD_COUNT,
            backup_error: self.backup_error.clone(),
            backup_saved: self
                .backup_saved
                .as_ref()
                .map(|path| path.display().to_string()),
            backup_was_verified: self.backup_was_verified,
            backup_export_available: self.backup_export_available,
            backup_copy_form_open: self.backup_copy_form_open,
            backup_skip_acknowledged: self.backup_skip_acknowledged,
            recovery_key_copy: self.recovery_key_copy.as_ref().map(|copy| {
                tilo_onboarding::KeyCopy {
                    words: tilo_onboarding::SecretText::new(copy.words.clone()),
                    checks: copy.checks,
                }
            }),
            recovery_key_written: self.recovery_key_written,
            recovery_key_copy_confirmed: self.recovery_key_copy_confirmed,
            chooser: self.chooser.map(|purpose| {
                let (initial, roots) = self.session.picker_scope();
                tilo_onboarding::Chooser {
                    purpose: match purpose {
                        PickerPurpose::ImportIdentity => tilo_onboarding::ChooserPurpose::Restore,
                        PickerPurpose::BackupIdentity => tilo_onboarding::ChooserPurpose::Backup,
                    },
                    initial,
                    roots,
                }
            }),
            compact: self.rect.height() > 0.0 && self.rect.height() <= 540.0,
        }
    }

    fn codecs_page(&self) -> tilo_onboarding::Codecs {
        use os_actions::codec_pack::PackState as Pack;
        let status = &self.codec_status;
        tilo_onboarding::Codecs {
            state: match status.state {
                Pack::NotInstalled => tilo_onboarding::PackState::NotInstalled,
                Pack::Installing => tilo_onboarding::PackState::Installing,
                Pack::WaitingForNetwork => tilo_onboarding::PackState::WaitingForNetwork,
                Pack::Installed => tilo_onboarding::PackState::Installed,
                Pack::Removing => tilo_onboarding::PackState::Removing,
                Pack::Failed => tilo_onboarding::PackState::Failed,
            },
            running: self.codec_running.is_some(),
            error: self.codec_error.clone(),
            fraction: status
                .progress
                .as_ref()
                .map(os_actions::codec_pack::Progress::fraction),
            summary: status.summary(),
            when_it_applies: status.when_it_applies().map(str::to_owned),
            last_error: status.last_error.clone(),
        }
    }

    /// Which carried-over transfer the migration page is about.
    fn migration_transfer(&self) -> tilo_onboarding::Transfer {
        use tilo_onboarding::Transfer;
        if self
            .portable_home
            .as_ref()
            .is_some_and(crate::onboarding_portable_home::Pending::pending)
        {
            Transfer::PortableHome
        } else if self
            .portable_applications
            .as_ref()
            .is_some_and(crate::onboarding_portable_applications::Pending::pending)
        {
            Transfer::PortableApplications
        } else if self
            .browser
            .as_ref()
            .is_some_and(crate::onboarding_browser::Pending::pending)
        {
            Transfer::Browser
        } else {
            Transfer::Migration
        }
    }

    fn migration_needs_password(&self) -> bool {
        self.portable_home
            .as_ref()
            .is_some_and(crate::onboarding_portable_home::Pending::needs_password)
            || self
                .portable_applications
                .as_ref()
                .is_some_and(crate::onboarding_portable_applications::Pending::needs_password)
            || self
                .browser
                .as_ref()
                .is_some_and(crate::onboarding_browser::Pending::needs_password)
    }

    /// Whether the transfer under way can still be cancelled.
    fn migration_cancellable(&self) -> bool {
        let status = self.migration_status().unwrap_or_default();
        let state = status
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unavailable");
        let flag = |key: &str| status.get(key).and_then(serde_json::Value::as_bool) == Some(true);
        state != "cancelling"
            && (state != "reviewing"
                || self
                    .portable_home
                    .as_ref()
                    .is_some_and(crate::onboarding_portable_home::Pending::busy)
                || self
                    .portable_applications
                    .as_ref()
                    .is_some_and(crate::onboarding_portable_applications::Pending::busy)
                || self
                    .browser
                    .as_ref()
                    .is_some_and(crate::onboarding_browser::Pending::busy)
                || flag("original_copy")
                || flag("retained_access"))
    }

    /// Show the app the page the ceremony stands on now.
    pub(super) fn show_page(&self) {
        self.app.seat().show(self.page());
    }

    /// Take what the app's person asked into the ceremony's own queues.
    pub(super) fn take_app_asks(&mut self) {
        let asked = self.app.seat().take_asked();
        if asked.is_empty() {
            return;
        }
        let step = self.page().step();
        for effect in asked {
            match effect {
                OnboardingEffect::Acknowledge { checked } => {
                    let kind = match step {
                        Step::Protect => AcknowledgementKind::IdentityProtection,
                        Step::Backup => AcknowledgementKind::SkipBackup,
                        _ => continue,
                    };
                    self.acknowledgements.borrow_mut().push((kind, checked));
                }
                OnboardingEffect::Enter { field, value } if field == "backup-file" => {
                    if step == Step::Restore {
                        self.connect_path.set(value);
                    }
                }
                OnboardingEffect::Pick { path } => self.picked = Some(path.into()),
                OnboardingEffect::CloseChooser => {
                    if self.chooser.take().is_some() {
                        self.invalidation.both();
                    }
                }
                effect => {
                    if let Some(key) = self.press_for(&effect, step) {
                        self.pressed.press(key);
                    }
                }
            }
        }
    }

    /// The ceremony's press an app action stands for on `step`. Each press
    /// still checks its own guard when `sync` takes it.
    fn press_for(&self, effect: &OnboardingEffect, step: Step) -> Option<&'static str> {
        Some(match (effect, step) {
            (OnboardingEffect::Continue, Step::Intro) => KEY_BEGIN,
            (OnboardingEffect::Continue, Step::RecoveryKey) => KEY_RECOVERY_KEY_RESTORE,
            (OnboardingEffect::Continue, Step::RecoveryKeyCopy) => KEY_RECOVERY_KEY_CONFIRM,
            (OnboardingEffect::Continue, Step::Restore) => KEY_IMPORT,
            (OnboardingEffect::Continue, Step::Prepared) => KEY_PREPARED_IMPORT,
            (OnboardingEffect::Continue, Step::Protect) => KEY_CREATE,
            (OnboardingEffect::Continue, Step::Backup) => KEY_BACKUP_CONTINUE,
            (OnboardingEffect::Continue, Step::Tour(_)) => KEY_TOUR_NEXT,
            (OnboardingEffect::Continue, Step::Codecs) => KEY_CODECS_CONTINUE,
            (OnboardingEffect::Continue, Step::Ready) | (OnboardingEffect::Finish, _) => KEY_FINISH,
            (OnboardingEffect::Continue | OnboardingEffect::Retry, Step::Migration) => {
                "onboarding.migration.next"
            }
            (OnboardingEffect::Back, Step::RecoveryKeyCopy) => KEY_RECOVERY_KEY_CLOSE,
            (OnboardingEffect::Back, Step::Tour(_)) => KEY_TOUR_BACK,
            (OnboardingEffect::Back, _) => KEY_BACK,
            (OnboardingEffect::Choose { option }, _) => match option.as_str() {
                "create" => KEY_BEGIN,
                "existing" => KEY_EXISTING,
                "nearby" => KEY_CHOOSE_NEARBY,
                "recovery-key" => KEY_CHOOSE_RECOVERY_KEY,
                "backup-file" => KEY_CHOOSE_RESTORE,
                "write-down-recovery-key" => KEY_RECOVERY_KEY_SHOW,
                "another-copy" => KEY_BACKUP_ANOTHER,
                "tour" => KEY_TOUR_START,
                "install-codecs" => KEY_CODECS_INSTALL,
                "accept-incomplete" => "onboarding.migration.continue",
                _ => return None,
            },
            (OnboardingEffect::Browse, Step::Restore) => KEY_CONNECT_BROWSE,
            (OnboardingEffect::Browse, Step::Backup) => KEY_BACKUP_BROWSE,
            (OnboardingEffect::Skip, Step::Tour(_)) => KEY_TOUR_SKIP,
            (OnboardingEffect::Skip, Step::Codecs) => KEY_CODECS_SKIP,
            (OnboardingEffect::Skip, Step::Migration) => "onboarding.migration.later",
            (OnboardingEffect::Retry, Step::Nearby) => KEY_CONNECT_NEARBY,
            (OnboardingEffect::Retry, Step::Existing) => KEY_CONNECT_RETRY,
            (OnboardingEffect::Retry, Step::Codecs) => KEY_CODECS_INSTALL,
            (OnboardingEffect::Cancel, Step::Nearby) => KEY_CONNECT_CANCEL,
            (OnboardingEffect::Cancel, Step::Migration) => "onboarding.migration.cancel",
            (OnboardingEffect::Cancel, Step::Backup) if self.portable_backup_only => KEY_FINISH,
            _ => return None,
        })
    }
}
