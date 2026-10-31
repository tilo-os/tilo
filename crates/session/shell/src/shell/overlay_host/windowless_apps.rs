//! The apps the session runs with no window ([`crate::shell::windowless`]), on the overlay
//! host's loop, where the session's in-process apps live: mounted when the broker asks for one,
//! then run on every wake and never drawn.

use std::collections::HashMap;

use ui::widget::AppWidget;

use crate::shell::windowless::{self, Instance};

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

/// The windowless instances this host runs, by app and room.
#[derive(Default)]
pub(super) struct WindowlessApps {
    running: HashMap<(String, String), Box<dyn AppWidget>>,
}

impl ShellOverlayHost {
    /// Mount the windowless instances asked for since the last turn, and run every one with
    /// what its wake brought (an invocation, a collaborator's change). Whether anything ran;
    /// none of it is drawn.
    pub(super) fn run_windowless(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        for (app, room) in windowless::asked() {
            self.mount_windowless(&app, &room);
        }
        let mut ran = false;
        for app in self.windowless.running.values_mut() {
            ran |= app.update(doc, path);
        }
        ran
    }

    fn mount_windowless(&mut self, app_id: &str, room: &str) {
        let key = (app_id.to_owned(), room.to_owned());
        if self.windowless.running.contains_key(&key) {
            windowless::settle(app_id, room, Instance::Running);
            return;
        }
        let Some(definition) = windowless::definition(app_id) else {
            windowless::settle(
                app_id,
                room,
                Instance::Failed(format!("this session runs no {app_id} without a window")),
            );
            return;
        };
        let Some(apps) = self.sheet_apps() else {
            windowless::settle(
                app_id,
                room,
                Instance::Failed("this session mounts no apps in its own process".into()),
            );
            return;
        };
        // The record a launched window of the app opens and the room's
        // readers read (Tasks' tracker): its package primary, the record
        // whose route is the app's own channel. A tab's default record would
        // be another document, which neither a Tasks window nor the tracker's
        // readers ever see.
        let mounted = apps.borrow_mut().mount(
            definition,
            room,
            windowless::INSTANCE,
            ui::record::RecordId::package_primary(),
            |_| {},
        );
        let component = match mounted {
            Ok(component) => component,
            // The store's thread opens the record off this loop; its wake mounts it again.
            Err(error) if error.is_opening() => return,
            Err(error) => {
                tracing::error!(%error, app_id, room, "an app could not run without a window");
                windowless::settle(app_id, room, Instance::Failed(error.to_string()));
                return;
            }
        };
        let Some(collaboration) = apps.borrow().windowless_collaboration(&component, room) else {
            windowless::settle(
                app_id,
                room,
                Instance::Failed("this session's apps reach no broker".into()),
            );
            return;
        };
        match tilo_desktop::resolver::app_tile(component, Some(collaboration), |_| {
            crate::shell::wake_compositor();
        }) {
            Ok(app) => {
                tracing::info!(app_id, room, "an app runs without a window for its actions");
                self.windowless.running.insert(key, app);
                windowless::settle(app_id, room, Instance::Running);
            }
            Err(error) => {
                tracing::error!(%error, app_id, room, "an app could not run without a window");
                windowless::settle(app_id, room, Instance::Failed(error.to_string()));
            }
        }
    }
}
