use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use serde_json::json;
use tilo_service::{Service, Services};
use tilo_test_env::PrivateBus;
use tokio::net::UnixStream;
use zbus::connection::Builder;
use zbus::zvariant::{OwnedValue, Str, Type as _};

use super::*;

const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";

fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn services(notifications: &Notifications) -> Services {
    let mut services = Services::default();
    services.add(Box::new(notifications.clone()));
    services
}

/// A server and a client over a peer-to-peer link, the daemon served on
/// the server's end.
async fn pair(
    notifications: &Notifications,
    callers: Arc<dyn Callers>,
) -> (Server, zbus::Connection) {
    let (server, client) = UnixStream::pair().expect("socket pair");
    let server = Builder::unix_stream(server)
        .server(TEST_GUID)
        .expect("guid")
        .p2p();
    let client = Builder::unix_stream(client).p2p();
    let (server, client) =
        futures_util::try_join!(server.build(), client.build()).expect("p2p pair");
    let server = serve_on(server, notifications.clone(), callers)
        .await
        .expect("served");
    (server, client)
}

async fn proxy(client: &zbus::Connection) -> zbus::Proxy<'_> {
    zbus::proxy::Builder::<zbus::Proxy<'_>>::new(client)
        .destination(BUS_NAME)
        .expect("destination")
        .path(OBJECT_PATH)
        .expect("path")
        .interface(BUS_NAME)
        .expect("interface")
        .build()
        .await
        .expect("proxy")
}

type NotifyCall<'a> = (
    &'a str,
    u32,
    &'a str,
    &'a str,
    &'a str,
    Vec<String>,
    HashMap<String, OwnedValue>,
    i32,
);

async fn notify(proxy: &zbus::Proxy<'_>, call: NotifyCall<'_>) -> u32 {
    proxy
        .call_method("Notify", &call)
        .await
        .expect("Notify")
        .body()
        .deserialize()
        .expect("an id")
}

fn arrived(notifications: &Notifications) -> Vec<Notification> {
    notifications
        .drain()
        .into_iter()
        .filter_map(|arrival| match arrival {
            Arrival::Notify(notification) => Some(notification),
            Arrival::Close(_) => None,
        })
        .collect()
}

async fn next_signal<T>(signals: &mut zbus::proxy::SignalStream<'_>) -> T
where
    T: for<'d> zbus::zvariant::DynamicDeserialize<'d> + zbus::zvariant::Type,
{
    let message = tokio::time::timeout(Duration::from_secs(5), signals.next())
        .await
        .expect("the signal arrives")
        .expect("the stream stays open");
    message
        .body()
        .deserialize()
        .expect("the signal's arguments")
}

/// Attributes every sender to the app `app_id` launched.
struct LaunchedAs(&'static str);

impl Callers for LaunchedAs {
    fn caller(&self, _sender: Sender<'_>) -> Option<Caller> {
        Some(Caller {
            app_id: self.0.into(),
            workspace: Some("studio".into()),
        })
    }
}

/// The app `org.example.app` launched in workspace 7, as a peer-to-peer
/// link names no sender and no process.
struct Launched;

impl Callers for Launched {
    fn caller(&self, sender: Sender<'_>) -> Option<Caller> {
        assert_eq!(sender, Sender::default());
        Some(Caller {
            app_id: "org.example.app".into(),
            workspace: Some("7".into()),
        })
    }
}

fn resource_action_ids(resource: &tilo_actions::ResourceSpec) -> Vec<String> {
    resource
        .actions
        .iter()
        .map(|action| action.id.clone())
        .collect()
}

/// An `image-data` hint: `channels` samples per pixel, `bytes` as the wire
/// carries them.
fn image_hint(channels: i32, bytes: Vec<u8>) -> HashMap<String, OwnedValue> {
    let mut hints = HashMap::new();
    hints.insert(
        "image-data".to_owned(),
        OwnedValue::try_from(zbus::zvariant::Value::new((
            2_i32, 1_i32, 8_i32, true, 8_i32, channels, bytes,
        )))
        .expect("image-data"),
    );
    hints
}

/// One notification's worth of wire arguments, and what it arrived as.
async fn sent(
    proxy: &zbus::Proxy<'_>,
    notifications: &Notifications,
    hints: HashMap<String, OwnedValue>,
    actions: Vec<String>,
) -> Notification {
    let id = notify(proxy, ("App", 0, "", "Sum", "", actions, hints, -1)).await;
    let [notification] = arrived(notifications).try_into().expect("exactly one");
    assert_eq!(notification.id, id);
    notification
}

#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
async fn an_app_notifies_the_session_and_the_session_answers() {
    let mut notifications = Notifications::new();
    notifications.mint_activation_tokens(|| Some("token-1".into()));
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&woken);
    notifications.on_change(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let tapped = Arc::new(Mutex::new(Vec::new()));
    let tap = Arc::clone(&tapped);
    notifications.on_taken(move |id, taken| {
        if let Taken::Action {
            key,
            activation_token,
        } = taken
        {
            tap.lock().expect("tap").push((
                id,
                key.to_owned(),
                activation_token.map(str::to_owned),
            ));
        }
    });
    let (_server, client) = pair(&notifications, Arc::new(Launched)).await;
    let proxy = proxy(&client).await;

    // What the daemon advertises, and what it decodes, agree.
    let capabilities: Vec<String> = proxy
        .call_method("GetCapabilities", &())
        .await
        .expect("GetCapabilities")
        .body()
        .deserialize()
        .expect("capabilities");
    for capability in ["body", "actions", "persistence", "inline-reply"] {
        assert!(capabilities.iter().any(|c| c == capability), "{capability}");
    }
    let (name, vendor, _version, spec): (String, String, String, String) = proxy
        .call_method("GetServerInformation", &())
        .await
        .expect("GetServerInformation")
        .body()
        .deserialize()
        .expect("server information");
    assert_eq!(name, "Tilo Notifications");
    assert_eq!(vendor, "Tilo");
    assert_eq!(spec, "1.2");
    let xml: String = client
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )
        .await
        .expect("Introspect")
        .body()
        .deserialize()
        .expect("xml");
    for name in [
        "Notify",
        "CloseNotification",
        "GetCapabilities",
        "GetServerInformation",
        "NotificationClosed",
        "ActionInvoked",
        "ActivationToken",
        "NotificationReplied",
    ] {
        assert!(xml.contains(&format!("name=\"{name}\"")), "{name}");
    }
    let notify_method = xml
        .split("<method name=\"Notify\">")
        .nth(1)
        .and_then(|rest| rest.split("</method>").next())
        .expect("Notify is introspected");
    let advertised: String = notify_method
        .lines()
        .filter(|line| line.contains("direction=\"in\""))
        .filter_map(|line| line.split("type=\"").nth(1)?.split('"').next())
        .collect();
    assert_eq!(
        format!("({advertised})"),
        <dbus::NotifyArgs>::SIGNATURE.to_string(),
        "the daemon decodes exactly what it advertises"
    );

    let mut closed = proxy
        .receive_signal("NotificationClosed")
        .await
        .expect("subscribed");
    let mut actions = proxy
        .receive_signal("ActionInvoked")
        .await
        .expect("subscribed");
    let mut tokens = proxy
        .receive_signal("ActivationToken")
        .await
        .expect("subscribed");
    let mut replies = proxy
        .receive_signal("NotificationReplied")
        .await
        .expect("subscribed");

    // An attributed app's notification arrives whole, and is published.
    let mut hints = HashMap::new();
    hints.insert("urgency".to_owned(), OwnedValue::from(2u8));
    hints.insert("x-tilo-badge-count".to_owned(), OwnedValue::from(3u32));
    hints.insert(
        "category".to_owned(),
        OwnedValue::from(Str::from("im.received")),
    );
    let build = notify(
        &proxy,
        (
            "Example",
            0,
            "",
            "Build finished",
            "All 524 tests passed",
            vec!["default".into(), "Open".into()],
            hints,
            -1,
        ),
    )
    .await;
    assert_eq!(build, 1, "the first id is 1");
    assert_eq!(woken.load(Ordering::SeqCst), 1, "the session is woken");
    let [first] = arrived(&notifications).try_into().expect("exactly one");
    assert_eq!(first.id, 1);
    assert_eq!(first.summary, "Build finished");
    assert_eq!(first.urgency, 2);
    assert_eq!(first.actions, [("default".to_owned(), "Open".to_owned())]);
    assert_eq!(first.badge_count, Some(3));
    assert_eq!(first.category.as_deref(), Some("im.received"));
    let caller = first.caller.clone().expect("attributed");
    assert_eq!(caller.app_id, "org.example.app");
    assert_eq!(caller.workspace.as_deref(), Some("7"));
    let published = notifications.resources();
    let root = published.first().expect("the notifications resource");
    assert_eq!(root.id.as_str(), NOTIFICATIONS_RESOURCE);
    assert_eq!(root.kind, NOTIFICATIONS_KIND);
    let [published] = root.children.as_slice() else {
        panic!("one resource per live notification");
    };
    assert_eq!(published.id.as_str(), notification_resource(build));
    assert_eq!(published.kind, NOTIFICATION_KIND);
    assert_eq!(published.label, "Build finished");
    assert_eq!(
        published
            .presentation
            .as_ref()
            .and_then(|p| p.preview.as_deref()),
        Some("All 524 tests passed")
    );
    assert_eq!(resource_action_ids(published), ["close", "invoke"]);
    assert!(notifications.drain().is_empty(), "drained once");

    // An image path arrives as pixels, and is never kept as state.
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("avatar.png");
    image::RgbaImage::from_pixel(4, 2, image::Rgba([0, 0, 255, 255]))
        .save(&path)
        .expect("written");
    let mut hints = HashMap::new();
    hints.insert(
        "image-path".to_owned(),
        OwnedValue::from(Str::from(path.to_str().expect("utf-8"))),
    );
    let with_art = notify(&proxy, ("App", 0, "", "Sum", "", Vec::new(), hints, -1)).await;
    assert!(with_art > build, "ids are monotonic and never zero");
    let [pictured] = arrived(&notifications).try_into().expect("exactly one");
    let pixels = pictured.image_data.clone().expect("decoded");
    assert_eq!((pixels.width, pixels.height), (4, 2));
    assert!(
        notifications
            .notification(with_art)
            .expect("live")
            .image_data
            .is_none(),
        "the published notification keeps no pixels"
    );

    // A replaces_id is returned verbatim, and its timeout is preserved.
    let replaced = notify(
        &proxy,
        ("App", 99, "", "Sum", "Body", Vec::new(), HashMap::new(), 0),
    )
    .await;
    assert_eq!(replaced, 99);
    let [replacement] = arrived(&notifications).try_into().expect("exactly one");
    assert_eq!(replacement.id, 99);
    assert_eq!(replacement.expire_timeout, 0, "never-expire preserved");

    // What a replying and a plain notification offer their banner.
    let chat = notify(
        &proxy,
        (
            "Chat",
            0,
            "",
            "Ada",
            "Lunch?",
            vec![
                "default".into(),
                "Open".into(),
                INLINE_REPLY.into(),
                "Reply".into(),
            ],
            HashMap::new(),
            -1,
        ),
    )
    .await;
    let plain = notify(
        &proxy,
        ("Mail", 0, "", "", "", Vec::new(), HashMap::new(), -1),
    )
    .await;
    let _ = notifications.drain();
    let published = notifications.resources();
    let children = &published.first().expect("root").children;
    let replying = children
        .iter()
        .find(|child| child.id.as_str() == notification_resource(chat))
        .expect("the replying one");
    assert_eq!(replying.label, "Ada");
    assert_eq!(resource_action_ids(replying), ["close", "invoke", "reply"]);
    let plain_row = children
        .iter()
        .find(|child| child.id.as_str() == notification_resource(plain))
        .expect("the plain one");
    assert_eq!(plain_row.label, "Mail", "no summary: listed by its app");
    assert_eq!(
        resource_action_ids(plain_row),
        ["close"],
        "nothing to invoke or answer"
    );

    // Each notification reads as its own data; the root reads as the list.
    let back: Notification = serde_json::from_value(
        notifications
            .read(&notification_resource(chat))
            .expect("a live notification reads"),
    )
    .expect("its data");
    assert_eq!(
        back,
        Notification {
            app_name: "Chat".into(),
            summary: "Ada".into(),
            body: "Lunch?".into(),
            actions: vec![
                ("default".into(), "Open".into()),
                (INLINE_REPLY.into(), "Reply".into()),
            ],
            caller: Some(caller),
            ..Notification::new(chat)
        }
    );
    assert_eq!(
        notifications
            .read(NOTIFICATIONS_RESOURCE)
            .and_then(|all| all.as_array().map(Vec::len)),
        Some(5)
    );
    assert_eq!(notifications.read(&notification_resource(9)), None);
    assert!(notifications.poll(), "the arrivals are news");
    assert!(!notifications.poll(), "nothing changed since");

    // Invoking a key the notification does not offer is refused.
    let refused = services(&notifications)
        .invoke(
            &notification_resource(chat),
            "invoke",
            &arguments(json!({ "action": "delete-everything" })),
        )
        .expect("published");
    assert!(refused.is_err());
    assert_eq!(notifications.live().len(), 5, "still shown");

    // Invoking through the resource mints a token, tells the app, closes.
    let done = services(&notifications)
        .invoke(
            &notification_resource(build),
            "invoke",
            &arguments(json!({ "action": "default" })),
        )
        .expect("published");
    assert!(done.is_ok(), "{done:?}");
    assert_eq!(
        next_signal::<(u32, String)>(&mut tokens).await,
        (build, "token-1".to_owned())
    );
    assert_eq!(
        next_signal::<(u32, String)>(&mut actions).await,
        (build, "default".to_owned())
    );
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (build, 2));
    assert_eq!(notifications.drain(), vec![Arrival::Close(build)]);
    assert_eq!(
        *tapped.lock().expect("tap"),
        [(build, "default".to_owned(), Some("token-1".to_owned()))]
    );
    assert!(notifications.read(&notification_resource(build)).is_none());

    // Replying through the resource answers the app and closes.
    let done = services(&notifications)
        .invoke(
            &notification_resource(chat),
            "reply",
            &arguments(json!({ "text": "Yes" })),
        )
        .expect("published");
    assert!(done.is_ok(), "{done:?}");
    assert_eq!(
        next_signal::<(u32, String)>(&mut replies).await,
        (chat, "Yes".to_owned())
    );
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (chat, 2));
    assert!(notifications.drain().contains(&Arrival::Close(chat)));

    // Only a sender offering inline replies takes one.
    assert!(
        services(&notifications)
            .invoke(
                &notification_resource(with_art),
                "reply",
                &arguments(json!({ "text": "x" }))
            )
            .is_none(),
    );
    let done = services(&notifications)
        .invoke(
            &notification_resource(with_art),
            "close",
            &serde_json::Map::new(),
        )
        .expect("published");
    assert!(done.is_ok(), "{done:?}");
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (with_art, 2));
    assert_eq!(notifications.drain(), vec![Arrival::Close(pictured.id)]);

    // The app closes its notification: the session takes the banner away.
    proxy
        .call_method("CloseNotification", &(replaced,))
        .await
        .expect("CloseNotification");
    assert_eq!(notifications.drain(), vec![Arrival::Close(replaced)]);
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (replaced, 3));
    assert!(
        notifications
            .resources()
            .first()
            .expect("root")
            .children
            .iter()
            .all(|child| child.id.as_str() != notification_resource(replaced))
    );

    // The session answers a banner itself; the close is not handed back.
    let banner = notify(
        &proxy,
        (
            "App",
            0,
            "",
            "Sum",
            "",
            vec!["default".into(), "Open".into()],
            HashMap::new(),
            -1,
        ),
    )
    .await;
    let _ = notifications.drain();
    notifications.action_invoked(banner, "default", None);
    assert!(notifications.closed(banner, CloseReason::Dismissed));
    assert!(
        !notifications.closed(banner, CloseReason::Dismissed),
        "closes once"
    );
    assert_eq!(
        next_signal::<(u32, String)>(&mut actions).await,
        (banner, "default".to_owned())
    );
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (banner, 2));
    assert!(notifications.drain().is_empty());

    // A notification that expired is gone, with the specification's reason.
    notifications.closed(plain, CloseReason::Expired);
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (plain, 1));
    assert!(notifications.live().is_empty());
    assert!(notifications.read(&notification_resource(plain)).is_none());
    assert!(notifications.drain().is_empty());
}

#[tokio::test]
async fn an_unattributed_sender_gets_no_tilo_extensions() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
    let mut hints = HashMap::new();
    hints.insert("x-tilo-badge-count".to_owned(), OwnedValue::from(3u32));
    hints.insert(
        "x-tilo-sender-name".to_owned(),
        OwnedValue::from(Str::from("Ada")),
    );
    let id = notify(
        &proxy(&client).await,
        ("App", 0, "", "Sum", "", Vec::new(), hints, -1),
    )
    .await;
    assert!(id > 0);
    let [notification] = arrived(&notifications).try_into().expect("exactly one");
    assert_eq!(notification.caller, None);
    assert_eq!(notification.badge_count, None);
    assert_eq!(notification.sender_name, None);
}

#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
async fn hints_from_the_wire_are_read_bounded_and_defaulted() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(Launched)).await;
    let proxy = proxy(&client).await;

    let pixels = sent(
        &proxy,
        &notifications,
        image_hint(4, vec![255, 0, 0, 255, 0, 255, 0, 255]),
        Vec::new(),
    )
    .await
    .image_data
    .expect("RGBA is decoded");
    assert_eq!((pixels.width, pixels.height), (2, 1));
    assert_eq!(pixels.rgba, [255, 0, 0, 255, 0, 255, 0, 255]);

    let mut hints = HashMap::new();
    hints.insert(
        "image-data".to_owned(),
        OwnedValue::try_from(zbus::zvariant::Value::new((
            1_i32,
            1_i32,
            4_i32,
            false,
            8_i32,
            3_i32,
            vec![1_u8, 2, 3, 0],
        )))
        .expect("image-data"),
    );
    let pixels = sent(&proxy, &notifications, hints, Vec::new())
        .await
        .image_data
        .expect("three channels gain an opaque alpha");
    assert_eq!(pixels.rgba, [1, 2, 3, 255]);

    let mut hints = HashMap::new();
    hints.insert(
        "image-data".to_owned(),
        OwnedValue::try_from(zbus::zvariant::Value::new((
            1_i32,
            1_i32,
            4_i32,
            false,
            16_i32,
            3_i32,
            vec![0_u8; 32],
        )))
        .expect("image-data"),
    );
    assert!(
        sent(&proxy, &notifications, hints, Vec::new())
            .await
            .image_data
            .is_none(),
        "more than 8 bits per sample is not something the card can paint"
    );

    let notification = sent(
        &proxy,
        &notifications,
        HashMap::new(),
        vec![
            "default".into(),
            "Open".into(),
            "snooze".into(),
            "Snooze".into(),
            "dangling".into(),
        ],
    )
    .await;
    assert_eq!(
        notification.actions,
        [
            ("default".to_owned(), "Open".to_owned()),
            ("snooze".to_owned(), "Snooze".to_owned()),
        ],
        "a dangling key is dropped"
    );
    assert_eq!(notification.urgency, 1, "normal unless stated");

    let mut hints = HashMap::new();
    hints.insert("urgency".to_string(), OwnedValue::from(2u8));
    hints.insert("x-tilo-badge-count".into(), OwnedValue::from(u32::MAX));
    hints.insert(
        "x-tilo-contact-handle".into(),
        OwnedValue::from(Str::from("ada@example.test")),
    );
    let notification = sent(&proxy, &notifications, hints, Vec::new()).await;
    assert_eq!(notification.urgency, 2);
    assert_eq!(notification.badge_count, Some(9_999), "bounded");
    assert_eq!(
        notification.contact_endpoint.as_deref(),
        Some("ada@example.test")
    );

    let mut hints = HashMap::new();
    hints.insert(
        "x-tilo-contact-handle".into(),
        OwnedValue::from(Str::from("bad\nhandle")),
    );
    assert_eq!(
        sent(&proxy, &notifications, hints, Vec::new())
            .await
            .contact_endpoint,
        None,
        "an endpoint is one line"
    );

    let mut hints = HashMap::new();
    hints.insert(
        "x-tilo-sender-name".into(),
        OwnedValue::from(Str::from("  Ada  ")),
    );
    assert_eq!(
        sent(&proxy, &notifications, hints, Vec::new())
            .await
            .sender_name
            .as_deref(),
        Some("Ada")
    );

    let mut hints = HashMap::new();
    hints.insert(
        "x-tilo-sender-name".into(),
        OwnedValue::from(Str::from("Ada\nLovelace")),
    );
    assert_eq!(
        sent(&proxy, &notifications, hints, Vec::new())
            .await
            .sender_name,
        None
    );

    let mut hints = HashMap::new();
    hints.insert(
        "category".into(),
        OwnedValue::from(Str::from("im.received")),
    );
    hints.insert("suppress-sound".into(), OwnedValue::from(true));
    let notification = sent(&proxy, &notifications, hints, Vec::new()).await;
    assert_eq!(notification.category.as_deref(), Some("im.received"));
    assert!(notification.suppress_sound);
    assert!(!notification.incoming_call);

    let mut hints = HashMap::new();
    hints.insert(
        "category".into(),
        OwnedValue::from(Str::from("call.incoming")),
    );
    assert!(
        sent(&proxy, &notifications, hints, Vec::new())
            .await
            .incoming_call
    );

    let mut hints = HashMap::new();
    hints.insert("category".into(), OwnedValue::from(Str::from("a\nb")));
    assert_eq!(
        sent(&proxy, &notifications, hints, Vec::new())
            .await
            .category,
        None
    );
}

/// A private session bus for the test's life.
struct SessionBus(PrivateBus);

impl SessionBus {
    fn start() -> Self {
        Self(PrivateBus::start())
    }

    async fn connect(&self) -> zbus::Connection {
        Builder::address(self.0.address())
            .expect("address")
            .build()
            .await
            .expect("connected")
    }
}

/// Attributes every sender on a bus, which names it and its process.
struct OnTheBus;

impl Callers for OnTheBus {
    fn caller(&self, sender: Sender<'_>) -> Option<Caller> {
        assert!(sender.unique_name.is_some_and(|name| name.starts_with(':')));
        assert_eq!(sender.pid, Some(std::process::id()));
        Some(Caller {
            app_id: "org.example.android".into(),
            workspace: None,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ongoing_notification_is_its_senders_and_ends_when_it_leaves() {
    let bus = SessionBus::start();
    let notifications = Notifications::new();
    let changed = Arc::new(tokio::sync::Notify::new());
    let wake = Arc::clone(&changed);
    notifications.on_change(move || wake.notify_one());
    let server = serve_on(
        bus.connect().await,
        notifications.clone(),
        Arc::new(OnTheBus),
    )
    .await
    .expect("served");
    let name = server
        .connection()
        .unique_name()
        .expect("on a bus")
        .to_string();
    let call = |client: zbus::Connection, method: &'static str, body: NotifyCall<'static>| {
        let name = name.clone();
        async move {
            client
                .call_method(
                    Some(name.as_str()),
                    OBJECT_PATH,
                    Some(BUS_NAME),
                    method,
                    &body,
                )
                .await
        }
    };
    let owner = bus.connect().await;
    let mut hints = HashMap::new();
    hints.insert(
        "category".to_owned(),
        OwnedValue::from(Str::from(hints::ONGOING)),
    );
    let id: u32 = call(
        owner.clone(),
        "Notify",
        ("Player", 0, "", "Playing", "", Vec::new(), hints, 5_000),
    )
    .await
    .expect("Notify")
    .body()
    .deserialize()
    .expect("an id");
    let [notification] = arrived(&notifications).try_into().expect("exactly one");
    assert_eq!(
        notification.expire_timeout, 0,
        "lasts while its sender does"
    );

    let stranger = bus.connect().await;
    let refused = stranger
        .call_method(
            Some(name.as_str()),
            OBJECT_PATH,
            Some(BUS_NAME),
            "CloseNotification",
            &(id,),
        )
        .await;
    assert!(refused.is_err(), "only its sender closes it");
    assert_eq!(notifications.live().len(), 1);

    drop(owner);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if notifications.live().is_empty() {
                break;
            }
            changed.notified().await;
        }
    })
    .await
    .expect("the notification ends with its sender");
    assert_eq!(notifications.drain(), vec![Arrival::Close(id)]);
}

type Contacts = Vec<(String, String, Vec<String>)>;

fn contacts(id: &str, endpoint: &str) -> Contacts {
    vec![(id.into(), id.into(), vec![endpoint.into()])]
}

/// The extensions only an app the session launched may use: the person's
/// priority contacts and the deadlines an app leaves behind.
#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
async fn deadlines_and_priority_contacts_belong_only_to_the_session() {
    let dir = tempfile::tempdir().expect("private fixture");
    let path = dir.path().join("priority.json");
    let notifications = Notifications::with(
        PriorityContacts::open(Some(path.clone())),
        Schedules::in_memory(),
    );
    let far = 4_102_444_800_000_i64; // 2100-01-01

    let (_app_server, app) = pair(&notifications, Arc::new(Launched)).await;
    let app = proxy(&app).await;
    let refused = app
        .call_method(
            "XTiloReplacePriorityContacts",
            &(contacts("ada", "ada@example.com"),),
        )
        .await;
    assert!(refused.is_err(), "another app may not name them");
    assert_eq!(notifications.priority_contacts().count(), 0);

    let (_stranger_server, stranger) = pair(&notifications, Arc::new(NoCallers)).await;
    let refused = proxy(&stranger)
        .await
        .call_method(
            "XTiloScheduleNotification",
            &(
                "t",
                "T",
                "",
                "",
                far,
                Vec::<(u8, u8, u8, i64, String)>::new(),
            ),
        )
        .await;
    assert!(
        refused.is_err(),
        "an unknown sender keeps no deadlines here"
    );
    assert!(notifications.schedules().deadlines().is_empty());

    let (_contacts_server, contacts_app) =
        pair(&notifications, Arc::new(LaunchedAs(priority::CONTACTS_APP))).await;
    let contacts_app = proxy(&contacts_app).await;
    let junk: u32 = contacts_app
        .call_method(
            "XTiloReplacePriorityContacts",
            &(vec![(
                "../escape".to_owned(),
                "x".repeat(1000),
                vec!["a@example.com".to_owned()],
            )],),
        )
        .await
        .expect("answered")
        .body()
        .deserialize()
        .expect("a count");
    assert_eq!(junk, 0, "a contact that is not one is never kept");
    assert_eq!(notifications.priority_contacts().count(), 0);
    let kept: u32 = contacts_app
        .call_method(
            "XTiloReplacePriorityContacts",
            &(vec![(
                "ca761232-ed42-11ce-bacd-00aa0057b223".to_owned(),
                "Ada".to_owned(),
                vec![" Ada@Example.COM ".to_owned(), "+49 123 456789".to_owned()],
            )],),
        )
        .await
        .expect("Contacts names them")
        .body()
        .deserialize()
        .expect("a count");
    assert_eq!(kept, 1);
    let raw = std::fs::read_to_string(&path).expect("kept in the person's state");
    assert!(
        !raw.contains("Ada@Example.COM"),
        "an endpoint is never written as it came"
    );
    assert!(!raw.contains("123 456789"));
    assert!(notifications.priority_contacts().matches("ada@example.com"));
    assert!(
        notifications
            .priority_contacts()
            .matches("+49 (123) 456789")
    );
    let reopened = PriorityContacts::open(Some(path));
    assert!(
        reopened.matches("ada@example.com"),
        "the record is the person's own"
    );

    let message = |handle: &str| {
        let mut hints = HashMap::new();
        hints.insert(
            "x-tilo-contact-handle".to_owned(),
            OwnedValue::from(Str::from(handle)),
        );
        ("Chat", 0, "", "Hi", "", Vec::new(), hints, -1)
    };
    notify(&app, message("ada@example.com")).await;
    notify(&app, message("eve@example.com")).await;
    let marked: Vec<bool> = arrived(&notifications)
        .iter()
        .map(|notification| notification.priority_contact)
        .collect();
    assert_eq!(marked, [true, false]);

    app.call_method(
        "XTiloScheduleNotification",
        &(
            "standup",
            "Standup",
            "In the small room",
            "example:standup",
            far,
            Vec::<(u8, u8, u8, i64, String)>::new(),
        ),
    )
    .await
    .expect("scheduled");
    let pending: Vec<(String, i64)> = app
        .call_method("XTiloScheduledNotifications", &())
        .await
        .expect("read")
        .body()
        .deserialize()
        .expect("pending");
    assert_eq!(pending, [("standup".to_owned(), far)]);
    let [deadline] = notifications
        .schedules()
        .deadlines()
        .try_into()
        .expect("one");
    assert_eq!(deadline.app_id, "org.example.app");
    assert_eq!(deadline.workspace, "7");
    app.call_method("XTiloCancelScheduledNotification", &("standup",))
        .await
        .expect("cancelled");
    assert!(notifications.schedules().deadlines().is_empty());
}

/// What outlives a notification — the deadlines apps leave behind and the
/// person's priority contacts — is changed through the resources it is
/// published as, the same way a banner or an agent does.
#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn deadlines_and_priority_contacts_are_changed_through_their_resources() {
    let notifications = Notifications::new();
    let mut services = services(&notifications);

    let due: jiff::Timestamp = "2026-04-14T12:00:00+00:00".parse().expect("instant");
    let done = services
        .invoke(
            SCHEDULES_RESOURCE,
            "schedule",
            &arguments(json!({
                "app_id": "so.tilo.clocks",
                "tag": "wake",
                "title": "Wake up",
                "activation": "tilo-clocks:alarm/1",
                "at_ms": due.as_millisecond(),
                "alarm_hour": 12,
            })),
        )
        .expect("published");
    assert!(done.is_ok(), "{done:?}");
    assert!(services.poll(), "the deadline is news");
    let [deadline] = notifications
        .schedules()
        .deadlines()
        .try_into()
        .expect("one");
    let resource = schedule_resource(&deadline.token);
    assert!(
        services
            .read(&resource)
            .is_some_and(|reading| reading["tag"] == "wake"),
        "each deadline reads as itself"
    );

    notifications.deliver_due(&due.to_zoned(jiff::tz::TimeZone::UTC));
    let [rung] = arrived(&notifications).try_into().expect("it rings");
    let occurrence = rung.scheduled.clone().expect("its deadline");
    assert!(occurrence.calendar_alarm);
    assert_eq!(occurrence.uri, "tilo-clocks:alarm/1");
    let snoozed = services
        .invoke(
            &notification_resource(rung.id),
            "invoke",
            &arguments(json!({ "action": scheduled::SNOOZE })),
        )
        .expect("published");
    assert!(snoozed.is_ok(), "{snoozed:?}");
    let reading = services.read(&resource).expect("still scheduled");
    assert!(reading["snoozed_until"].is_i64(), "{reading}");
    assert_eq!(
        resource_action_ids(&notifications.resources()[1].children[0]),
        ["cancel", "dismiss-snooze"]
    );
    let cancelled = services
        .invoke(&resource, "cancel", &serde_json::Map::new())
        .expect("published");
    assert!(cancelled.is_ok(), "{cancelled:?}");
    assert!(services.read(&resource).is_none());

    // The person's priority contacts are marked and unmarked through
    // their resources.
    let marked = services
        .invoke(
            PRIORITY_CONTACTS_RESOURCE,
            "mark",
            &arguments(json!({
                "id": "ada",
                "name": "Ada Lovelace",
                "endpoints": ["ada@example.com"],
            })),
        )
        .expect("published");
    assert!(marked.is_ok(), "{marked:?}");
    assert!(notifications.priority_contacts().matches("ada@example.com"));
    let reading = services.read(PRIORITY_CONTACTS_RESOURCE).expect("read");
    assert_eq!(reading, json!([{ "id": "ada", "name": "Ada Lovelace" }]));
    assert!(
        !reading.to_string().contains("example.com"),
        "an endpoint is never published"
    );
    let resource = priority_contact_resource("ada");
    assert_eq!(
        notifications.resources()[2].children[0].id.as_str(),
        resource
    );
    // Marking the same id again replaces it rather than adding one.
    let replaced = services
        .invoke(
            PRIORITY_CONTACTS_RESOURCE,
            "mark",
            &arguments(json!({
                "id": "ada",
                "name": "Ada Lovelace",
                "endpoints": ["+49 30 1234567"],
            })),
        )
        .expect("published");
    assert!(replaced.is_ok(), "{replaced:?}");
    assert_eq!(
        services.read(PRIORITY_CONTACTS_RESOURCE),
        Some(json!([{ "id": "ada", "name": "Ada Lovelace" }]))
    );
    assert!(!notifications.priority_contacts().matches("ada@example.com"));
    assert!(notifications.priority_contacts().matches("+49301234567"));
    let unmarked = services
        .invoke(&resource, "unmark", &serde_json::Map::new())
        .expect("published");
    assert!(unmarked.is_ok(), "{unmarked:?}");
    assert!(!notifications.priority_contacts().matches("+49301234567"));
    assert!(
        services
            .invoke(&resource, "unmark", &serde_json::Map::new())
            .is_none(),
        "a contact that is not one has nothing to unmark"
    );
    let refused = services
        .invoke(
            PRIORITY_CONTACTS_RESOURCE,
            "mark",
            &arguments(json!({ "id": "x", "name": "X", "endpoints": ["nope"] })),
        )
        .expect("published");
    assert!(refused.is_err(), "no endpoint to match");
}

/// A deadline scheduled through its resource is kept in the person's
/// workspace: the session restarts, publishes it again, and it rings.
#[test]
fn a_deadline_scheduled_through_its_resource_rings_after_a_restart() {
    let workspace: std::rc::Rc<dyn tilo_service::WorkspaceStore> =
        std::rc::Rc::new(scheduled::tests::Workspace::new());
    // The session as it starts: it names the person who sits here, then reaches the
    // workspace on its first pass.
    let start = || {
        let mut notifications = Notifications::new();
        notifications.own_person(scheduled::tests::PERSON, &[]);
        let mut services = services(&notifications);
        services
            .keep_in_workspace(&workspace)
            .expect("the workspace reached");
        (notifications, services)
    };
    let due: jiff::Timestamp = "2026-04-14T12:00:00+00:00".parse().expect("instant");
    let (notifications, mut services) = start();
    let done = services
        .invoke(
            SCHEDULES_RESOURCE,
            "schedule",
            &arguments(json!({
                "app_id": "so.tilo.clocks",
                "tag": "wake",
                "title": "Wake up",
                "activation": "tilo-clocks:alarm/1",
                "at_ms": due.as_millisecond(),
                "alarm_hour": 12,
            })),
        )
        .expect("published");
    assert!(done.is_ok(), "{done:?}");
    services
        .keep_in_workspace(&workspace)
        .expect("the next pass");
    drop((notifications, services));

    let (notifications, services) = start();
    let [deadline] = notifications
        .schedules()
        .deadlines()
        .try_into()
        .expect("the deadline is kept across the restart");
    assert!(
        services
            .read(&schedule_resource(&deadline.token))
            .is_some_and(|reading| reading["tag"] == "wake"),
        "the restarted session publishes it"
    );
    notifications.deliver_due(&due.to_zoned(jiff::tz::TimeZone::UTC));
    let [rung] = arrived(&notifications).try_into().expect("it rings");
    assert_eq!(rung.summary, "Wake up");
    assert_eq!(
        rung.scheduled.expect("its deadline").uri,
        "tilo-clocks:alarm/1"
    );
}

#[test]
fn a_producer_hears_every_tap_and_close_with_its_own_id() {
    let notifications = Notifications::new();
    let taken: Arc<Mutex<Vec<(u32, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(Mutex::new(Vec::new()));
    let receiving = Arc::clone(&taken);
    notifications.on_taken(move |id, what| {
        let (kind, text) = match what {
            Taken::Action { key, .. } => ("action", key.to_owned()),
            Taken::Reply { text } => ("reply", text.to_owned()),
        };
        receiving.lock().unwrap().push((id, kind.to_owned(), text));
    });
    let hearing = Arc::clone(&closed);
    notifications.on_closed(move |id, reason| {
        hearing.lock().unwrap().push((id, reason));
    });

    let mut shown = Notification::new(notifications.alloc_id());
    shown.summary = "Ada wrote".to_owned();
    notifications.push(shown);
    let id = notifications.live()[0].id;
    notifications.action_invoked(id, "default", None);
    notifications.replied(id, "on my way");
    assert_eq!(
        *taken.lock().unwrap(),
        [
            (id, "action".to_owned(), "default".to_owned()),
            (id, "reply".to_owned(), "on my way".to_owned()),
        ]
    );

    assert!(
        notifications.closed(id, CloseReason::Dismissed),
        "it was live"
    );
    assert_eq!(
        *closed.lock().unwrap(),
        [(id, CloseReason::Dismissed)],
        "a close is told once, with its reason"
    );
    assert!(
        !notifications.closed(id, CloseReason::Closed),
        "a notification that is gone is told to nobody again"
    );
    assert_eq!(closed.lock().unwrap().len(), 1);
}
