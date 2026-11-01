//! Rows the desktop describes and the panel shows: the Devices section's
//! device fabric rows and the Clipboard section's copies.
//!
//! The desktop holds the device fabric and the clipboard history, so it
//! describes each row ([`HostedRow`]) while its section shows; the panel
//! lists the rows and names which of a row's buttons was pressed in its
//! effects ([`crate::RowPart`]).

use ui::icons::Icon;

/// What a row offers besides a press on its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedActions {
    None,
    /// The row's one action, named on a button (stop, cancel, try again).
    Select(String),
    /// Two choices: allow and decline.
    Pair {
        first: (Icon, String),
        second: (Icon, String),
    },
    /// Driving another device with this machine's pointer. Where its
    /// display stands is arranged in its settings pane, which a press on
    /// the row opens.
    Control {
        name: String,
        /// This machine's pointer is driving it now.
        controlling: bool,
        /// It can be driven: online and not this machine.
        controllable: bool,
    },
    /// A copy's pin and remove.
    Clipboard {
        pinned: bool,
        pin: String,
        remove: String,
    },
}

/// One of the person's devices, as the device fabric reads it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "what the fabric says of this device now: independent facts"
)]
pub struct PersonalDevice {
    /// The fabric id, which names the device in the panel's effects.
    pub id: String,
    pub label: String,
    /// It has a live session with this device now.
    pub online: bool,
    /// This machine.
    pub local: bool,
    /// It shows, so where its display stands beside this one's is arranged in
    /// its settings pane.
    pub display: bool,
    /// One of its interfaces wakes on a magic packet, so a device on its
    /// network can wake it while it sleeps.
    pub wakeable: bool,
    /// It plays the sound this device sends it.
    pub audio_output: bool,
}

impl PersonalDevice {
    /// The Settings page where this device is managed: where its display is
    /// placed beside this one's when it shows, else the person's devices and
    /// what they keep in step.
    #[must_use]
    pub fn settings_link(&self) -> &'static str {
        if self.display {
            "settings://displays"
        } else {
            "settings://devices_sync"
        }
    }
}

/// The Devices section's rows: one per device other than this one, in
/// `devices`' order. `playing` titled what plays here when another device
/// can take it, and `sending` says a sound of this device's is chosen for
/// one of them: each row that can take it says so, and every other row says
/// only where its device is, since pressing it opens that device's settings.
#[must_use]
pub fn device_rows(
    devices: &[PersonalDevice],
    playing: Option<&str>,
    sending: bool,
) -> Vec<HostedRow> {
    devices
        .iter()
        .filter(|device| !device.local)
        .map(|device| {
            let takes_sound = sending && device.online && device.audio_output;
            HostedRow {
                key: device.id.clone(),
                title: device.label.clone(),
                detail: crate::i18n::t(if takes_sound {
                    "qs-devices-send-here"
                } else if device.online {
                    "qs-devices-online"
                } else {
                    "qs-devices-offline"
                }),
                icon: Icon::Monitor,
                selected: false,
                enabled: true,
                actions: if takes_sound {
                    HostedActions::Select(crate::i18n::t("qs-devices-action-send"))
                } else {
                    match playing {
                        Some(title) if device.online => HostedActions::Select(crate::i18n::t_args(
                            "qs-devices-play-here",
                            &[("media", title.into())],
                        )),
                        _ if !device.online && device.wakeable => {
                            HostedActions::Select(crate::i18n::t("qs-devices-wake"))
                        }
                        _ => HostedActions::None,
                    }
                },
            }
        })
        .collect()
}

/// The row key prefix of one of this device's sounds.
pub const SOUND_KEY: &str = "audio:sound:";
/// The row key prefix of one route of a sound.
pub const ROUTE_KEY: &str = "audio:route:";
/// The row key prefix of one device asking for this device's sound.
pub const ASK_KEY: &str = "audio:ask:";

/// One sound of this device's that can be sent to another.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SendableSound {
    /// Names the sound in the panel's effects, and follows it while the
    /// person can still pick it.
    pub token: u64,
    pub label: String,
    /// One application's sound, or everything this device plays.
    pub application: bool,
}

/// One route of a sound, on its way or playing.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SoundRoute {
    /// Names the route in the panel's effects.
    pub id: String,
    /// The sound that travels, as the route names it.
    pub label: String,
    /// What the route is doing now, in words that name the other device.
    pub detail: String,
    /// The route stopped and is only shown until it is dismissed.
    pub stopped: bool,
}

/// Another of the person's devices asking to play a sound of this one's.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SoundAsk {
    /// Names the ask in the panel's effects.
    pub id: String,
    /// The sound it would play here.
    pub media: String,
    /// The device asking, as the person knows it.
    pub source: String,
}

/// The sounds this device can send, the routes of the sounds it sends or
/// receives, and the devices asking for its sound, as the remote audio
/// service reads them: the rows the Devices section lists above the
/// person's devices.
#[must_use]
pub fn remote_audio_rows(
    sounds: &[SendableSound],
    routes: &[SoundRoute],
    asks: &[SoundAsk],
    chosen: Option<u64>,
) -> Vec<HostedRow> {
    let sounds = sounds.iter().map(|sound| HostedRow {
        key: format!("{SOUND_KEY}{}", sound.token),
        title: sound.label.clone(),
        detail: crate::i18n::t(if sound.application {
            "qs-devices-audio-app"
        } else {
            "qs-devices-audio-choose"
        }),
        icon: Icon::Volume2,
        selected: chosen == Some(sound.token),
        enabled: true,
        actions: HostedActions::None,
    });
    let asks = asks.iter().map(|ask| HostedRow {
        key: format!("{ASK_KEY}{}", ask.id),
        title: crate::i18n::t_args(
            "qs-devices-play-here",
            &[("media", ask.media.as_str().into())],
        ),
        detail: crate::i18n::t_args(
            "qs-devices-requested-allow-once",
            &[("source", ask.source.as_str().into())],
        ),
        icon: Icon::Volume2,
        selected: false,
        enabled: true,
        actions: HostedActions::Pair {
            first: (Icon::Check, crate::i18n::t("qs-devices-allow-audio")),
            second: (Icon::X, crate::i18n::t("qs-devices-decline-audio")),
        },
    });
    let routes = routes.iter().map(|route| HostedRow {
        key: format!("{ROUTE_KEY}{}", route.id),
        title: route.label.clone(),
        detail: route.detail.clone(),
        icon: Icon::Volume2,
        selected: false,
        enabled: true,
        actions: HostedActions::Select(crate::i18n::t(if route.stopped {
            "qs-devices-action-dismiss"
        } else {
            "qs-devices-action-stop"
        })),
    });
    // What waits for an answer comes first, then what travels, then what can
    // travel: the person answers a device before choosing one.
    asks.chain(routes).chain(sounds).collect()
}

/// The same devices as an accessory's page lists where it can move to, this
/// device included and marked local.
#[must_use]
pub fn handoff_targets(devices: &[PersonalDevice]) -> Vec<crate::HandoffTarget> {
    devices
        .iter()
        .map(|device| crate::HandoffTarget {
            id: device.id.clone(),
            label: device.label.clone(),
            online: device.online,
            local: device.local,
        })
        .collect()
}

/// The Devices section as the session publishes it: the person's devices as
/// the device fabric reads them, what plays here that one of them can take,
/// and the sounds the remote audio service sends, receives and is asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DevicesReading {
    pub devices: Vec<PersonalDevice>,
    /// What plays here, titled, while media continuity is on: what a press on
    /// another device sends.
    pub playing: Option<String>,
    /// The sound of this device's that a press on a device would send.
    pub chosen: Option<u64>,
    pub sounds: Vec<SendableSound>,
    pub routes: Vec<SoundRoute>,
    pub asks: Vec<SoundAsk>,
}

impl DevicesReading {
    /// The Devices section's rows: what waits for an answer, what travels,
    /// what can travel, then the person's other devices.
    #[must_use]
    pub fn rows(&self) -> Vec<HostedRow> {
        let mut rows = remote_audio_rows(&self.sounds, &self.routes, &self.asks, self.chosen);
        rows.extend(device_rows(
            &self.devices,
            self.playing.as_deref(),
            self.chosen.is_some(),
        ));
        rows
    }

    /// The same devices as an accessory's page lists where it can move to.
    #[must_use]
    pub fn targets(&self) -> Vec<crate::HandoffTarget> {
        handoff_targets(&self.devices)
    }

    /// How many devices the fabric knows, this one included.
    #[must_use]
    pub fn count(&self) -> usize {
        self.devices.len()
    }
}

/// One row's face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedRow {
    /// Names the row in the panel's effects: what the desktop knows it by.
    pub key: String,
    pub title: String,
    pub detail: String,
    pub icon: Icon,
    /// The row is the chosen one (the media source the volume follows).
    pub selected: bool,
    pub enabled: bool,
    pub actions: HostedActions,
}

/// One copy the desktop keeps, as the Clipboard section lists it: the row's
/// face without its bytes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClipboardCopy {
    /// Names the copy in the panel's effects.
    pub key: String,
    /// The format its bytes are in.
    pub mime: String,
    /// Its first line, when it is text.
    pub text: Option<String>,
    /// When it was copied, in Unix milliseconds.
    pub created_ms: u64,
    /// Whether the person keeps it when the history is cleared.
    pub pinned: bool,
    /// The device another of the person's devices shared it from.
    pub from: Option<String>,
}

/// One notification the centre keeps, as the session publishes it: what it
/// says, when it arrived, and what it offers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeptNotification {
    /// Names it in the centre's actions.
    pub id: u64,
    /// The app group it is listed under, by its stable key.
    pub group: String,
    /// The app, as the group is named.
    pub app: String,
    pub title: String,
    pub body: String,
    /// When it arrived, in Unix milliseconds.
    pub created_ms: u64,
    /// The sender's choices, as `(key, label)`.
    pub actions: Vec<(String, String)>,
    /// Choosing it opens something (its sender, or its app); otherwise it
    /// is read in full in the panel.
    pub opens: bool,
}

/// The notification centre, as the session publishes it: what it keeps,
/// why fresh banners are held, and whether a Clear all can be undone.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotificationCentre {
    /// How many it keeps.
    pub count: usize,
    /// Newest first.
    pub rows: Vec<KeptNotification>,
    /// Why fresh notifications are being held (Do Not Disturb, presenting,
    /// a focus schedule); `None` while they show.
    pub quiet: Option<String>,
    /// The last Clear all can still be undone.
    pub can_undo: bool,
}

impl NotificationCentre {
    /// The centre as the panel lists it, as of `now_ms`.
    #[must_use]
    pub fn view(&self, now_ms: u64) -> crate::NotificationsView {
        crate::NotificationsView {
            rows: self
                .rows
                .iter()
                .map(|kept| crate::NotificationRow {
                    id: kept.id,
                    group: kept.group.clone(),
                    app: kept.app.clone(),
                    title: kept.title.clone(),
                    body: kept.body.clone(),
                    age: ui::format::relative(
                        i64::try_from(now_ms.saturating_sub(kept.created_ms) / 1000)
                            .unwrap_or(i64::MAX),
                    ),
                    // A row shows at most the two the banner showed it.
                    actions: kept.actions.iter().take(2).cloned().collect(),
                    // No agent is live on this desktop to hand one to.
                    agent: None,
                    opens: kept.opens,
                })
                .collect(),
            quiet: self.quiet.clone(),
            can_undo: self.can_undo,
        }
    }
}

/// Formats whose copies are pictures.
const IMAGE_MIMES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// The longest title a text copy shows before the line is cut.
const TITLE_CHARS: usize = 120;

/// The Clipboard section and its row for `copies`, newest first, as of
/// `now_ms`.
#[must_use]
pub fn clipboard_view(copies: &[ClipboardCopy], now_ms: u64) -> crate::ClipboardView {
    crate::ClipboardView {
        total: copies.len(),
        // The section says what it is showing: how many, or what is
        // searched for.
        caption: String::new(),
        rows: copies
            .iter()
            .map(|copy| clipboard_row(copy, now_ms))
            .collect(),
    }
}

/// One copy's row: what it holds, how long ago, and where it came from.
fn clipboard_row(copy: &ClipboardCopy, now_ms: u64) -> HostedRow {
    // The format says what the row is; the text is what it holds, which for
    // a copy of files is the first file's name.
    let files = ui::dnd::is_file_mime(&copy.mime);
    let title = if let Some(text) = copy.text.as_deref() {
        title_of(text)
    } else if files {
        crate::i18n::t("qs-copy-files")
    } else if IMAGE_MIMES.contains(&copy.mime.as_str()) {
        crate::i18n::t("qs-copy-image")
    } else {
        crate::i18n::t("qs-copy-formatted")
    };
    let icon = if files {
        Icon::Folder
    } else if IMAGE_MIMES.contains(&copy.mime.as_str()) {
        Icon::Image
    } else {
        Icon::FileText
    };
    let seconds = now_ms.saturating_sub(copy.created_ms) / 1000;
    let age = ui::format::relative(i64::try_from(seconds).unwrap_or(i64::MAX));
    let detail = match &copy.from {
        Some(device) => crate::i18n::t_args(
            "qs-copy-detail-from",
            &[("age", age.into()), ("device", device.as_str().into())],
        ),
        None => crate::i18n::t_args("qs-copy-detail-this-device", &[("age", age.into())]),
    };
    HostedRow {
        key: copy.key.clone(),
        title,
        detail,
        icon,
        selected: false,
        enabled: true,
        actions: HostedActions::Clipboard {
            pinned: copy.pinned,
            pin: String::new(),
            remove: String::new(),
        },
    }
}

/// A text copy's title: its first line with its runs of whitespace closed
/// up, cut at [`TITLE_CHARS`].
fn title_of(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let closed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if closed.chars().count() <= TITLE_CHARS {
        return closed;
    }
    let cut: String = closed.chars().take(TITLE_CHARS).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The format a text copy travels in.
    const TEXT: &str = "text/plain;charset=utf-8";

    fn copy(mime: &str, text: Option<&str>) -> ClipboardCopy {
        ClipboardCopy {
            key: "k".to_owned(),
            mime: mime.to_owned(),
            text: text.map(str::to_owned),
            created_ms: 0,
            pinned: false,
            from: None,
        }
    }

    /// One notification the centre keeps.
    fn kept(id: u64, title: &str, created_ms: u64, actions: &[(&str, &str)]) -> KeptNotification {
        KeptNotification {
            id,
            group: "mail".to_owned(),
            app: "Mail".to_owned(),
            title: title.to_owned(),
            body: "Two new messages".to_owned(),
            created_ms,
            actions: actions
                .iter()
                .map(|(key, label)| ((*key).to_owned(), (*label).to_owned()))
                .collect(),
            opens: true,
        }
    }

    #[test]
    fn the_panel_lists_the_centre_the_session_publishes() {
        // Case `the_centre_lists_what_arrived_named_after_its_app`:
        // A row says what arrived, from which app, how long ago, and offers
        // at most the two choices the banner offered.
        let now = 1_000_000;
        let centre = NotificationCentre {
            count: 2,
            rows: vec![
                kept(2, "New mail", now - 30_000, &[]),
                kept(
                    1,
                    "Backup finished",
                    now - 600_000,
                    &[
                        ("retry", "Try again"),
                        ("open", "Open"),
                        ("late", "Too late"),
                    ],
                ),
            ],
            quiet: Some("Do Not Disturb".to_owned()),
            can_undo: true,
        };
        let view = centre.view(now);
        assert_eq!(
            view.rows
                .iter()
                .map(|row| (row.title.as_str(), row.app.as_str()))
                .collect::<Vec<_>>(),
            [("New mail", "Mail"), ("Backup finished", "Mail")],
            "newest first, named after its app"
        );
        assert_eq!(
            view.rows[0].age,
            ui::format::relative(30),
            "a row's age is its own arrival's"
        );
        assert_eq!(view.rows[1].age, ui::format::relative(600));
        assert_eq!(
            view.rows[1]
                .actions
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["retry", "open"],
            "a row offers the two the banner offered"
        );
        assert_eq!(view.quiet.as_deref(), Some("Do Not Disturb"));
        assert!(view.can_undo, "the centre's Clear all can be undone");
    }

    #[test]
    fn a_copy_says_when_and_where_it_was_copied() {
        // Case `a_copy_says_which_device_it_came_from`:
        // A copy shared from another device names it, and one made here says
        // it came from this computer, both after how long ago it was made.
        let now = 1_000_000;
        let mut here = copy(TEXT, Some("made here"));
        here.created_ms = now - 30_000;
        let mut there = copy(TEXT, Some("made there"));
        there.created_ms = now - 30_000;
        there.from = Some("laptop".to_owned());
        let view = clipboard_view(&[here, there], now);
        assert!(
            view.rows[0].detail.ends_with(", from this computer"),
            "a copy made here: {}",
            view.rows[0].detail
        );
        assert!(
            view.rows[1].detail.ends_with(", from laptop"),
            "a copy shared from a laptop: {}",
            view.rows[1].detail
        );
        assert!(
            !view.rows[1].detail.starts_with(", from"),
            "the age comes first: {}",
            view.rows[1].detail
        );
    }

    #[test]
    fn a_copied_file_is_a_folder_row_named_after_the_first_file() {
        // Case `a_file_copy_lists_as_the_files_it_is`:
        // A copy of files is not formatted text: it is a folder, named by
        // the first file, so the row says what was copied.
        let view = clipboard_view(
            &[copy("text/uri-list", Some("holiday.png"))],
            ui::time::unix_now_ms(),
        );
        assert_eq!(view.rows[0].title, "holiday.png");
        assert_eq!(view.rows[0].icon, Icon::Folder);

        // Case `a_file_copy_without_a_name_still_says_files`:
        // With no path to show, the row still says files rather than
        // claiming a format it does not hold.
        let view = clipboard_view(&[copy("x-special/gnome-copied-files", None)], 0);
        assert_eq!(view.rows[0].icon, Icon::Folder);
        assert_ne!(view.rows[0].title, "");
    }
}
