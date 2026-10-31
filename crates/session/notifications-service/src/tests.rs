use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::StreamExt as _;
use serde_json::json;
use tilo_service::{Service, Services};
use tilo_test_env::PrivateBus;
use tokio::net::UnixStream;
use zbus::connection::Builder;
use zbus::zvariant::{OwnedValue, Str};

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

fn shown(id: u32, actions: &[(&str, &str)]) -> Notification {
    Notification {
        app_name: "Mail".into(),
        summary: "Invoice".into(),
        body: "Due Friday".into(),
        actions: actions
            .iter()
            .map(|(key, label)| ((*key).to_owned(), (*label).to_owned()))
            .collect(),
        ..Notification::new(id)
    }
}

#[test]
fn alloc_id_is_monotonic_and_never_zero() {
    let notifications = Notifications::new();
    let a = notifications.alloc_id();
    let c = notifications.alloc_id();
    assert_ne!(a, 0);
    assert!(c > a, "ids must be monotonic: {a} then {c}");
}

#[test]
fn arrivals_drain_once_in_order_and_wake_the_session() {
    let notifications = Notifications::new();
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&woken);
    notifications.on_change(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    notifications.push(shown(1, &[]));
    assert!(notifications.request_close(1));
    assert_eq!(woken.load(Ordering::SeqCst), 2);
    assert_eq!(
        notifications.drain(),
        vec![Arrival::Notify(shown(1, &[])), Arrival::Close(1)]
    );
    assert!(notifications.drain().is_empty());
    assert!(
        !notifications.request_close(1),
        "a closed notification closes once"
    );
}

#[test]
fn a_notification_is_live_until_it_closes() {
    let notifications = Notifications::new();
    notifications.push(shown(4, &[]));
    assert_eq!(notifications.live().len(), 1);
    assert!(notifications.closed(4, CloseReason::Expired));
    assert!(notifications.live().is_empty());
    assert!(!notifications.closed(4, CloseReason::Dismissed));
    assert!(
        notifications
            .drain()
            .iter()
            .all(|arrival| matches!(arrival, Arrival::Notify(_))),
        "a close the session made is not handed back to it"
    );
}

#[test]
fn close_reasons_use_the_spec_codes() {
    assert_eq!(CloseReason::Expired.code(), 1);
    assert_eq!(CloseReason::Dismissed.code(), 2);
    assert_eq!(CloseReason::Closed.code(), 3);
}

#[test]
fn each_live_notification_is_a_resource_with_its_banners_actions() {
    let notifications = Notifications::new();
    notifications.push(shown(3, &[("default", "Open"), (INLINE_REPLY, "Reply")]));
    notifications.push(Notification {
        summary: String::new(),
        ..shown(5, &[])
    });

    let published = notifications.resources();
    let root = published.first().expect("the notifications resource");
    assert_eq!(root.id.as_str(), NOTIFICATIONS_RESOURCE);
    assert_eq!(root.kind, NOTIFICATIONS_KIND);
    let [replying, plain] = root.children.as_slice() else {
        panic!(
            "one resource per live notification, not {}",
            root.children.len()
        );
    };
    assert_eq!(replying.id.as_str(), notification_resource(3));
    assert_eq!(replying.kind, NOTIFICATION_KIND);
    assert_eq!(replying.label, "Invoice");
    assert_eq!(
        replying
            .presentation
            .as_ref()
            .and_then(|p| p.preview.as_deref()),
        Some("Due Friday")
    );
    let ids = |resource: &tilo_actions::ResourceSpec| {
        resource
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(replying), ["close", "invoke", "reply"]);
    assert_eq!(plain.label, "Mail", "no summary: listed by its app");
    assert_eq!(ids(plain), ["close"], "nothing to invoke or answer");
}

#[test]
fn each_notification_reads_as_its_data_and_poll_sees_changes() {
    let mut notifications = Notifications::new();
    assert!(!notifications.poll());
    notifications.push(Notification {
        image_data: Some(ImageData {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        }),
        ..shown(3, &[("default", "Open")])
    });
    assert!(notifications.poll());
    assert!(!notifications.poll(), "nothing changed since");

    let read = notifications
        .read(&notification_resource(3))
        .expect("a live notification reads");
    let back: Notification = serde_json::from_value(read).expect("its data");
    assert_eq!(back, shown(3, &[("default", "Open")]), "without its pixels");
    let all = notifications
        .read(NOTIFICATIONS_RESOURCE)
        .expect("the root reads");
    assert_eq!(all.as_array().map(Vec::len), Some(1));
    assert_eq!(notifications.read(&notification_resource(9)), None);

    assert!(notifications.closed(3, CloseReason::Expired));
    assert!(notifications.poll());
    assert_eq!(notifications.read(&notification_resource(3)), None);
}

#[test]
fn invoking_a_key_the_notification_does_not_offer_is_refused() {
    let notifications = Notifications::new();
    notifications.push(shown(3, &[("default", "Open")]));
    let refused = services(&notifications)
        .invoke(
            &notification_resource(3),
            "invoke",
            &arguments(json!({ "action": "delete-everything" })),
        )
        .expect("published");
    assert!(refused.is_err());
    assert_eq!(notifications.live().len(), 1, "still shown");
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

struct Launched;

impl Callers for Launched {
    fn caller(&self, sender: Sender<'_>) -> Option<Caller> {
        // A peer-to-peer link names no sender and no process.
        assert_eq!(sender, Sender::default());
        Some(Caller {
            app_id: "org.example.app".into(),
            workspace: Some("7".into()),
        })
    }
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

#[tokio::test]
async fn notify_round_trips_with_attribution_and_is_published() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(Launched)).await;
    let proxy = proxy(&client).await;
    let mut hints = HashMap::new();
    hints.insert("urgency".to_owned(), OwnedValue::from(2u8));
    hints.insert("x-tilo-badge-count".to_owned(), OwnedValue::from(3u32));
    hints.insert(
        "category".to_owned(),
        OwnedValue::from(Str::from("im.received")),
    );

    let id = notify(
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
    assert_eq!(id, 1, "the first id is 1");

    let [notification] = arrived(&notifications).try_into().expect("exactly one");
    assert_eq!(notification.id, 1);
    assert_eq!(notification.summary, "Build finished");
    assert_eq!(notification.urgency, 2);
    assert_eq!(
        notification.actions,
        [("default".to_owned(), "Open".to_owned())]
    );
    assert_eq!(notification.badge_count, Some(3));
    assert_eq!(notification.category.as_deref(), Some("im.received"));
    let caller = notification.caller.expect("attributed");
    assert_eq!(caller.app_id, "org.example.app");
    assert_eq!(caller.workspace.as_deref(), Some("7"));

    let published = notifications.resources();
    let children = &published.first().expect("root").children;
    assert_eq!(children.len(), 1);
    assert_eq!(
        children.first().map(|c| c.label.as_str()),
        Some("Build finished")
    );
}

#[tokio::test]
async fn an_unattributed_sender_gets_no_tilo_extensions() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
    let proxy = proxy(&client).await;
    let mut hints = HashMap::new();
    hints.insert("x-tilo-badge-count".to_owned(), OwnedValue::from(3u32));
    hints.insert(
        "x-tilo-sender-name".to_owned(),
        OwnedValue::from(Str::from("Ada")),
    );
    notify(&proxy, ("App", 0, "", "Sum", "", Vec::new(), hints, -1)).await;
    let [notification] = arrived(&notifications).try_into().expect("exactly one");
    assert_eq!(notification.caller, None);
    assert_eq!(notification.badge_count, None);
    assert_eq!(notification.sender_name, None);
}

#[tokio::test]
async fn notify_returns_a_nonzero_replaces_id_verbatim() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
    let proxy = proxy(&client).await;
    let id = notify(
        &proxy,
        ("App", 99, "", "Sum", "Body", Vec::new(), HashMap::new(), 0),
    )
    .await;
    assert_eq!(id, 99, "a non-zero replaces_id is returned verbatim");
    let [notification] = arrived(&notifications).try_into().expect("exactly one");
    assert_eq!(notification.id, 99);
    assert_eq!(notification.expire_timeout, 0, "never-expire preserved");
}

#[tokio::test]
async fn close_notification_from_the_app_reaches_the_session_and_is_signalled() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
    let proxy = proxy(&client).await;
    let mut closed = proxy
        .receive_signal("NotificationClosed")
        .await
        .expect("subscribed");
    let id = notify(
        &proxy,
        ("App", 0, "", "Sum", "", Vec::new(), HashMap::new(), -1),
    )
    .await;
    let _ = notifications.drain();

    proxy
        .call_method("CloseNotification", &(id,))
        .await
        .expect("CloseNotification");
    assert_eq!(notifications.drain(), vec![Arrival::Close(id)]);
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (id, 3));
    assert!(
        notifications
            .resources()
            .first()
            .expect("root")
            .children
            .is_empty()
    );
}

#[tokio::test]
async fn invoking_through_the_resource_sends_token_action_and_close() {
    let notifications = Notifications::new();
    notifications.mint_activation_tokens(|| Some("token-1".into()));
    let tapped = Arc::new(std::sync::Mutex::new(Vec::new()));
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
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
    let proxy = proxy(&client).await;
    let mut tokens = proxy
        .receive_signal("ActivationToken")
        .await
        .expect("subscribed");
    let mut actions = proxy
        .receive_signal("ActionInvoked")
        .await
        .expect("subscribed");
    let mut closed = proxy
        .receive_signal("NotificationClosed")
        .await
        .expect("subscribed");
    let id = notify(
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

    let done = services(&notifications)
        .invoke(
            &notification_resource(id),
            "invoke",
            &arguments(json!({ "action": "default" })),
        )
        .expect("published");
    assert!(done.is_ok(), "{done:?}");
    assert_eq!(
        next_signal::<(u32, String)>(&mut tokens).await,
        (id, "token-1".to_owned())
    );
    assert_eq!(
        next_signal::<(u32, String)>(&mut actions).await,
        (id, "default".to_owned())
    );
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (id, 2));
    assert_eq!(notifications.drain(), vec![Arrival::Close(id)]);
    assert_eq!(
        *tapped.lock().expect("tap"),
        [(id, "default".to_owned(), Some("token-1".to_owned()))]
    );
}

#[tokio::test]
async fn replying_and_closing_through_the_resource_are_signalled() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
    let proxy = proxy(&client).await;
    let mut replies = proxy
        .receive_signal("NotificationReplied")
        .await
        .expect("subscribed");
    let mut closed = proxy
        .receive_signal("NotificationClosed")
        .await
        .expect("subscribed");
    let chat = notify(
        &proxy,
        (
            "Chat",
            0,
            "",
            "Ada",
            "Lunch?",
            vec![INLINE_REPLY.into(), "Reply".into()],
            HashMap::new(),
            -1,
        ),
    )
    .await;
    let other = notify(
        &proxy,
        ("App", 0, "", "Sum", "", Vec::new(), HashMap::new(), -1),
    )
    .await;
    let _ = notifications.drain();

    let mut services = services(&notifications);
    let done = services
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

    assert!(
        services
            .invoke(
                &notification_resource(other),
                "reply",
                &arguments(json!({ "text": "x" }))
            )
            .is_none(),
        "only a sender offering inline replies takes one"
    );
    let done = services
        .invoke(
            &notification_resource(other),
            "close",
            &serde_json::Map::new(),
        )
        .expect("published");
    assert!(done.is_ok(), "{done:?}");
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (other, 2));
    assert_eq!(
        notifications.drain(),
        vec![Arrival::Close(chat), Arrival::Close(other)]
    );
}

#[tokio::test]
async fn the_session_answers_a_banner_through_the_handle() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
    let proxy = proxy(&client).await;
    let mut actions = proxy
        .receive_signal("ActionInvoked")
        .await
        .expect("subscribed");
    let mut closed = proxy
        .receive_signal("NotificationClosed")
        .await
        .expect("subscribed");
    let id = notify(
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

    notifications.action_invoked(id, "default", None);
    assert!(notifications.closed(id, CloseReason::Dismissed));
    assert_eq!(
        next_signal::<(u32, String)>(&mut actions).await,
        (id, "default".to_owned())
    );
    assert_eq!(next_signal::<(u32, u32)>(&mut closed).await, (id, 2));
    assert!(notifications.drain().is_empty());
}

#[tokio::test]
async fn capabilities_and_server_information_are_reported() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
    let proxy = proxy(&client).await;
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

#[test]
fn notify_decodes_exactly_the_advertised_arguments() {
    use zbus::zvariant::Type as _;
    let notify = dbus::INTROSPECTION
        .split("<method name=\"Notify\">")
        .nth(1)
        .and_then(|rest| rest.split("</method>").next())
        .expect("Notify is introspected");
    let advertised: String = notify
        .lines()
        .filter(|line| line.contains("direction=\"in\""))
        .filter_map(|line| line.split("type=\"").nth(1)?.split('"').next())
        .collect();
    assert_eq!(
        format!("({advertised})"),
        <dbus::NotifyArgs>::SIGNATURE.to_string()
    );
}

#[tokio::test]
async fn the_interface_introspects_its_methods_and_signals() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
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
}

/// Attributes every sender to the app `app_id` launched.
struct LaunchedAs(&'static str);

impl Callers for LaunchedAs {
    fn caller(&self, _sender: Sender<'_>) -> Option<Caller> {
        Some(Caller {
            app_id: self.0.into(),
            workspace: Some("room".into()),
        })
    }
}

type Contacts = Vec<(String, String, Vec<String>)>;

fn contacts(id: &str, endpoint: &str) -> Contacts {
    vec![(id.into(), id.into(), vec![endpoint.into()])]
}

#[tokio::test]
async fn only_contacts_names_the_priority_contacts_and_a_message_from_one_is_marked() {
    let notifications = Notifications::new();
    let (_stranger_server, stranger) = pair(&notifications, Arc::new(Launched)).await;
    let refused = proxy(&stranger)
        .await
        .call_method(
            "XTiloReplacePriorityContacts",
            &(contacts("ada", "ada@example.com"),),
        )
        .await;
    assert!(refused.is_err(), "another app may not name them");
    assert_eq!(notifications.priority_contacts().count(), 0);

    let (_server, client) =
        pair(&notifications, Arc::new(LaunchedAs(priority::CONTACTS_APP))).await;
    let kept: u32 = proxy(&client)
        .await
        .call_method(
            "XTiloReplacePriorityContacts",
            &(contacts("ada", "Ada@Example.com"),),
        )
        .await
        .expect("Contacts names them")
        .body()
        .deserialize()
        .expect("a count");
    assert_eq!(kept, 1);

    let proxy = proxy(&stranger).await;
    let message = |handle: &str| {
        let mut hints = HashMap::new();
        hints.insert(
            "x-tilo-contact-handle".to_owned(),
            OwnedValue::from(Str::from(handle)),
        );
        ("Chat", 0, "", "Hi", "", Vec::new(), hints, -1)
    };
    notify(&proxy, message("ada@example.com")).await;
    notify(&proxy, message("eve@example.com")).await;
    let marked: Vec<bool> = arrived(&notifications)
        .iter()
        .map(|notification| notification.priority_contact)
        .collect();
    assert_eq!(marked, [true, false]);
}

#[tokio::test]
async fn an_app_schedules_reads_and_cancels_its_own_deadlines() {
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(Launched)).await;
    let proxy = proxy(&client).await;
    let far = 4_102_444_800_000_i64; // 2100-01-01
    proxy
        .call_method(
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
    let pending: Vec<(String, i64)> = proxy
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
    assert_eq!(deadline.room, "7");
    proxy
        .call_method("XTiloCancelScheduledNotification", &("standup",))
        .await
        .expect("cancelled");
    assert!(notifications.schedules().deadlines().is_empty());

    let (_other_server, unattributed) = pair(&notifications, Arc::new(NoCallers)).await;
    let refused = self::proxy(&unattributed)
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
}

#[test]
fn deadlines_are_scheduled_delivered_snoozed_and_cancelled_through_their_resources() {
    let notifications = Notifications::new();
    let mut services = services(&notifications);
    let due = chrono::DateTime::parse_from_rfc3339("2026-04-14T12:00:00+00:00").expect("instant");
    let done = services
        .invoke(
            SCHEDULES_RESOURCE,
            "schedule",
            &arguments(json!({
                "app_id": "so.tilo.clocks",
                "tag": "wake",
                "title": "Wake up",
                "activation": "tilo-clocks:alarm/1",
                "at_ms": due.timestamp_millis(),
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

    notifications.deliver_due(&due);
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
    let actions: Vec<String> = notifications.resources()[1].children[0]
        .actions
        .iter()
        .map(|action| action.id.clone())
        .collect();
    assert_eq!(actions, ["cancel", "dismiss-snooze"]);
    let cancelled = services
        .invoke(&resource, "cancel", &serde_json::Map::new())
        .expect("published");
    assert!(cancelled.is_ok(), "{cancelled:?}");
    assert!(services.read(&resource).is_none());
}

#[test]
fn priority_contacts_are_marked_and_unmarked_through_their_resources() {
    let notifications = Notifications::new();
    let mut services = services(&notifications);
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
    let unmarked = services
        .invoke(&resource, "unmark", &serde_json::Map::new())
        .expect("published");
    assert!(unmarked.is_ok(), "{unmarked:?}");
    assert!(!notifications.priority_contacts().matches("ada@example.com"));
    let refused = services
        .invoke(
            PRIORITY_CONTACTS_RESOURCE,
            "mark",
            &arguments(json!({ "id": "x", "name": "X", "endpoints": ["nope"] })),
        )
        .expect("published");
    assert!(refused.is_err(), "no endpoint to match");
}

#[tokio::test]
async fn an_image_path_hint_arrives_as_pixels() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("avatar.png");
    let image = image::RgbaImage::from_pixel(4, 2, image::Rgba([0, 0, 255, 255]));
    image.save(&path).expect("written");
    let notifications = Notifications::new();
    let (_server, client) = pair(&notifications, Arc::new(NoCallers)).await;
    let proxy = proxy(&client).await;
    let mut hints = HashMap::new();
    hints.insert(
        "image-path".to_owned(),
        OwnedValue::from(Str::from(path.to_str().expect("utf-8"))),
    );
    notify(&proxy, ("App", 0, "", "Sum", "", Vec::new(), hints, -1)).await;
    let [notification] = arrived(&notifications).try_into().expect("exactly one");
    let pixels = notification.image_data.expect("decoded");
    assert_eq!((pixels.width, pixels.height), (4, 2));
    assert!(
        notifications.live()[0].image_data.is_none(),
        "the published notification keeps no pixels"
    );
}
