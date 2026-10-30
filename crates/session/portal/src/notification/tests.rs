use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use tilo_notifications_service::Arrival;
use tilo_service::Questions;
use tilo_service::Service as _;
use zbus::zvariant::Str;

use super::*;
use crate::auth::FRONTEND_BUS_NAME;
use crate::dbus_tests::SessionBus;
use crate::{BUS_NAME, PortalAuthenticator, PortalCaller};

type Options = HashMap<String, OwnedValue>;

fn text(value: &str) -> OwnedValue {
    OwnedValue::from(Str::from(value))
}

fn owned(value: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(value).expect("an owned value")
}

fn button(
    label: &str,
    action: &str,
    target: Option<&str>,
    purpose: Option<&str>,
) -> HashMap<String, Value<'static>> {
    let mut button = HashMap::from([
        ("label".to_owned(), Value::from(label.to_owned())),
        ("action".to_owned(), Value::from(action.to_owned())),
    ]);
    if let Some(target) = target {
        button.insert("target".to_owned(), Value::new(target.to_owned()));
    }
    if let Some(purpose) = purpose {
        button.insert("purpose".to_owned(), Value::from(purpose.to_owned()));
    }
    button
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encoded");
    bytes
}

#[test]
fn priority_maps_to_urgency_and_markup_reduces_to_text() {
    for (priority, expected) in [
        ("low", 0),
        ("normal", 1),
        ("high", 2),
        ("urgent", 2),
        ("bogus", 1),
    ] {
        let options = Options::from([("priority".to_owned(), text(priority))]);
        assert_eq!(urgency(&options), expected, "{priority}");
    }
    assert_eq!(urgency(&Options::new()), 1);
    assert_eq!(
        markup_to_plain_text("Hello <b>world</b> &amp; <a href='x'>friends</a>"),
        "Hello world & friends"
    );
    let options = Options::from([
        ("body".to_owned(), text("plain")),
        ("markup-body".to_owned(), text("<i>marked</i> up")),
    ]);
    assert_eq!(body(&options), "marked up", "the markup wins");
}

#[test]
fn the_default_action_and_buttons_become_the_daemons_actions() {
    let options = Options::from([
        ("default-action".to_owned(), text("app.open")),
        (
            "default-action-target".to_owned(),
            owned(Value::new("thread-7")),
        ),
        (
            "buttons".to_owned(),
            owned(Value::new(vec![
                button("Mark read", "app.read", Some("thread-7"), None),
                button("", "app.unlabelled", None, None),
                button("Reply", "app.reply", None, Some(REPLY_WITH_TEXT)),
                button("Archive", "app.archive", None, None),
                button("Fourth", "app.too-many", None, None),
            ])),
        ),
    ]);
    let (listed, routes) = actions(&options);
    assert_eq!(
        listed,
        [
            (DEFAULT.to_owned(), DEFAULT_LABEL.to_owned()),
            ("button-0".to_owned(), "Mark read".to_owned()),
            (INLINE_REPLY.to_owned(), "Reply".to_owned()),
        ],
        "a button without a label is skipped; only three buttons are read"
    );
    assert_eq!(
        routes.get(DEFAULT).map(|route| route.action.as_str()),
        Some("app.open")
    );
    assert_eq!(
        routes.get(INLINE_REPLY).map(|route| route.action.as_str()),
        Some("app.reply")
    );
}

#[test]
fn display_hints_category_and_sound_shape_the_notification() {
    let options = Options::from([
        ("title".to_owned(), text("Rain")),
        (
            "display-hint".to_owned(),
            owned(Value::new(vec!["persistent", "transient"])),
        ),
        ("category".to_owned(), text("weather.warning.extreme")),
        ("sound".to_owned(), text("silent")),
    ]);
    let shown = shown(9, "org.example.Weather", &options, Vec::new());
    assert_eq!(shown.id, 9);
    assert_eq!(shown.summary, "Rain");
    assert_eq!(shown.expire_timeout, 0, "persistent stays until closed");
    assert!(shown.transient);
    assert!(shown.suppress_sound);
    assert_eq!(shown.category.as_deref(), Some("weather.warning.extreme"));
    assert_eq!(
        shown.caller.map(|caller| caller.app_id),
        Some("org.example.Weather".to_owned())
    );
    let plain = super::shown(1, "app", &Options::new(), Vec::new());
    assert_eq!(plain.expire_timeout, -1);
    assert!(!plain.suppress_sound && !plain.transient);
}

#[tokio::test]
async fn an_icon_is_a_theme_name_or_decoded_pixels() {
    let themed_bare = Options::from([("icon".to_owned(), text("mail-unread"))]);
    assert_eq!(icon(&themed_bare).await, Icon::Themed("mail-unread".into()));
    let themed = Options::from([(
        "icon".to_owned(),
        owned(Value::new((
            "themed",
            Value::new(vec!["", "mail-unread", "mail"]),
        ))),
    )]);
    assert_eq!(icon(&themed).await, Icon::Themed("mail-unread".into()));
    let bytes = Options::from([(
        "icon".to_owned(),
        owned(Value::new(("bytes", Value::new(png(3, 2))))),
    )]);
    let Icon::Pixels(pixels) = icon(&bytes).await else {
        panic!("bytes decode to pixels");
    };
    assert_eq!((pixels.width, pixels.height), (3, 2));
    let garbage = Options::from([(
        "icon".to_owned(),
        owned(Value::new(("bytes", Value::new(vec![1_u8, 2, 3])))),
    )]);
    assert_eq!(icon(&garbage).await, Icon::None);
}

#[tokio::test]
async fn an_icon_by_file_descriptor_is_read_from_its_start() {
    use std::io::Write as _;
    let mut file = tempfile::tempfile().expect("a file");
    file.write_all(&png(2, 2)).expect("written");
    // The descriptor's offset is at its end, as the frontend leaves it.
    let fd = std::os::fd::OwnedFd::from(file);
    let options = Options::from([(
        "icon".to_owned(),
        owned(Value::new((
            "file-descriptor",
            Value::Fd(zbus::zvariant::Fd::from(fd)),
        ))),
    )]);
    let Icon::Pixels(pixels) = icon(&options).await else {
        panic!("the file decodes to pixels");
    };
    assert_eq!((pixels.width, pixels.height), (2, 2));
}

#[test]
fn records_are_bounded_and_route_actions_with_their_targets() {
    let records = Records::default();
    let mut next = 0_u32;
    let mut alloc = || {
        next = next.wrapping_add(1);
        next
    };
    for index in 0..=MAX_ACTIVE {
        let (_, evicted) = records.upsert(
            "org.example.App",
            &index.to_string(),
            HashMap::new(),
            &mut alloc,
        );
        assert_eq!(
            evicted,
            (index == MAX_ACTIVE).then_some(1),
            "the oldest makes room"
        );
    }
    assert_eq!(records.keys().len(), MAX_ACTIVE);
    assert!(!records.keys().iter().any(|(_, id)| id == "0"));

    let routes = HashMap::from([(
        DEFAULT.to_owned(),
        Route {
            action: "app.open".into(),
            target: Some(owned(Value::new("thread-7"))),
        },
    )]);
    let (numeric, _) = records.upsert("org.example.Chat", "msg", routes, &mut alloc);
    let (again, _) = records.upsert("org.example.Chat", "msg", HashMap::new(), &mut alloc);
    assert_eq!(again, numeric, "replacing keeps its id");
    assert!(
        records
            .invoked(
                numeric,
                Taken::Action {
                    key: DEFAULT,
                    activation_token: None
                }
            )
            .is_none(),
        "the replacement offers nothing"
    );
    let routes = HashMap::from([(
        DEFAULT.to_owned(),
        Route {
            action: "app.open".into(),
            target: Some(owned(Value::new("thread-7"))),
        },
    )]);
    records.upsert("org.example.Chat", "msg", routes, &mut alloc);
    let invoked = records
        .invoked(
            numeric,
            Taken::Action {
                key: DEFAULT,
                activation_token: Some("tok"),
            },
        )
        .expect("routed");
    assert_eq!(
        (
            invoked.app_id.as_str(),
            invoked.id.as_str(),
            invoked.action.as_str()
        ),
        ("org.example.Chat", "msg", "app.open")
    );
    let [target, platform] = invoked.parameter.as_slice() else {
        panic!("the target and the platform data");
    };
    assert_eq!(
        String::try_from(target.try_clone().expect("clone"))
            .ok()
            .as_deref(),
        Some("thread-7")
    );
    let platform: HashMap<String, OwnedValue> = platform
        .try_clone()
        .expect("clone")
        .try_into()
        .expect("a{sv}");
    assert_eq!(
        platform
            .get("activation-token")
            .and_then(|token| String::try_from(token.try_clone().ok()?).ok())
            .as_deref(),
        Some("tok")
    );
    assert_eq!(
        records.remove("org.example.Other", "msg"),
        None,
        "another app's id is not this one"
    );
    assert_eq!(records.remove("org.example.Chat", "msg"), Some(numeric));
}

/// The backend with its notification interface on a bus of its own, a
/// frontend owning its name, and the daemon's store it feeds.
struct Bed {
    _bus: SessionBus,
    notifications: Notifications,
    frontend: zbus::Connection,
    stranger: zbus::Connection,
    _backend: zbus::Connection,
}

async fn bed() -> Bed {
    let bus = SessionBus::start();
    let backend = bus.connect().await;
    let questions = Questions::new();
    let authenticator = PortalAuthenticator::session_bus(
        backend.clone(),
        Arc::new(|app: &str, _: Option<&str>| Some(PortalCaller::new(app))),
    );
    let portal = Portal::new(questions.clone(), authenticator)
        .with_consents(crate::consent::Consents::new(questions, None));
    let notifications = Notifications::new();
    serve_on(&portal, &backend, notifications.clone())
        .await
        .expect("served");
    backend
        .request_name(BUS_NAME)
        .await
        .expect("the backend's name");
    let frontend = bus.connect().await;
    frontend
        .request_name(FRONTEND_BUS_NAME)
        .await
        .expect("the frontend's name");
    let stranger = bus.connect().await;
    Bed {
        _bus: bus,
        notifications,
        frontend,
        stranger,
        _backend: backend,
    }
}

const INTERFACE: &str = "org.freedesktop.impl.portal.Notification";

async fn add(from: &zbus::Connection, app: &str, id: &str, options: Options) -> zbus::Result<()> {
    from.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some(INTERFACE),
        "AddNotification",
        &(app, id, options),
    )
    .await
    .map(drop)
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

/// The next `ActionInvoked` on `signals`: its app, id, action and parameter.
async fn next_invoked(
    signals: &mut zbus::proxy::SignalStream<'_>,
) -> (String, String, String, Vec<OwnedValue>) {
    tokio::time::timeout(Duration::from_secs(5), signals.next())
        .await
        .expect("the signal arrives")
        .expect("open")
        .body()
        .deserialize()
        .expect("arguments")
}

/// A chat message with a target, an icon as bytes and a reply button.
fn chat_message() -> Options {
    Options::from([
        ("title".to_owned(), text("New message")),
        ("body".to_owned(), text("Lunch?")),
        ("default-action".to_owned(), text("app.open")),
        (
            "default-action-target".to_owned(),
            owned(Value::new("thread-7")),
        ),
        (
            "icon".to_owned(),
            owned(Value::new(("bytes", Value::new(png(4, 4))))),
        ),
        (
            "buttons".to_owned(),
            owned(Value::new(vec![button(
                "Reply",
                "app.reply",
                Some("thread-7"),
                Some(REPLY_WITH_TEXT),
            )])),
        ),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_portal_notification_is_the_daemons_and_its_actions_reach_the_app() {
    let bed = bed().await;
    let options = chat_message();
    // Only the frontend may call.
    let refused = add(&bed.stranger, "org.example.Chat", "msg", options.clone()).await;
    assert!(refused.is_err() && bed.notifications.live().is_empty());

    let signals = zbus::Proxy::new(&bed.frontend, BUS_NAME, OBJECT_PATH, INTERFACE)
        .await
        .expect("proxy");
    let mut invoked = signals
        .receive_signal("ActionInvoked")
        .await
        .expect("subscribed");
    add(&bed.frontend, "org.example.Chat", "msg", options)
        .await
        .expect("added");
    let [shown] = arrived(&bed.notifications).try_into().expect("one arrives");
    assert_eq!(shown.summary, "New message");
    assert_eq!(shown.body, "Lunch?");
    assert!(shown.takes_reply());
    let pixels = shown.image_data.as_ref().expect("the icon as pixels");
    assert_eq!((pixels.width, pixels.height), (4, 4));

    // The person chooses it through its published resource.
    let mut daemon = bed.notifications.clone();
    let resource = tilo_notifications_service::notification_resource(shown.id);
    let mut arguments = serde_json::Map::new();
    arguments.insert("action".into(), serde_json::Value::from(DEFAULT));
    daemon
        .invoke(&resource, "invoke", &arguments)
        .expect("invoked");
    let (app, id, action, parameter) = next_invoked(&mut invoked).await;
    assert_eq!(
        (app.as_str(), id.as_str(), action.as_str()),
        ("org.example.Chat", "msg", "app.open")
    );
    assert_eq!(
        parameter
            .first()
            .and_then(|target| String::try_from(target.try_clone().ok()?).ok())
            .as_deref(),
        Some("thread-7")
    );

    // Added again, it replies inline: the reply reaches the app as its
    // button's action, with the text.
    add(
        &bed.frontend,
        "org.example.Chat",
        "msg",
        Options::from([
            ("title".to_owned(), text("Again")),
            (
                "buttons".to_owned(),
                owned(Value::new(vec![button(
                    "Reply",
                    "app.reply",
                    None,
                    Some(REPLY_WITH_TEXT),
                )])),
            ),
        ]),
    )
    .await
    .expect("replaced");
    let [again] = arrived(&bed.notifications).try_into().expect("one arrives");
    assert_eq!(again.id, shown.id, "the same notification, replaced");
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".into(), serde_json::Value::from("Sure"));
    daemon
        .invoke(&resource, "reply", &arguments)
        .expect("replied");
    let (_, _, action, parameter) = next_invoked(&mut invoked).await;
    assert_eq!(action, "app.reply");
    let platform: HashMap<String, OwnedValue> = parameter
        .last()
        .and_then(|data| data.try_clone().ok())
        .and_then(|data| data.try_into().ok())
        .expect("platform data");
    assert_eq!(
        platform
            .get("response")
            .and_then(|text| String::try_from(text.try_clone().ok()?).ok())
            .as_deref(),
        Some("Sure")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_withdraws_only_its_own_notification() {
    let bed = bed().await;
    let options = || Options::from([("title".to_owned(), text("Hi"))]);
    add(&bed.frontend, "org.example.A", "same", options())
        .await
        .expect("added");
    add(&bed.frontend, "org.example.B", "same", options())
        .await
        .expect("added");
    assert_eq!(bed.notifications.live().len(), 2);
    let remove = |from: &zbus::Connection, app: &'static str| {
        let from = from.clone();
        async move {
            from.call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(INTERFACE),
                "RemoveNotification",
                &(app, "same"),
            )
            .await
            .map(drop)
        }
    };
    assert!(remove(&bed.stranger, "org.example.A").await.is_err());
    assert_eq!(bed.notifications.live().len(), 2);
    remove(&bed.frontend, "org.example.A")
        .await
        .expect("removed");
    let live = bed.notifications.live();
    assert_eq!(live.len(), 1);
    assert_eq!(
        live.first()
            .and_then(|n| n.caller.as_ref())
            .map(|caller| caller.app_id.as_str()),
        Some("org.example.B")
    );
    let version: u32 = zbus::Proxy::new(&bed.frontend, BUS_NAME, OBJECT_PATH, INTERFACE)
        .await
        .expect("proxy")
        .get_property("version")
        .await
        .expect("version");
    assert_eq!(version, 2);
}
