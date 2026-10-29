//! The D-Bus face of the daemon: the `org.freedesktop.Notifications`
//! interface, its signals, and the connection that serves them.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use futures_util::StreamExt as _;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zbus::message::{Flags, Header, Message};
use zbus::names::{InterfaceName, MemberName};
use zbus::object_server::{DispatchResult2, SignalEmitter};
use zbus::zvariant::OwnedValue;
use zbus::{Connection, fdo};

use crate::hints::{self, Hints};
use crate::{BUS_NAME, Caller, Callers, Notification, Notifications, OBJECT_PATH, Sender, Signal};

/// The capabilities the daemon advertises: a multi-line body, actions,
/// notifications that stay until closed, and inline replies.
const CAPABILITIES: [&str; 4] = ["body", "actions", "persistence", "inline-reply"];
/// The version of the specification the daemon follows.
const SPEC_VERSION: &str = "1.2";

/// The interface one served connection carries.
struct Interface {
    notifications: Notifications,
    callers: Arc<dyn Callers>,
}

/// The app behind a sender, and whether only its sandbox proved it: a
/// Flatpak's own metadata names its app for policy, but not for the Tilo
/// extensions (badges, contact handles, ongoing notifications), which only
/// an app the session launched may use.
struct Attribution {
    caller: Option<Caller>,
    foreign: bool,
}

impl Interface {
    async fn attribute(&self, connection: &Connection, unique_name: Option<&str>) -> Attribution {
        let pid = match unique_name {
            Some(name) => sender_pid(connection, name).await,
            None => None,
        };
        if let Some(app_id) = pid.and_then(crate::flatpak::verified_flatpak_app_id) {
            return Attribution {
                caller: Some(Caller {
                    app_id,
                    workspace: None,
                }),
                foreign: true,
            };
        }
        Attribution {
            caller: self.callers.caller(Sender { unique_name, pid }),
            foreign: false,
        }
    }
}

/// The process behind `unique_name`, as the bus daemon reports it; `None`
/// on a peer-to-peer link, which has no bus daemon to ask.
async fn sender_pid(connection: &Connection, unique_name: &str) -> Option<u32> {
    connection.unique_name()?;
    let bus = fdo::DBusProxy::new(connection).await.ok()?;
    let name = zbus::names::BusName::try_from(unique_name).ok()?;
    bus.get_connection_unix_process_id(name).await.ok()
}

/// `Notify`'s wire arguments, in the specification's order: `app_name`,
/// `replaces_id`, `app_icon`, `summary`, `body`, `actions`, `hints`,
/// `expire_timeout`.
pub(crate) type NotifyArgs = (
    String,
    u32,
    String,
    String,
    String,
    Vec<String>,
    HashMap<String, OwnedValue>,
    i32,
);

/// The interface's introspection. Written out rather than generated:
/// `Notify` takes more arguments than a Rust signature should.
pub(crate) const INTROSPECTION: &str = r#"<interface name="org.freedesktop.Notifications">
  <method name="Notify">
    <arg name="app_name" type="s" direction="in"/>
    <arg name="replaces_id" type="u" direction="in"/>
    <arg name="app_icon" type="s" direction="in"/>
    <arg name="summary" type="s" direction="in"/>
    <arg name="body" type="s" direction="in"/>
    <arg name="actions" type="as" direction="in"/>
    <arg name="hints" type="a{sv}" direction="in"/>
    <arg name="expire_timeout" type="i" direction="in"/>
    <arg name="id" type="u" direction="out"/>
  </method>
  <method name="CloseNotification">
    <arg name="id" type="u" direction="in"/>
  </method>
  <method name="GetCapabilities">
    <arg name="capabilities" type="as" direction="out"/>
  </method>
  <method name="GetServerInformation">
    <arg name="name" type="s" direction="out"/>
    <arg name="vendor" type="s" direction="out"/>
    <arg name="version" type="s" direction="out"/>
    <arg name="spec_version" type="s" direction="out"/>
  </method>
  <signal name="NotificationClosed">
    <arg name="id" type="u"/>
    <arg name="reason" type="u"/>
  </signal>
  <signal name="ActionInvoked">
    <arg name="id" type="u"/>
    <arg name="action_key" type="s"/>
  </signal>
  <signal name="ActivationToken">
    <arg name="id" type="u"/>
    <arg name="activation_token" type="s"/>
  </signal>
  <signal name="NotificationReplied">
    <arg name="id" type="u"/>
    <arg name="text" type="s"/>
  </signal>
</interface>
"#;

impl Interface {
    /// Show a notification; answers its id.
    async fn notify(&self, header: &Header<'_>, connection: &Connection, args: NotifyArgs) -> u32 {
        let (app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout) = args;
        let sender = header.sender().map(ToString::to_string);
        let id = if replaces_id != 0
            && self
                .notifications
                .may_change(replaces_id, sender.as_deref())
        {
            replaces_id
        } else {
            self.notifications.alloc_id()
        };
        let Attribution { caller, foreign } = self.attribute(connection, sender.as_deref()).await;
        let ongoing_owner = (caller.is_some() && !foreign && hints::parse_ongoing(&hints))
            .then_some(sender)
            .flatten();
        let notification = notification(
            id,
            Request {
                app_name,
                app_icon,
                summary,
                body,
                actions,
                hints,
                expire_timeout: if ongoing_owner.is_some() {
                    0
                } else {
                    expire_timeout
                },
            },
            caller,
            foreign,
        );
        tracing::debug!(
            target: "notifications",
            id,
            app = notification.caller.as_ref().map(|caller| caller.app_id.as_str()),
            "notification arrived"
        );
        self.notifications.arrive(notification, ongoing_owner);
        id
    }

    /// Close a notification the app sent.
    fn close_notification(&self, header: &Header<'_>, id: u32) -> fdo::Result<()> {
        let sender = header.sender().map(zbus::names::UniqueName::as_str);
        if !self.notifications.may_change(id, sender) {
            return Err(fdo::Error::AccessDenied(
                "ongoing notification belongs to another sender".into(),
            ));
        }
        self.notifications.request_close(id);
        Ok(())
    }

    /// Route one method call.
    fn dispatch<'call>(
        &'call self,
        connection: &'call Connection,
        message: &'call Message,
        member: &str,
    ) -> DispatchResult2<'call> {
        match member {
            "Notify" => reply_to(connection, message, async move {
                let args: NotifyArgs = message.body().deserialize()?;
                Ok(self.notify(&message.header(), connection, args).await)
            }),
            "CloseNotification" => reply_to(connection, message, async move {
                let id: u32 = message.body().deserialize()?;
                self.close_notification(&message.header(), id)
            }),
            "GetCapabilities" => reply_to(connection, message, async move {
                Ok(CAPABILITIES.map(ToOwned::to_owned).to_vec())
            }),
            "GetServerInformation" => reply_to(connection, message, async move {
                Ok((
                    "Tilo Notifications".to_owned(),
                    "Tilo".to_owned(),
                    env!("CARGO_PKG_VERSION").to_owned(),
                    SPEC_VERSION.to_owned(),
                ))
            }),
            _ => DispatchResult2::NotFound,
        }
    }
}

/// Answer `message` with what `reply` resolves to, unless the caller
/// expects no reply.
fn reply_to<'call, Reply>(
    connection: &'call Connection,
    message: &'call Message,
    reply: impl Future<Output = fdo::Result<Reply>> + Send + 'call,
) -> DispatchResult2<'call>
where
    Reply: serde::Serialize + zbus::zvariant::DynamicType + Send + Sync,
{
    DispatchResult2::Async(Box::pin(async move {
        let reply = reply.await;
        let header = message.header();
        if header.primary().flags().contains(Flags::NoReplyExpected) {
            return Ok(());
        }
        match reply {
            Ok(reply) => connection.reply(&header, &reply).await,
            Err(error) => connection.reply_dbus_error(&header, error).await,
        }
        .map_err(|error| match error {
            zbus::Error::FDO(error) => *error,
            error => fdo::Error::Failed(error.to_string()),
        })
    }))
}

#[zbus::export::async_trait::async_trait]
impl zbus::object_server::Interface for Interface {
    fn name() -> InterfaceName<'static> {
        InterfaceName::from_static_str_unchecked(BUS_NAME)
    }

    async fn get(
        &self,
        _property: &str,
        _server: &zbus::ObjectServer,
        _connection: &Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> Option<fdo::Result<OwnedValue>> {
        None
    }

    async fn get_all(
        &self,
        _server: &zbus::ObjectServer,
        _connection: &Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        Ok(HashMap::new())
    }

    fn set<'call>(
        &'call self,
        _property: &'call str,
        _value: &'call zbus::zvariant::Value<'_>,
        _server: &'call zbus::ObjectServer,
        _connection: &'call Connection,
        _header: Option<&'call Header<'_>>,
        _emitter: &'call SignalEmitter<'_>,
    ) -> DispatchResult2<'call> {
        DispatchResult2::NotFound
    }

    async fn set_mut(
        &mut self,
        _property: &str,
        _value: &zbus::zvariant::Value<'_>,
        _server: &zbus::ObjectServer,
        _connection: &Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> Option<fdo::Result<()>> {
        None
    }

    fn call<'call>(
        &'call self,
        _server: &'call zbus::ObjectServer,
        connection: &'call Connection,
        message: &'call Message,
        name: MemberName<'call>,
    ) -> DispatchResult2<'call> {
        self.dispatch(connection, message, name.as_str())
    }

    fn call_mut<'call>(
        &'call mut self,
        _server: &'call zbus::ObjectServer,
        _connection: &'call Connection,
        _message: &'call Message,
        _name: MemberName<'call>,
    ) -> DispatchResult2<'call> {
        DispatchResult2::NotFound
    }

    fn introspect_to_writer(&self, writer: &mut dyn std::fmt::Write, level: usize) {
        for line in INTROSPECTION.lines() {
            let _ = writeln!(writer, "{:level$}{line}", "");
        }
    }
}

/// One `Notify` call's arguments, but its id and caller.
struct Request {
    app_name: String,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: Hints,
    expire_timeout: i32,
}

/// What `request` says, for notification `id` from `caller`. A sender only
/// its sandbox proved (`foreign`) gets none of the Tilo extensions.
fn notification(id: u32, request: Request, caller: Option<Caller>, foreign: bool) -> Notification {
    let Request {
        app_name,
        app_icon,
        summary,
        body,
        actions,
        hints,
        expire_timeout,
    } = request;
    let extended = caller.is_some() && !foreign;
    Notification {
        id,
        app_name,
        app_icon: Some(app_icon.trim())
            .filter(|icon| !icon.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| hints::parse_icon_hint(&hints, "image-path")),
        desktop_entry: hints::parse_icon_hint(&hints, "desktop-entry"),
        image_data: hints::parse_image_data(&hints),
        summary,
        body,
        actions: hints::pair_actions(&actions),
        expire_timeout,
        urgency: hints::parse_urgency(&hints),
        caller,
        badge_count: extended.then(|| hints::parse_badge_count(&hints)).flatten(),
        contact_endpoint: extended
            .then(|| hints::parse_contact_endpoint(&hints))
            .flatten(),
        sender_name: extended.then(|| hints::parse_sender_name(&hints)).flatten(),
        category: hints::parse_category(&hints),
        suppress_sound: hints::parse_suppress_sound(&hints),
        incoming_call: hints::parse_incoming_call(&hints),
        machine_wide: false,
    }
}

/// Keeps the daemon's signals and its watch on the bus running while held.
/// The connection, and the bus name on it, live as long as it or a clone
/// of [`Self::connection`] does.
pub struct Server {
    connection: Connection,
    notifications: Notifications,
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("connection", &self.connection.unique_name())
            .finish_non_exhaustive()
    }
}

impl Server {
    pub fn connection(&self) -> &Connection {
        &self.connection
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.notifications.detach();
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Serve the interface for `notifications` at [`OBJECT_PATH`] on
/// `connection`, attributing senders through `callers`. Must run inside a
/// tokio runtime. A bus connection also ends ongoing notifications when
/// their sender leaves the bus; a peer-to-peer link has no bus to watch.
///
/// # Errors
/// The interface could not be registered, or the bus would not report
/// senders leaving.
pub async fn serve_on(
    connection: Connection,
    notifications: Notifications,
    callers: Arc<dyn Callers>,
) -> zbus::Result<Server> {
    let mut tasks = Vec::new();
    if connection.unique_name().is_some() {
        // Subscribe before admitting ongoing notifications: without it they
        // would outlive their sender.
        let bus = fdo::DBusProxy::new(&connection).await?;
        let mut changes = bus.receive_name_owner_changed().await?;
        let watched = notifications.clone();
        tasks.push(tokio::spawn(async move {
            while let Some(change) = changes.next().await {
                if let Ok(args) = change.args() {
                    let name = args.name().as_str();
                    if name.starts_with(':') && args.new_owner().is_none() {
                        watched.owner_left(name);
                    }
                }
            }
        }));
    }
    let registered = connection
        .object_server()
        .at(
            OBJECT_PATH,
            Interface {
                notifications: notifications.clone(),
                callers,
            },
        )
        .await;
    if let Err(error) = registered {
        for task in &tasks {
            task.abort();
        }
        return Err(error);
    }
    let (signals, receiver) = mpsc::unbounded_channel();
    notifications.attach(signals);
    tasks.push(tokio::spawn(emit(connection.clone(), receiver)));
    Ok(Server {
        connection,
        notifications,
        tasks,
    })
}

/// [`serve_on`] the session bus, owning [`BUS_NAME`].
///
/// # Errors
/// The session bus is unreachable, the interface could not be registered,
/// or another daemon owns the name.
pub async fn serve(
    notifications: Notifications,
    callers: Arc<dyn Callers>,
) -> zbus::Result<Server> {
    let connection = Connection::session().await?;
    let server = serve_on(connection, notifications, callers).await?;
    server.connection.request_name(BUS_NAME).await?;
    Ok(server)
}

/// Send each signal the handle queues, in order, until the server goes.
async fn emit(connection: Connection, mut signals: mpsc::UnboundedReceiver<Signal>) {
    let emitter = match SignalEmitter::new(&connection, OBJECT_PATH) {
        Ok(emitter) => emitter,
        Err(error) => {
            tracing::warn!(target: "notifications", %error, "signals unavailable");
            return;
        }
    };
    while let Some(signal) = signals.recv().await {
        let sent = match signal {
            Signal::Closed(id, reason) => {
                emitter
                    .emit(BUS_NAME, "NotificationClosed", &(id, reason.code()))
                    .await
            }
            Signal::Action { id, key, token } => {
                let token = match token {
                    Some(token) => {
                        emitter
                            .emit(BUS_NAME, "ActivationToken", &(id, token.as_str()))
                            .await
                    }
                    None => Ok(()),
                };
                token.and(
                    emitter
                        .emit(BUS_NAME, "ActionInvoked", &(id, key.as_str()))
                        .await,
                )
            }
            Signal::Replied(id, text) => {
                emitter
                    .emit(BUS_NAME, "NotificationReplied", &(id, text.as_str()))
                    .await
            }
        };
        if let Err(error) = sent {
            tracing::debug!(target: "notifications", %error, "signal not sent");
        }
    }
}
