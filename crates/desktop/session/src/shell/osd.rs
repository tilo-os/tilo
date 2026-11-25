//! The hardware keys' readout, as the shell that draws it reads it: the level a volume,
//! brightness or backlight key just set, or the state an airplane, night-light or transport key
//! just switched, shown on the display whose key changed it and gone a moment after the last
//! press.
//!
//! The session raises it ([`crate::Shared::show_readout`], or the readout's own `show` action) and
//! publishes it as `device.osd`, so the shell in tilolang draws the pill from the same words and
//! the same level whoever asked for it. Nothing here holds a view: it keeps what the last press
//! said and when it goes.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};
use tilo_session_core::desktop::osd::{OSD_RESOURCE, Osd, OsdShow, osd_resource};

/// The resource the readout is published as.
pub(crate) const RESOURCE: &str = "session:osd";

/// How long a readout stands after the last press that changed it.
pub(crate) const HOLD: Duration = Duration::from_millis(1_500);

/// The readout as a view of another language reads it (`device.osd`).
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Readout")]
pub(crate) struct OsdRoot;

pub(crate) use tilo_device_surfaces::osd::Readout;

/// The readout `osd` says, on the display `display` whose key changed it.
pub(crate) fn readout_of(osd: &Osd, display: Option<&str>) -> Readout {
    let kind = serde_json::to_value(osd.kind())
        .ok()
        .and_then(|kind| kind.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut readout = Readout {
        visible: true,
        kind,
        display: display.map(str::to_owned),
        ..Readout::default()
    };
    match osd {
        Osd::Level(level) => {
            readout.level = Some(f64::from(level.level.clamp(0.0, 1.0)));
            readout.on = level.on;
            if let Some(device) = &level.device {
                readout.name = Some(device.name.clone());
                readout.glyph = serde_json::to_value(device.glyph)
                    .ok()
                    .and_then(|glyph| glyph.as_str().map(str::to_owned));
            }
        }
        Osd::Hint(hint) => {
            readout.on = hint.on;
            readout.name.clone_from(&hint.name);
        }
    }
    readout
}

/// What the last press said, and when it goes.
#[derive(Debug, Default)]
pub(crate) struct Shown {
    readout: Option<Readout>,
    hide_at: Option<Instant>,
}

/// The readout, shared between the pass that raises it and the service that publishes it.
pub(crate) type Shared = Rc<RefCell<Shown>>;

impl Shown {
    /// Show `readout` now, holding it for [`HOLD`].
    pub(crate) fn show(&mut self, readout: Readout) {
        self.readout = Some(readout);
        self.hide_at = Instant::now().checked_add(HOLD);
    }

    /// What stands now: the readout while its hold lasts, else an empty one.
    fn now(&self) -> Readout {
        let Some(readout) = &self.readout else {
            return Readout::default();
        };
        let standing = self.hide_at.is_some_and(|hide_at| Instant::now() < hide_at);
        if standing {
            readout.clone()
        } else {
            Readout::default()
        }
    }

    /// When the readout stops standing, while one does.
    fn deadline(&self) -> Option<Instant> {
        self.readout.as_ref().and(self.hide_at)
    }
}

/// The readout as the session publishes it.
pub(crate) struct Served {
    shown: Shared,
    published: Option<Readout>,
}

impl Served {
    pub(crate) fn new(shown: Shared) -> Self {
        Self {
            shown,
            published: None,
        }
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            // A key, an agent or a script raises the readout through its published action, as any
            // caller would.
            osd_resource(),
            offer::<NoEffect>(
                tilo_resource::LiveRow::typed::<OsdRoot>(RESOURCE, crate::i18n::t("session-osd"))
                    .into_resource(),
            ),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| {
            let readout = self.published.clone().unwrap_or_default();
            reading(&readout)
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let address = ActionAddress::new(resource, action);
        if resource != OSD_RESOURCE {
            return Err(ApiError::Unknown(address));
        }
        let encoded = tilo_actions::api::encode_json(&[osd_resource()], &address, arguments)?;
        let show = OsdShow::from_payload(&encoded.payload)
            .map_err(|error| ApiError::Failed(error.to_string()))?;
        // The action's caller names the display it shows on; a key's press has the session say
        // which one changed, and both land in the same reading the shell draws.
        let display = arguments.get("display").and_then(Value::as_str);
        self.shown
            .borrow_mut()
            .show(readout_of(&show.osd(), display));
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        let now = self.shown.borrow().now();
        if self.published.as_ref() == Some(&now) {
            return false;
        }
        self.published = Some(now);
        true
    }

    fn wakes_in(&self) -> Option<Duration> {
        let deadline = self.shown.borrow().deadline()?;
        Some(deadline.saturating_duration_since(Instant::now()))
    }
}
