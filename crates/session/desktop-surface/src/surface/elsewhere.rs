//! A window that runs on another device, while nothing of it is shown here.
//!
//! The workspace's layout lists such a window on every shell as its device's
//! ([`tilo_desktop_model::TileContent::HostedWindow`]), shared or not, and a shell that holds none
//! of its pictures has nothing of the window to draw. Its tab says so, in the toolkit's own notice
//! for a place that holds nothing: which app's window it is and the device it runs on, and under
//! that why it is not here, as the host's session reads it ([`Elsewhere`]).
//!
//! The device goes by the name the shell already calls it, the one its tile carries. A device
//! nothing names is another device of the workspace: its key is never a name.

use tilo_desktop_model::{Tab, TileContent as ModelTile};
use ui::widget::TileContent;

use tilo_session_core::desktop::i18n::{t, t_args};

/// Why a window that runs on another device is not shown in its tab, as the host's session reads
/// it. The shell decides none of it: a session that reads its workspaces says which holds, and a
/// shell nobody told says only where the window runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Elsewhere {
    /// Nobody said how the window stands here: the tab says where it runs, and no more.
    #[default]
    Unread,
    /// Its device is in the workspace and shares no such window with it.
    NotShared,
    /// The workspace says its device shares it, and nothing of it has arrived here yet.
    Awaited {
        /// What it waits for, as the session's own reading of the share says it; empty while it
        /// waits for the device alone.
        waiting_for: String,
    },
    /// This device hears nothing of its device in the workspace.
    Away,
    /// The workspace no longer admits its device.
    Left,
}

/// `label` where it names a device: not blank, and not a key written out.
fn device_name(label: &str) -> Option<&str> {
    ui::presence::shown_name(label)
}

/// Where a window of `app_id` runs, as its tab's notice is titled: on the device called `device`,
/// or on another device of the workspace where nothing names it.
pub(super) fn title(app_id: &str, device: &str) -> String {
    let app = tilo_desktop_search::names::app_name(app_id);
    match device_name(device) {
        Some(device) => t_args(
            "peer-window-elsewhere",
            &[("app", app.as_str().into()), ("device", device.into())],
        ),
        None => t_args(
            "peer-window-elsewhere-unnamed",
            &[("app", app.as_str().into())],
        ),
    }
}

/// Why the window is not here, as the sentence under the notice's title; nothing where nobody
/// said.
fn reason(device: &str, elsewhere: &Elsewhere) -> String {
    let of_device = |named: &str, unnamed: &str| match device_name(device) {
        Some(device) => t_args(named, &[("device", device.into())]),
        None => t(unnamed),
    };
    match elsewhere {
        Elsewhere::Unread => String::new(),
        Elsewhere::NotShared => t("peer-window-not-shared"),
        Elsewhere::Awaited { waiting_for } if !waiting_for.trim().is_empty() => {
            waiting_for.trim().to_owned()
        }
        Elsewhere::Awaited { .. } => {
            of_device("peer-window-awaited", "peer-window-awaited-unnamed")
        }
        Elsewhere::Away => of_device("peer-window-host-away", "peer-window-host-away-unnamed"),
        Elsewhere::Left => of_device("peer-window-host-left", "peer-window-host-left-unnamed"),
    }
}

/// What `tab` shows of a window that runs on another device while no share of it stands there:
/// the toolkit's notice, saying where the window runs and why it is not here. `None` for a tab
/// that holds no such window.
pub(super) fn notice(tab: &Tab, elsewhere: &Elsewhere) -> Option<TileContent> {
    let ModelTile::HostedWindow {
        app_id, host_label, ..
    } = &tab.tile
    else {
        return None;
    };
    Some(TileContent::Notice {
        title: title(app_id, host_label),
        detail: reason(host_label, elsewhere),
    })
}
