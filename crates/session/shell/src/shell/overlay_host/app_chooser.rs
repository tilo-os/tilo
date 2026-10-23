//! The shell's app chooser, as an app asks for it (the chooser service's
//! `ChooseApp`): the palette over the installed apps, each with its icon,
//! filtered by typing. Choosing one answers the app with it instead of
//! opening it; closing the palette answers that none was chosen.

use std::sync::mpsc;

use tilo_dialogs::shell::ChosenApp;

use super::ShellOverlayHost;
use crate::shell::i18n::t_args;
use crate::shell::search::{ResourceKind, SearchResult};
use ui::widget::Widget as _;

/// An app chooser on screen, waiting for the person's choice.
pub(crate) struct PendingAppChooser {
    reply: mpsc::Sender<Result<Option<ChosenApp>, String>>,
    /// The kinds the palette was filtered to before, given back after.
    previous_filter: Vec<ResourceKind>,
}

impl ShellOverlayHost {
    /// Open the palette on choosing an app for `purpose`; `reply` hears the
    /// app chosen, or `None` once the palette closes without one. A chooser
    /// already open answers `None` first: one choice at a time.
    pub(crate) fn open_app_chooser(
        &mut self,
        purpose: &str,
        reply: mpsc::Sender<Result<Option<ChosenApp>, String>>,
    ) {
        use crate::shell::chrome_state::SearchScope;
        self.answer_app_chooser(None);
        if self.chrome.is_none() {
            let _ = reply.send(Err(crate::shell::i18n::t("portal-chooser-session-closing")));
            return;
        }
        let previous_filter = std::mem::replace(
            &mut self.active_kind_filter,
            vec![ResourceKind::Application],
        );
        if !self.palette_visible
            || self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().search_scope != SearchScope::All)
        {
            self.open_palette_with_scope(SearchScope::All);
        }
        self.palette.set_query("");
        self.palette.set_query_placeholder(t_args(
            "host-app-chooser-placeholder",
            &[("purpose", purpose.into())],
        ));
        self.last_search_query = "\0app-chooser\0".into();
        self.palette_visible = true;
        self.palette.set_focus(true);
        self.palette_layout_dirty = true;
        self.app_chooser = Some(PendingAppChooser {
            reply,
            previous_filter,
        });
    }

    /// The app chooser open for a capture, where nobody reads the answer.
    pub fn open_app_chooser_for_shots(&mut self, purpose: &str) {
        let (reply, answer) = mpsc::channel();
        // The capture ends with the process; its answer is never read.
        std::mem::forget(answer);
        self.open_app_chooser(purpose, reply);
    }

    pub(crate) fn choosing_app(&self) -> bool {
        self.app_chooser.is_some()
    }

    /// The selected row, while choosing an app: answer with the app it is
    /// and close the palette. Whether the row was taken as the choice (a
    /// row that is no installed app is not).
    pub(super) fn choose_app_row(&mut self, result: &SearchResult) -> bool {
        if self.app_chooser.is_none() {
            return false;
        }
        let Some(app) = self.chosen_app(result) else {
            return false;
        };
        self.answer_app_chooser(Some(app));
        self.palette_visible = false;
        self.palette.close();
        true
    }

    /// An app chooser whose palette closed without a choice answers so.
    /// Called once per turn of the compositor loop.
    pub(crate) fn settle_app_chooser(&mut self) {
        if self.app_chooser.is_some() && !self.palette_visible {
            self.answer_app_chooser(None);
        }
    }

    fn answer_app_chooser(&mut self, app: Option<ChosenApp>) {
        if let Some(pending) = self.app_chooser.take() {
            self.active_kind_filter = pending.previous_filter;
            let _ = pending.reply.send(Ok(app));
        }
    }

    /// The app a palette row stands for, if it is one installed here: a
    /// Tilo app by its package id, a host app by its desktop-file id with
    /// every id its windows may announce. Shortcuts and catalogue offers
    /// are no installed app.
    fn chosen_app(&self, result: &SearchResult) -> Option<ChosenApp> {
        if result.kind != ResourceKind::Application {
            return None;
        }
        if result.provider_id == crate::shell::search::providers::host_applications::PROVIDER_ID {
            let app = crate::shell::host_apps::find(&result.id)?;
            let mut window_ids = vec![app.desktop_id.clone()];
            for id in app.window_identities {
                if !window_ids.contains(&id) {
                    window_ids.push(id);
                }
            }
            return Some(ChosenApp {
                id: app.desktop_id,
                name: result.title.clone(),
                window_ids,
            });
        }
        let installed = crate::shell::launcher::installed_apps_without_waiting()
            .iter()
            .any(|app| app.package_id == result.id)
            || self.chrome.as_ref().is_some_and(|chrome| {
                chrome
                    .borrow()
                    .hosted_inputs
                    .native_apps()
                    .iter()
                    .any(|app| app.package_id == result.id)
            });
        installed.then(|| ChosenApp {
            id: result.id.clone(),
            name: result.title.clone(),
            window_ids: vec![result.id.clone()],
        })
    }
}
