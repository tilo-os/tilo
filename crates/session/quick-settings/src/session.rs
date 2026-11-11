//! The session's published resources, as the panel reaches them.
//!
//! The panel is an app of the session, so what it shows of the device is
//! what the session's services publish on their resources, and what a press
//! does is one of those resources' actions. Both go through the broker ([`ui::collaboration::SessionResource`]):
//! [`Watch`] opens each resource once, wakes the panel at every reading and
//! hands a press to the resource that owns it. Nothing here decides anything
//! about the device; it carries what the services say, both ways.

use std::time::{Duration, Instant};

use ui::WakeHandle;
use ui::collaboration::SessionResource;
use ui::serde_json::{Map, Value};

/// How long a resource the session did not answer is left before it is asked
/// for again: the panel shows nothing of a resource it never reached, so it
/// keeps asking rather than settle for nothing.
const RETRY: Duration = Duration::from_secs(1);

/// The connectivity service's reading of the radios, the links and the
/// networks in reach.
pub(crate) const NETWORK: &str = "network";
/// The connectivity service's reading of the Bluetooth adapter.
pub(crate) const BLUETOOTH: &str = "bluetooth";
/// The connectivity service's reading of the VPN profiles.
pub(crate) const VPN: &str = "vpn";
/// The audio service's reading of the sound.
pub(crate) const SOUND: &str = "sound";
/// The audio service's reading of the players.
pub(crate) const MEDIA: &str = "media";
/// The power service's reading of the pack.
pub(crate) const BATTERY: &str = "power:battery";
/// The power service's reading of the sleep policy: what holds the machine
/// awake, and the power profiles it offers.
pub(crate) const SLEEP: &str = "power:sleep";
/// The system service's reading of the update.
pub(crate) const SYSTEM_UPDATE: &str = "system-update";
/// The session's reading of its displays and how bright each is.
pub(crate) const DISPLAYS: &str = "session:displays";
/// The person's display settings on this device, as the settings service publishes them.
pub(crate) const DISPLAY_SETTINGS: &str = "settings:settings.display";
/// The notification centre's reading of what it keeps.
pub(crate) const NOTIFICATIONS: &str = "session:notifications";
/// The session's reading of the clipboard history.
pub(crate) const CLIPBOARD: &str = "session:clipboard";
/// The session's reading of which controls the panel shows, and in what order.
pub(crate) const QUICK_SETTINGS: &str = "session:quick-settings";
/// The session's reading of the person's devices.
pub(crate) const DEVICES: &str = "session:devices";
/// The session's own screen: locking it, the power menu.
pub(crate) const SCREEN: &str = "session:screen";
/// The session's night light.
pub(crate) const NIGHT_LIGHT: &str = "session:night-light";
/// The desk: opening an app, its search.
pub(crate) const DESK: &str = "desktop:desk";
/// The desk's chrome: showing the app that plays what plays.
pub(crate) const CHROME: &str = "desktop:chrome";

/// Every resource the panel shows or acts on, watched for its whole life.
/// The desk is here for its search alone: Settings opens through it, and the
/// app that plays a song opens through the chrome. A resource is opened
/// before its actions are asked of it, so the screen and the night light are
/// here even where the panel only presses them or only shows the switch.
pub(crate) const WATCHED: [&str; 17] = [
    NETWORK,
    BLUETOOTH,
    VPN,
    SOUND,
    MEDIA,
    BATTERY,
    SLEEP,
    SYSTEM_UPDATE,
    DISPLAYS,
    NOTIFICATIONS,
    CLIPBOARD,
    QUICK_SETTINGS,
    DEVICES,
    SCREEN,
    NIGHT_LIGHT,
    DESK,
    CHROME,
];

/// The session's resources as the panel reads and changes them.
pub(crate) struct Watch {
    opened: Vec<(&'static str, SessionResource)>,
    /// When a resource the session did not answer may be asked for again.
    again: Vec<(&'static str, Instant)>,
}

impl Watch {
    pub(crate) const fn new() -> Self {
        Self {
            opened: Vec::new(),
            again: Vec::new(),
        }
    }

    /// Ask the session for every resource the panel shows that is not
    /// reached yet and whose retry is due, and have `wake` woken at each
    /// reading any of them publishes. Opening waits for the first reading, so
    /// this runs off the frame thread. Whether one opened.
    pub(crate) fn open(&mut self, wake: &WakeHandle) -> bool {
        let mut opened = false;
        for resource in WATCHED {
            if self.reached(resource) {
                continue;
            }
            let now = Instant::now();
            if let Some((_, at)) = self.again.iter().find(|(waiting, _)| *waiting == resource)
                && now < *at
            {
                continue;
            }
            self.again.retain(|(waiting, _)| *waiting != resource);
            match SessionResource::open(resource) {
                Ok(watched) => {
                    watched.attach(wake.clone());
                    self.opened.push((resource, watched));
                    opened = true;
                }
                Err(error) => {
                    // A service that is not there yet is asked for again: the
                    // panel shows its part of the device once it publishes.
                    tracing::debug!(%error, %resource, "the session publishes no reading yet");
                    self.again
                        .push((resource, now.checked_add(RETRY).unwrap_or(now)));
                }
            }
        }
        opened
    }

    /// Whether the session's `resource` is reached.
    pub(crate) fn reached(&self, resource: &str) -> bool {
        self.opened.iter().any(|(open, _)| *open == resource)
    }

    /// Hand every reading published since the last pass to `land`, in
    /// resource order. Whether one arrived.
    pub(crate) fn drain(&self, land: &impl Fn(&'static str, &str)) -> bool {
        let mut arrived = false;
        for (resource, opened) in &self.opened {
            while let Some(reading) = opened.latest() {
                arrived = true;
                match reading {
                    Ok(text) => land(resource, &text),
                    Err(error) => tracing::warn!(%error, %resource, "the session's reading"),
                }
            }
        }
        arrived
    }

    /// Ask `resource` to carry out `action` with `arguments`. Blocking,
    /// bounded: call it off the frame thread.
    ///
    /// # Errors
    ///
    /// The resource is not published by the session, or the door refused the
    /// action, with what it said.
    pub(crate) fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<(), String> {
        let Some((_, opened)) = self.opened.iter().find(|(open, _)| *open == resource) else {
            return Err(format!("the session publishes no {resource}"));
        };
        opened.invoke(action, arguments)
    }
}
