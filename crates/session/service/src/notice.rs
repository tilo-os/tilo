//! One notice a role tells the person, and where choosing it lands.
//!
//! A role that knows something the person should know — a mention of them in a document, a
//! filesystem running out — tells it as one [`Notice`] through [`Notices`]. What the session does
//! with it is its own: the native session posts it to the person's notifications, the page hands it
//! to the notifier it is served. What the person chose comes back through the same boundary, so a
//! role never reads a platform's notification log.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use settings_store::system_sounds::SystemSoundEvent;
use tilo_actions::MemberId;

/// The name a notice that opens something offers that choice under.
pub const OPEN_ACTION: &str = "open";

/// The tag of the next notice told under no name of its own: one per telling, so the same words
/// told twice are two notices.
static RAISED: AtomicU64 = AtomicU64::new(1);

/// One notice to tell the person: what it says, and what choosing it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The name it shows under: one per notice, so telling it twice tells it once and taking it
    /// back names it.
    pub tag: String,
    /// The name the person sees it came from ("Storage").
    pub app: String,
    /// `0` low, `1` normal, `2` critical: what the banner and the centre order it by.
    pub urgency: u8,
    pub title: String,
    pub body: String,
    /// Where choosing it lands, where it opens something ([`Self::opens`]).
    pub place: Option<Place>,
    /// What it is about, where the layout leads it with the glyph for that rather than the app's own
    /// mark ([`Self::about`]): the system's bell for the machine's own news, which is the default.
    pub leading: NotificationIconKind,
    /// `(key, label)` pairs, as a notification's actions.
    pub actions: Vec<(String, String)>,
    /// How long it stays on screen; `None` leaves that to the person's own notification settings.
    pub expires_after: Option<Duration>,
    /// Whether every display shows it, or only the display the person is using.
    pub machine_wide: bool,
    /// The event sound it makes, played by the host that shows it: a sender's category never
    /// reaches the one an app may not ask for ([`SystemSoundEvent::LowBattery`]).
    pub sound: Option<SystemSoundEvent>,
}

impl Notice {
    /// A notice `app` tells the person at `urgency`, about the whole machine.
    #[must_use]
    pub fn new(
        app: impl Into<String>,
        urgency: u8,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            tag: format!("notice:{}", RAISED.fetch_add(1, Ordering::Relaxed)),
            app: app.into(),
            urgency,
            title: title.into(),
            body: body.into(),
            place: None,
            leading: NotificationIconKind::System,
            actions: Vec::new(),
            expires_after: None,
            machine_wide: true,
            sound: None,
        }
    }

    /// The notice of `place`: what `app` says of it, and where choosing it lands.
    #[must_use]
    pub fn opens(
        app: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        place: Place,
    ) -> Self {
        Self {
            place: Some(place),
            ..Self::new(app, 1, title, body)
        }
    }

    /// Told under `tag`, so a later telling of the same thing replaces it.
    #[must_use]
    pub fn tagged(mut self, tag: impl Into<String>) -> Self {
        self.tag = tag.into();
        self
    }

    /// About `kind` of news, where it leads with that glyph rather than the app's own mark.
    #[must_use]
    pub fn about(mut self, kind: NotificationIconKind) -> Self {
        self.leading = kind;
        self
    }

    /// Offering the person `actions`, as `(key, label)` pairs.
    #[must_use]
    pub fn choosing(mut self, actions: Vec<(String, String)>) -> Self {
        self.actions = actions;
        self
    }

    /// Staying on screen `after`, or until the person lets it go for `Some(Duration::ZERO)`.
    #[must_use]
    pub fn staying(mut self, after: Option<Duration>) -> Self {
        self.expires_after = after;
        self
    }

    /// Shown on the display the person is using, rather than on every display.
    #[must_use]
    pub fn here(mut self) -> Self {
        self.machine_wide = false;
        self
    }

    /// Making `event`'s sound as it is shown.
    #[must_use]
    pub fn sounding(mut self, event: SystemSoundEvent) -> Self {
        self.sound = Some(event);
        self
    }
}

/// What a notification is about, when the desktop itself knows: plain information leads with its
/// glyph. `Generic` has none of its own, so the notification leads with its sender's mark, and with
/// the system's bell when no app is named. `System` is the desktop's own notice about the machine,
/// which leads with the same bell whoever raises it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotificationIconKind {
    #[default]
    Generic,
    System,
    Workspace,
    Permission,
}

/// Why a notice the person was shown is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// It ran out of time.
    Expired,
    /// The person dismissed it.
    Dismissed,
    /// The role took it back: what it told is gone.
    Withdrawn,
}

/// Showing a role's notices on one host, and taking back what the person chose.
pub trait Notices {
    /// Show `notice`, replacing one showing under its tag.
    fn show(&self, notice: &Notice);

    /// Take the notice under `tag` back: what it told is gone.
    fn withdraw(&self, tag: &str);

    /// The places the person chose since this was last taken.
    fn take_chosen(&self) -> Vec<Place> {
        Vec::new()
    }

    /// The actions the person chose since this was last taken, by the tag of the notice that
    /// offered them.
    fn take_chosen_actions(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The notices that closed since this was last taken, each with why.
    fn take_closed(&self) -> Vec<(String, Closed)> {
        Vec::new()
    }
}

/// Where choosing a notice lands: the app keeping a record, the record, and the item in it, opened
/// as a search hit is: the `open` of `resource`, told `anchor` as the item to show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Place {
    /// The app keeping the record, by package id.
    pub app: String,
    /// The record, as its route names it: empty for the app's own record of the workspace.
    pub record: String,
    /// The resource whose `open` shows the item, or empty for the record as a whole.
    pub resource: String,
    /// The item that `open` shows (`<kind>/<field>/<id>` for an item of a record), or empty for
    /// the resource itself.
    pub anchor: String,
}

/// The activation scheme a notice's [`Place`] is carried by: what a host that reports clicks hands back
/// when the person chooses it.
pub const SCHEME: &str = "tilo-mention";

/// What the tag of a mention's notice starts with ([`Place::tag`]): how a host showing them knows which
/// of its notifications are theirs.
pub const TAGS: &str = "mention:";

impl Place {
    /// The activation URI a host carries the place by:
    /// `tilo-mention:<app>?<record>#<resource>#<anchor>`, each part percent-encoded.
    #[must_use]
    pub fn activation(&self) -> String {
        format!(
            "{SCHEME}:{}?{}#{}#{}",
            encode(&self.app),
            encode(&self.record),
            encode(&self.resource),
            encode(&self.anchor)
        )
    }

    /// The place an [`Self::activation`] names.
    #[must_use]
    pub fn from_activation(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix(SCHEME)?.strip_prefix(':')?;
        let (app, rest) = rest.split_once('?')?;
        let (record, rest) = rest.split_once('#')?;
        let (resource, anchor) = rest.split_once('#')?;
        let place = Self {
            app: decode(app)?,
            record: decode(record)?,
            resource: decode(resource)?,
            anchor: decode(anchor)?,
        };
        (!place.app.is_empty()).then_some(place)
    }

    /// The tag a notice of this place for `person` goes by: the same on every device and window, so the
    /// host shows it once.
    #[must_use]
    pub fn tag(&self, person: &MemberId) -> String {
        format!(
            "{TAGS}{}/{}/{}/{}/{}",
            self.app,
            self.record,
            self.resource,
            self.anchor,
            person.id()
        )
    }
}

fn encode(part: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(part.len());
    for byte in part.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/' | b':' | b'@')
        {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn decode(part: &str) -> Option<String> {
    let mut out = Vec::with_capacity(part.len());
    let mut bytes = part.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(byte);
        }
    }
    String::from_utf8(out).ok()
}
