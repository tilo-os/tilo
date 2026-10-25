//! An app the shell shows in a tab is let author its room as a launched app
//! is: through the one opening every native app makes
//! ([`launcher::admit_native_app`]), asked off the frame loop.

use std::collections::BTreeSet;

use crate::shell::launcher::{self, NativeAppOpening};
use crate::shell::state::{RoomId, WorkspaceTreeHandle};

/// The apps the session's tabs show, each in its room, admitted once the
/// session's broker is there to ask through. A tab the session restores
/// mounts before the broker binds: its app waits here until it does.
#[derive(Default)]
pub(super) struct TabAdmission {
    through: Option<(WorkspaceTreeHandle, NativeAppOpening)>,
    waiting: BTreeSet<(String, String)>,
    asked: BTreeSet<(String, String)>,
}

impl TabAdmission {
    /// Ask through the session's broker from now on, for the apps already
    /// waiting too.
    pub(super) fn bind(&mut self, tree: WorkspaceTreeHandle, opening: NativeAppOpening) {
        self.through = Some((tree, opening));
        for (room, app) in std::mem::take(&mut self.waiting) {
            self.admit(&room, &app);
        }
    }

    /// Open `app` in `room`, once per session: a room this device's native
    /// owner holds lets the app author there when this device's person may
    /// grant that. A room not held here (on this device only, or still
    /// joining) is asked again when a tab of it mounts once more.
    pub(super) fn admit(&mut self, room: &str, app: &str) {
        let key = (room.to_owned(), app.to_owned());
        let Some((tree, opening)) = &self.through else {
            self.waiting.insert(key);
            return;
        };
        let Some(parsed) = RoomId::parse(room) else {
            return;
        };
        if !tree.read().native_authority.holds(parsed) || !self.asked.insert(key) {
            return;
        }
        let (tree, opening, app) = (tree.clone(), opening.clone(), app.to_owned());
        let spawned = std::thread::Builder::new()
            .name("tab-app-admission".into())
            .spawn(move || {
                let admitted =
                    tilo_identity_host::broker::trusted_current_device(&parsed.to_string())
                        .map_err(|error| error.to_string())
                        .and_then(|(_, certificate)| {
                            let owner = tree
                                .read()
                                .installed_native_owner()
                                .ok_or("native app owner unavailable")?;
                            launcher::admit_native_app(
                                &owner,
                                parsed,
                                &certificate,
                                &app,
                                Some(&opening),
                            )
                        });
                if let Err(error) = admitted {
                    tracing::warn!(
                        room = %parsed,
                        app = %app,
                        %error,
                        "an app shown in a tab was not let author its room"
                    );
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "an app shown in a tab could not be admitted");
        }
    }
}
