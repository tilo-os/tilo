//! `org.freedesktop.impl.portal.Notification` (version 2): a sandboxed
//! app's notifications.
//!
//! A portal notification becomes an ordinary notification of the session's
//! notifications daemon ([`Notifications`]): the same published resource
//! the banners and the notification centre read, with the same actions.
//! The app names its notification by a string id of its own; the backend
//! keeps the daemon's numeric id for each `(app, id)`, with what each of
//! its actions hands back, so an action the person (or anyone acting
//! through the published actions) invokes on it reaches the app as
//! `ActionInvoked(app_id, id, action, parameter)`.
//!
//! What a v2 notification carries is read and bounded here: title, body
//! (or its markup, reduced to plain text), the default action and up to
//! three buttons with their targets (a button whose purpose is
//! `im.reply-with-text` is the notification's inline reply), the
//! priority, the category, the display hints, the icon (a themed name, or
//! an image as bytes or a file descriptor, decoded into pixels) and the
//! sound (silence, or the session's own cue for any other).
//!
//! Only the frontend may call it ([`crate::auth`]); each call is for the
//! app it authenticated, which may replace or withdraw only its own
//! notifications.

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::FileExt as _;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use tilo_notifications_service::{INLINE_REPLY, ImageData, Notification, Notifications, Taken};
use tokio::sync::mpsc;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

use crate::{OBJECT_PATH, Portal, option};

/// The most portal notifications kept at once; the oldest is withdrawn to
/// make room for a new one.
const MAX_ACTIVE: usize = 256;
const MAX_ID_BYTES: usize = 255;
const MAX_TITLE_CHARS: usize = 512;
const MAX_BODY_CHARS: usize = 16 * 1024;
const MAX_BUTTONS: usize = 3;
const MAX_LABEL_CHARS: usize = 80;
const MAX_CATEGORY_BYTES: usize = 128;
/// The button purpose that makes a button the inline reply.
pub const REPLY_WITH_TEXT: &str = "im.reply-with-text";
/// The daemon's key for a notification's default action.
const DEFAULT: &str = "default";
/// What the default action is listed as until the session says it in the
/// person's language.
const DEFAULT_LABEL: &str = "Open";

/// The categories the backend reads: what they stand for decides the
/// sound and how a banner treats it.
const CATEGORIES: [&str; 8] = [
    "im.message",
    "alarm.ringing",
    "call.incoming",
    "call.ongoing",
    "call.missed",
    "weather.warning.extreme",
    "browser.web-notification",
    "os.battery.low",
];

/// What one of a notification's actions hands back to its app.
struct Route {
    /// The app's own name for the action.
    action: String,
    /// The value the app attached to it.
    target: Option<OwnedValue>,
}

impl Route {
    fn target(&self) -> Option<OwnedValue> {
        self.target
            .as_ref()
            .and_then(|value| value.try_clone().ok())
    }
}

/// One portal notification the daemon shows.
struct Record {
    app_id: String,
    id: String,
    numeric: u32,
    /// By the daemon's action key.
    routes: HashMap<String, Route>,
}

/// The portal notifications shown, oldest first.
#[derive(Clone, Default)]
struct Records(Arc<Mutex<VecDeque<Record>>>);

impl Records {
    /// Keep `(app_id, id)` with `routes`: under its numeric id when it is
    /// shown already, else under one `alloc` makes. Its numeric id, and the
    /// one it pushed out, if any.
    fn upsert(
        &self,
        app_id: &str,
        id: &str,
        routes: HashMap<String, Route>,
        alloc: impl FnOnce() -> u32,
    ) -> (u32, Option<u32>) {
        let mut records = self.0.lock_recover();
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.app_id == app_id && record.id == id)
        {
            record.routes = routes;
            return (record.numeric, None);
        }
        let evicted = (records.len() >= MAX_ACTIVE)
            .then(|| records.pop_front().map(|record| record.numeric))
            .flatten();
        let numeric = alloc();
        records.push_back(Record {
            app_id: app_id.to_owned(),
            id: id.to_owned(),
            numeric,
            routes,
        });
        drop(records);
        (numeric, evicted)
    }

    /// Forget `(app_id, id)`; its numeric id.
    fn remove(&self, app_id: &str, id: &str) -> Option<u32> {
        let mut records = self.0.lock_recover();
        let index = records
            .iter()
            .position(|record| record.app_id == app_id && record.id == id)?;
        records.remove(index).map(|record| record.numeric)
    }

    /// The `ActionInvoked` what the person did with notification `numeric`
    /// makes, when it is a portal notification and offered that.
    fn invoked(&self, numeric: u32, taken: Taken<'_>) -> Option<Invoked> {
        let mut platform_data: HashMap<&str, Value<'_>> = HashMap::new();
        let key = match taken {
            Taken::Action {
                key,
                activation_token,
            } => {
                if let Some(token) = activation_token {
                    platform_data.insert("activation-token", Value::from(token));
                }
                key
            }
            Taken::Reply { text } => {
                platform_data.insert("response", Value::from(text));
                INLINE_REPLY
            }
        };
        let found = self
            .0
            .lock_recover()
            .iter()
            .find(|record| record.numeric == numeric)
            .and_then(|record| {
                let route = record.routes.get(key)?;
                Some((
                    record.app_id.clone(),
                    record.id.clone(),
                    route.action.clone(),
                    route.target(),
                ))
            });
        let (app_id, id, action, target) = found?;
        let mut parameter: Vec<OwnedValue> = target.into_iter().collect();
        parameter.push(OwnedValue::try_from(Value::new(platform_data)).ok()?);
        Some(Invoked {
            app_id,
            id,
            action,
            parameter,
        })
    }

    #[cfg(test)]
    fn keys(&self) -> Vec<(String, String)> {
        self.0
            .lock_recover()
            .iter()
            .map(|record| (record.app_id.clone(), record.id.clone()))
            .collect()
    }
}

/// An `ActionInvoked` on its way to the app.
#[derive(Debug, PartialEq)]
struct Invoked {
    app_id: String,
    id: String,
    action: String,
    parameter: Vec<OwnedValue>,
}

/// The `org.freedesktop.impl.portal.Notification` interface.
pub struct NotificationPortal {
    portal: Portal,
    notifications: Notifications,
    records: Records,
}

/// Serve the notification interface for `portal` on `connection`, showing
/// what apps send through `notifications`, and send their actions back
/// while the connection lives.
///
/// # Errors
/// The object server refused the interface.
pub async fn serve_on(
    portal: &Portal,
    connection: &zbus::Connection,
    notifications: Notifications,
) -> zbus::Result<()> {
    let records = Records::default();
    let (invoked, mut pending) = mpsc::unbounded_channel::<Invoked>();
    let tapped = records.clone();
    notifications.on_taken(move |numeric, taken| {
        if let Some(signal) = tapped.invoked(numeric, taken) {
            let _ = invoked.send(signal);
        }
    });
    connection
        .object_server()
        .at(
            OBJECT_PATH,
            NotificationPortal {
                portal: portal.clone(),
                notifications,
                records,
            },
        )
        .await?;
    let emitter = SignalEmitter::new(connection, OBJECT_PATH)?.into_owned();
    tokio::spawn(async move {
        while let Some(signal) = pending.recv().await {
            let sent = NotificationPortal::action_invoked(
                &emitter,
                &signal.app_id,
                &signal.id,
                &signal.action,
                signal.parameter,
            )
            .await;
            if let Err(error) = sent {
                tracing::debug!(%error, "portal notification action not sent");
            }
        }
    });
    Ok(())
}

impl NotificationPortal {
    /// Whether the call is the frontend's, for `app_id`.
    async fn authenticated(&self, header: &zbus::message::Header<'_>, app_id: &str) -> bool {
        let sender = header
            .sender()
            .cloned()
            .map(zbus::names::OwnedUniqueName::from);
        self.portal
            .authenticator
            .authenticate(sender.as_ref(), app_id, None)
            .await
            .inspect_err(|error| tracing::debug!(%error, app_id, "portal notification refused"))
            .is_ok()
    }
}

#[interface(name = "org.freedesktop.impl.portal.Notification")]
impl NotificationPortal {
    /// Show notification `id` of `app_id`, or replace it.
    async fn add_notification(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        app_id: String,
        id: String,
        notification: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        if !self.authenticated(&header, &app_id).await {
            return Err(zbus::fdo::Error::AccessDenied(
                "untrusted portal caller".into(),
            ));
        }
        if !valid_id(&id) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "invalid notification id".into(),
            ));
        }
        let (actions, routes) = actions(&notification);
        let icon = icon(&notification).await;
        let (numeric, evicted) = self
            .records
            .upsert(&app_id, &id, routes, || self.notifications.alloc_id());
        if let Some(evicted) = evicted {
            self.notifications.request_close(evicted);
        }
        let mut shown = shown(numeric, &app_id, &notification, actions);
        match icon {
            Icon::Themed(name) => shown.app_icon = Some(name),
            Icon::Pixels(pixels) => shown.image_data = Some(pixels),
            Icon::None => {}
        }
        self.notifications.push(shown);
        Ok(())
    }

    /// Withdraw notification `id` of `app_id`: only that app's.
    async fn remove_notification(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        app_id: String,
        id: String,
    ) -> zbus::fdo::Result<()> {
        if !self.authenticated(&header, &app_id).await {
            return Err(zbus::fdo::Error::AccessDenied(
                "untrusted portal caller".into(),
            ));
        }
        if let Some(numeric) = self.records.remove(&app_id, &id) {
            self.notifications.request_close(numeric);
        }
        Ok(())
    }

    /// What the backend reads of a notification beyond the basics.
    #[zbus(property)]
    fn supported_options(&self) -> HashMap<String, OwnedValue> {
        let mut options = HashMap::new();
        if let Ok(categories) = OwnedValue::try_from(Value::new(CATEGORIES.to_vec())) {
            options.insert("category".to_owned(), categories);
        }
        if let Ok(purposes) = OwnedValue::try_from(Value::new(vec![REPLY_WITH_TEXT])) {
            options.insert("button-purpose".to_owned(), purposes);
        }
        options
    }

    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    /// The person invoked `action` on notification `id` of `app_id`.
    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        app_id: &str,
        id: &str,
        action: &str,
        parameter: Vec<OwnedValue>,
    ) -> zbus::Result<()>;
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES && !id.chars().any(char::is_control)
}

fn valid_action(action: &str) -> bool {
    !action.is_empty() && action.len() <= 255 && !action.chars().any(char::is_control)
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn clone_field(values: &HashMap<String, OwnedValue>, key: &str) -> Option<OwnedValue> {
    values.get(key).and_then(|value| value.try_clone().ok())
}

/// The daemon's notification `numeric` for what `app_id` sent as
/// `notification`, offering `actions`.
fn shown(
    numeric: u32,
    app_id: &str,
    notification: &HashMap<String, OwnedValue>,
    actions: Vec<(String, String)>,
) -> Notification {
    let display_hints = option::<Vec<String>>(notification, "display-hint").unwrap_or_default();
    let hinted = |hint: &str| display_hints.iter().any(|given| given == hint);
    let category = option::<String>(notification, "category").filter(|category| {
        !category.is_empty()
            && category.len() <= MAX_CATEGORY_BYTES
            && !category.chars().any(char::is_control)
    });
    Notification {
        app_name: app_id.to_owned(),
        summary: truncate_chars(
            &option::<String>(notification, "title").unwrap_or_default(),
            MAX_TITLE_CHARS,
        ),
        body: body(notification),
        actions,
        expire_timeout: if hinted("persistent") { 0 } else { -1 },
        urgency: urgency(notification),
        caller: Some(tilo_notifications_service::Caller {
            app_id: app_id.to_owned(),
            workspace: None,
        }),
        incoming_call: category.as_deref() == Some("call.incoming"),
        category,
        suppress_sound: silent(notification),
        transient: hinted("transient"),
        ..Notification::new(numeric)
    }
}

/// `priority`: `low`, `normal`, `high` or `urgent`, as the specification's
/// urgency. Anything else is normal.
fn urgency(notification: &HashMap<String, OwnedValue>) -> u8 {
    match option::<String>(notification, "priority").as_deref() {
        Some("low") => 0,
        Some("high" | "urgent") => 2,
        _ => 1,
    }
}

/// `sound`: `"silent"` asks for none. `"default"` and a sound of the app's
/// own (by file descriptor) play the session's cue for the category.
fn silent(notification: &HashMap<String, OwnedValue>) -> bool {
    notification
        .get("sound")
        .is_some_and(|sound| matches!(&**sound, Value::Str(name) if name.as_str() == "silent"))
}

fn body(notification: &HashMap<String, OwnedValue>) -> String {
    let plain = option::<String>(notification, "body").unwrap_or_default();
    let markup = option::<String>(notification, "markup-body");
    truncate_chars(
        &markup.map_or(plain, |markup| markup_to_plain_text(&markup)),
        MAX_BODY_CHARS,
    )
}

/// The daemon's `(key, label)` actions for `notification` and what each
/// hands back: the default action under the daemon's `default` key, each
/// button under a key of its own, and a reply button as the inline reply.
fn actions(
    notification: &HashMap<String, OwnedValue>,
) -> (Vec<(String, String)>, HashMap<String, Route>) {
    let mut listed = Vec::new();
    let mut routes = HashMap::new();
    if let Some(action) =
        option::<String>(notification, "default-action").filter(|action| valid_action(action))
    {
        routes.insert(
            DEFAULT.to_owned(),
            Route {
                action,
                target: clone_field(notification, "default-action-target"),
            },
        );
        listed.push((DEFAULT.to_owned(), DEFAULT_LABEL.to_owned()));
    }
    let buttons =
        option::<Vec<HashMap<String, OwnedValue>>>(notification, "buttons").unwrap_or_default();
    for (index, button) in buttons.into_iter().take(MAX_BUTTONS).enumerate() {
        let Some(action) = option::<String>(&button, "action").filter(|v| valid_action(v)) else {
            continue;
        };
        let Some(label) = option::<String>(&button, "label")
            .filter(|label| !label.trim().is_empty())
            .map(|label| truncate_chars(&label, MAX_LABEL_CHARS))
        else {
            continue;
        };
        let replies = option::<String>(&button, "purpose").as_deref() == Some(REPLY_WITH_TEXT);
        let key = if replies {
            INLINE_REPLY.to_owned()
        } else {
            format!("button-{index}")
        };
        if routes.contains_key(&key) {
            continue;
        }
        routes.insert(
            key.clone(),
            Route {
                action,
                target: clone_field(&button, "target"),
            },
        );
        listed.push((key, label));
    }
    (listed, routes)
}

/// A notification's icon, as the daemon shows it.
#[derive(Debug, PartialEq)]
enum Icon {
    None,
    /// A name of the icon theme.
    Themed(String),
    /// An image the app handed over, decoded.
    Pixels(ImageData),
}

/// `icon`: a serialised icon, `("themed", as)`, `("bytes", ay)` or
/// `("file-descriptor", h)`, or for historical reasons a bare icon name.
async fn icon(notification: &HashMap<String, OwnedValue>) -> Icon {
    let Some(icon) = notification.get("icon") else {
        return Icon::None;
    };
    let (kind, value) = match &**icon {
        Value::Str(name) => return themed(std::iter::once(name.as_str())),
        Value::Structure(structure) => match structure.fields() {
            [Value::Str(kind), Value::Value(value)] => (kind.as_str(), &**value),
            _ => return Icon::None,
        },
        _ => return Icon::None,
    };
    match (kind, value) {
        ("themed", Value::Array(names)) => themed(names.iter().filter_map(|name| match name {
            Value::Str(name) => Some(name.as_str()),
            _ => None,
        })),
        ("bytes", Value::Array(_)) => {
            let Ok(bytes) = Vec::<u8>::try_from(value.try_clone().unwrap_or(Value::Bool(false)))
            else {
                return Icon::None;
            };
            decoded(move || tilo_notifications_service::artwork::decode(&bytes)).await
        }
        ("file-descriptor", Value::Fd(fd)) => {
            let Some(file) = fd
                .try_to_owned()
                .ok()
                .and_then(|fd| std::os::fd::OwnedFd::try_from(fd).ok())
                .map(std::fs::File::from)
            else {
                return Icon::None;
            };
            decoded(move || tilo_notifications_service::artwork::read(Positioned::new(file))).await
        }
        _ => Icon::None,
    }
}

/// The first usable name of `names`.
fn themed<'a>(mut names: impl Iterator<Item = &'a str>) -> Icon {
    names
        .find(|name| {
            !name.is_empty()
                && name.len() <= 255
                && !name.contains('/')
                && !name.chars().any(char::is_control)
        })
        .map_or(Icon::None, |name| Icon::Themed(name.to_owned()))
}

/// The pixels `decode` makes, off the bus's threads.
async fn decoded(decode: impl FnOnce() -> Option<ImageData> + Send + 'static) -> Icon {
    tokio::task::spawn_blocking(decode)
        .await
        .ok()
        .flatten()
        .map_or(Icon::None, Icon::Pixels)
}

/// A file read from its start whatever its descriptor's offset: the
/// frontend's sealed memory file may have been written through it.
struct Positioned {
    file: std::fs::File,
    at: u64,
}

impl Positioned {
    const fn new(file: std::fs::File) -> Self {
        Self { file, at: 0 }
    }
}

impl std::io::Read for Positioned {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.file.read_at(buf, self.at)?;
        self.at = self.at.saturating_add(read as u64);
        Ok(read)
    }
}

/// Render the deliberately tiny portal markup language as plain text.
fn markup_to_plain_text(markup: &str) -> String {
    let mut plain = String::with_capacity(markup.len());
    let mut chars = markup.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            for next in chars.by_ref() {
                if next == '>' {
                    break;
                }
            }
            continue;
        }
        if ch == '&' {
            let mut entity = String::new();
            while entity.len() <= 6 {
                let Some(next) = chars.peek().copied() else {
                    break;
                };
                chars.next();
                if next == ';' {
                    break;
                }
                entity.push(next);
            }
            match entity.as_str() {
                "amp" => plain.push('&'),
                "lt" => plain.push('<'),
                "gt" => plain.push('>'),
                "quot" => plain.push('"'),
                "apos" => plain.push('\''),
                _ => plain.push('\u{fffd}'),
            }
            continue;
        }
        plain.push(ch);
    }
    plain
}

#[cfg(test)]
mod tests;
