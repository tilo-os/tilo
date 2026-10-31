//! What the desktop's layers ask of the native session: the bar's source
//! (the clock, the person, their notifications) and the rules of what the
//! person may change.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_action_policy::Role;
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::{
    ActiveMediaUse, ChromeIntent, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource,
    LinkKind, LinkStatus, MediaKind, OwnChip, SystemStatus, VolumeState,
};
use tilo_desktop::desktop_host::DesktopRules;
use tilo_desktop::desktop_host::{DesktopAsk, DesktopChrome};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::commands::{DesktopCommand, Standing, may_command, may_create_workspace};
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_surface::DesktopHandle;
use tilo_surface_host::{Placement, WidgetHost};

use crate::notifications::NoticeSignals;

/// The bar's view of the session: the desktop's tree, the wall clock, and
/// the person at this device.
pub(crate) struct SessionChrome {
    pub(crate) handle: DesktopHandle,
    /// Where the bar's intents go: the chrome carries them out.
    pub(crate) intents: Rc<RefCell<Vec<ChromeIntent>>>,
    pub(crate) person: String,
    /// What the bar reads of the notifications: the unread count, the
    /// silenced bell and each app's own badge.
    pub(crate) signals: Rc<NoticeSignals>,
    /// The session has no quick settings panel: the status cluster is the
    /// bell that opens the notification centre.
    pub(crate) notifications_only: bool,
    /// Who is here, when this session takes part in the collaboration
    /// plane: the bar's trees carry them as the active workspace's
    /// collaborators.
    pub(crate) sync: Option<Rc<crate::sync::Client>>,
    /// The device as the quick settings panel reads it: the status cluster's
    /// battery, network and volume glyphs, and the streams being captured.
    pub(crate) readings: crate::panels::Readings,
    /// `(tree revision, minute, the last wave heard)` the snapshot revision
    /// was last bumped for.
    seen: Cell<(u64, i64, Option<String>)>,
    revision: Cell<u64>,
}

impl SessionChrome {
    pub(crate) fn new(
        handle: DesktopHandle,
        intents: Rc<RefCell<Vec<ChromeIntent>>>,
        person: String,
        signals: Rc<NoticeSignals>,
        notifications_only: bool,
        sync: Option<Rc<crate::sync::Client>>,
        readings: crate::panels::Readings,
    ) -> Self {
        Self {
            handle,
            intents,
            person,
            signals,
            notifications_only,
            sync,
            readings,
            seen: Cell::new((u64::MAX, 0, None)),
            revision: Cell::new(0),
        }
    }

    /// The device as the quick settings panel last read it, in the bar's own
    /// terms: what its battery, network and volume glyphs draw and what its
    /// status cluster says of them. A session without a panel reads nothing,
    /// and draws no glyph (absence, never a zero).
    fn status(&self) -> SystemStatus {
        let feed = self.readings.borrow().clone();
        feed.map_or_else(SystemStatus::default, |feed| {
            system_status(&feed.current().status)
        })
    }
}

/// The bar's status from the panel's reading of the device: the shell's
/// `system_status`, field for field.
fn system_status(status: &tilo_quick_settings::SystemStatus) -> SystemStatus {
    SystemStatus {
        battery_percent: status.battery_percent(),
        battery_charging: status.battery_is_charging(),
        battery_low: status.battery_is_low(),
        battery_critical: status.battery_is_critical(),
        volume: status.volume.map(|volume| VolumeState {
            level: volume.level,
            muted: volume.muted,
        }),
        link: status.link.as_ref().map(|link| LinkStatus {
            kind: match link.kind {
                os_actions::LinkKind::Offline => LinkKind::Offline,
                os_actions::LinkKind::Wired => LinkKind::Wired,
                os_actions::LinkKind::Wifi => LinkKind::Wifi,
            },
            name: link.name.clone(),
            signal: link.signal,
        }),
        media: status
            .media
            .iter()
            .map(|media| ActiveMediaUse {
                client_id: media.client_id,
                label: media.label.clone(),
                kind: match media.kind {
                    os_actions::MediaKind::Microphone => MediaKind::Microphone,
                    os_actions::MediaKind::Camera => MediaKind::Camera,
                },
            })
            .collect(),
        summary: status.summary(),
    }
}

impl DesktopChromeSource for SessionChrome {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let clock = ClockSnapshot::now();
        let waved = self.sync.as_ref().and_then(|sync| sync.wave_notice());
        let seen = (
            self.handle.revision(),
            clock.unix_seconds.div_euclid(60),
            waved.as_ref().map(|notice| notice.from.clone()),
        );
        if self.seen.replace(seen.clone()) != seen {
            self.revision.set(self.revision.get().wrapping_add(1));
        }
        DesktopChromeSnapshot {
            revision: self.revision.get(),
            status: self.status(),
            clock,
            waved,
            own: Some(OwnChip {
                name: self.person.clone(),
                rgb: ui::presence::user_color(&self.person),
                identity: String::new(),
            }),
            notification_badge: self.signals.unread(),
            notifications_quiet: self.signals.quiet(),
            notifications_only: self.notifications_only,
            ..DesktopChromeSnapshot::default()
        }
    }

    fn tree(&self) -> DesktopTree {
        let mut tree = self.handle.read(Clone::clone);
        // Who is here, as the active workspace's collaborators: the bar's
        // faces and every roster read the tree, not a list of their own.
        if let Some(sync) = &self.sync {
            sync.show(&mut tree, &|_: &str| None);
        }
        tree
    }

    fn app_notification_badge(&self, app_id: &str) -> u32 {
        self.signals.badge(app_id)
    }

    fn intent(&self, intent: ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        true
    }
}

/// The person at this device owns its desktop: the desktop's one policy
/// answers what they may change, as it answers a room's owner.
pub(crate) struct SessionRules {
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    /// Who made each strip and opened each window.
    pub(crate) authors: RefCell<Authors>,
}

impl SessionRules {
    fn standing(&self) -> Standing<'_> {
        Standing {
            person: &self.person,
            role: Some(Role::Owner),
            layout_shared: true,
            members: None,
        }
    }
}

impl DesktopRules for SessionRules {
    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        self.person.clone()
    }

    fn may(&self, command: &DesktopCommand) -> Result<(), String> {
        let authors = self.authors.borrow();
        self.handle.read(|tree| {
            let workspace = tree
                .active_workspace()
                .ok_or_else(|| "There is no workspace open".to_owned())?;
            may_command(&self.standing(), &authors, workspace, command)
                .map_err(|refusal| refusal.to_string())
        })
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        may_create_workspace(&self.standing()).map_err(|refusal| refusal.to_string())
    }

    fn made_strip(&self, strip: tilo_desktop_model::StripId) {
        self.authors
            .borrow_mut()
            .insert(strip.0, self.person.clone());
    }
}

/// The chrome above the windows (the top bar, overview, menus) as the
/// session hosts it: its own tree beside the desk's, covering the outputs.
pub(crate) fn host(
    handle: &DesktopHandle,
    options: &crate::Options,
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    signals: Rc<NoticeSignals>,
    sync: Option<Rc<crate::sync::Client>>,
    readings: &crate::panels::Readings,
    overview: &crate::mount::overview::Overview,
) -> WidgetHost {
    let rules = Rc::new(SessionRules {
        handle: handle.clone(),
        person: options.person.clone(),
        authors: RefCell::new(std::collections::BTreeMap::new()),
    });
    let tree = handle.read(Clone::clone);
    // The bar's source hands its intents to the chrome it sits in.
    let source = Rc::new(SessionChrome::new(
        handle.clone(),
        Rc::clone(&intents),
        options.person.clone(),
        signals,
        options.quick_settings.is_none(),
        sync,
        Rc::clone(readings),
    ));
    let bar = TopBarChrome::from_tree(&tree)
        .with_chrome(source)
        .with_tilo_mark();
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        crate::GENERATION.into(),
        rules,
        bar,
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    )
    .with_intents(intents)
    .with_asks(asks)
    .with_overview(overview.flight(), overview.open_flag());
    chrome.set_shortcuts(options.config.shortcuts.clone());
    let keyboard = chrome.keyboard();
    let placement = Placement {
        origin: (0, 0),
        size: (1, 1),
        scale: options.scale,
    };
    let mut host = WidgetHost::new(Box::new(chrome), placement, false);
    host.set_theme((options.presentation)().theme);
    let pictures = overview.clone();
    host.with_window_pictures(Rc::new(move || pictures.pictures()))
        .covering_outputs()
        .with_keyboard(keyboard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use os_actions::{
        LinkKind as OsLinkKind, LinkStatus as OsLinkStatus, MediaKind as OsMediaKind,
    };
    use tilo_power_mgmt::{BatterySnapshot, BatteryState};
    use tilo_quick_settings::SystemStatus as Reading;

    fn reading() -> Reading {
        Reading {
            battery: Some(BatterySnapshot {
                present: true,
                percent: 42,
                state: BatteryState::Charging,
            }),
            battery_time: None,
            link: Some(OsLinkStatus {
                kind: OsLinkKind::Wifi,
                name: Some("Home".to_owned()),
                signal: 3,
            }),
            volume: Some(media::audio::default_node::VolumeState {
                level: 0.6,
                muted: false,
            }),
            media: vec![os_actions::ActiveMediaUse {
                client_id: 7,
                pid: 4242,
                app_id: "so.tilo.meet".to_owned(),
                label: "Meet".to_owned(),
                binary: std::path::PathBuf::from("/usr/bin/meet"),
                kind: OsMediaKind::Microphone,
            }],
        }
    }

    /// The bar's glyphs: the battery's charge, the network's name and
    /// strength, the volume's level, and the stream being captured.
    #[test]
    fn the_bar_reads_the_machines_battery_network_and_volume() {
        let status = system_status(&reading());
        assert_eq!(status.battery_percent(), Some(42));
        assert!(status.battery_is_charging());
        assert!(!status.battery_is_low());
        let link = status.link.clone().expect("a link");
        assert_eq!(link.kind, LinkKind::Wifi);
        assert_eq!(link.name.as_deref(), Some("Home"));
        assert_eq!(link.signal, 3);
        let volume = status.volume.expect("a level");
        assert!((volume.level - 0.6).abs() < f32::EPSILON);
        assert!(!volume.muted);
        assert!(!status.audio_is_silent());
        assert_eq!(status.media.len(), 1);
        assert_eq!(status.media[0].client_id, 7);
        assert_eq!(status.media[0].label, "Meet");
        assert_eq!(status.media[0].kind, MediaKind::Microphone);
    }

    /// A machine that read nothing (no panel mounted yet) draws no glyph:
    /// absence, never a zero battery or a full network.
    #[test]
    fn a_machine_with_no_readings_shows_no_glyph() {
        let status = system_status(&Reading::default());
        assert_eq!(status.battery_percent(), None);
        assert!(status.link.is_none());
        assert!(status.volume.is_none());
        assert!(status.media.is_empty());
    }
}
