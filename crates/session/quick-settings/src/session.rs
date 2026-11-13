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
/// The session's desk: the surface over the windows, which the panel opens
/// the notification centre on and Settings through, and which app a link to
/// what plays shows. Nothing of it is shown here, so none of its reading is
/// followed.
pub(crate) const DESK: &str = "session:desk";

/// Every resource the panel shows or acts on, watched for its whole life.
/// The desk is not here: the panel presses its actions and shows none of its
/// reading, so nothing of it crosses the wire. A resource is opened before
/// its actions are asked of it, so the screen and the night light are here
/// even where the panel only presses them or only shows the switch.
pub(crate) const WATCHED: [&str; 15] = [
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
    /// reading any of them publishes. A resource is followed whether or not
    /// its service is up now: one that starts later publishes, and the panel
    /// shows its part of the device then. Whether one was reached.
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
            match SessionResource::follow(resource, wake.clone()) {
                Ok(watched) => {
                    self.opened.push((resource, watched));
                    opened = true;
                }
                Err(error) => {
                    // The session's command bridge is not there yet: ask for
                    // it again, as nothing of it is followed until then.
                    tracing::debug!(%error, %resource, "the session's reading is not reached");
                    self.again
                        .push((resource, now.checked_add(RETRY).unwrap_or(now)));
                }
            }
        }
        opened
    }

    /// When a resource not reached yet is asked for again, if one waits.
    pub(crate) fn retry_at(&self) -> Option<Instant> {
        self.again.iter().map(|(_, at)| *at).min()
    }

    /// Whether the session's `resource` is reached.
    pub(crate) fn reached(&self, resource: &str) -> bool {
        self.opened.iter().any(|(open, _)| *open == resource)
    }

    /// Hand every reading published since the last pass to `land`, in
    /// resource order. Whether one the panel shows arrived.
    pub(crate) fn drain(&self, land: &impl Fn(&'static str, &str) -> bool) -> bool {
        let mut arrived = false;
        for (resource, opened) in &self.opened {
            while let Some(reading) = opened.latest() {
                match reading {
                    Ok(text) => arrived |= land(resource, &text),
                    // A resource no service of this device serves, as a machine
                    // without a battery has none: the panel shows no part of it
                    // and says nothing of it. Anything else is a reading that
                    // should have arrived, and is worth finding in the log.
                    Err(error) if absent(&error) => {
                        tracing::debug!(%error, %resource, "this device serves no such resource");
                    }
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
    /// Nothing on this device answers it, or the resource refused, in its own
    /// words ([`Refusal`]).
    pub(crate) fn invoke(
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<(), Refusal> {
        // The resource is pressed without being followed: the panel shows
        // nothing of the desk, so none of its reading is carried here.
        match SessionResource::invoke_without_watching(resource, action, arguments) {
            Ok(()) => Ok(()),
            Err(error) if absent(&error) => Err(Refusal::Nothing),
            Err(error) => Err(Refusal::Said(error)),
        }
    }
}

/// Why a press did not reach the device.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// No service of this device serves what the press asked for: the panel
    /// says so in its own words rather than in the wire's.
    Nothing,
    /// The resource answered, and this is what it said.
    Said(String),
}

/// Whether `error` says the session serves nothing under the resource: either
/// from the app's side of the wire, or with the door's own refusal.
fn absent(error: &str) -> bool {
    error.starts_with("this session publishes no ") || error.starts_with("No service publishes ")
}
