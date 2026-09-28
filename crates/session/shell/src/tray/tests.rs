//! A fake application registers a tray item over a peer-to-peer D-Bus
//! connection; the shell's watcher reads it, reports it, publishes its
//! menu as actions, and delivers an invoked action back to the fake.

#![allow(
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::type_complexity,
    clippy::too_many_lines,
    reason = "the fakes implement fixed D-Bus signatures, and the test is one ordered transcript"
)]

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use automerge::{AutoCommit, ROOT};
use tokio::net::UnixStream;
use zbus::connection::Builder;
use zbus::zvariant::{Array, Dict, OwnedObjectPath, OwnedValue, StructureBuilder, Value};

use super::{Tray, WATCHER_BUS_NAME, WATCHER_PATH, watcher};
use crate::shell::chrome_state::ChromeState;
use crate::shell::chrome_state::SearchScope;
use crate::shell::search::SearchRegistry;
use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
use crate::shell::search::providers::tray_items::{self, TrayItemsProvider};

const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";

type Calls = Arc<Mutex<Vec<String>>>;

struct FakeItem {
    calls: Calls,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl FakeItem {
    #[zbus(property)]
    fn id(&self) -> String {
        "keepassxc".into()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        "KeePassXC".into()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        "Active".into()
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        vec![(2, 1, vec![0xff, 0x10, 0x20, 0x30, 0x80, 0x40, 0x50, 0x60])]
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/MenuBar").unwrap_or_default()
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    fn activate(&self, x: i32, y: i32) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("activate {x} {y}"));
    }
}

struct FakeMenu {
    calls: Calls,
}

fn entry(
    id: i32,
    props: &[(&str, Value<'static>)],
    children: Vec<Value<'static>>,
) -> Value<'static> {
    let props: HashMap<String, Value<'static>> = props
        .iter()
        .map(|(key, value)| {
            (
                (*key).to_string(),
                value.try_clone().unwrap_or_else(|_| Value::from("")),
            )
        })
        .collect();
    let structure = StructureBuilder::new()
        .add_field(id)
        .append_field(Value::Dict(Dict::from(props)))
        .append_field(Value::Array(Array::from(children)))
        .build()
        .expect("menu entry structure");
    Value::Structure(structure)
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl FakeMenu {
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn get_layout(
        &self,
        _parent: i32,
        _depth: i32,
        _properties: Vec<String>,
    ) -> (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>)) {
        let children = vec![
            entry(1, &[("label", Value::from("_Lock database"))], Vec::new()),
            entry(2, &[("type", Value::from("separator"))], Vec::new()),
            entry(
                3,
                &[
                    ("label", Value::from("Settings")),
                    ("children-display", Value::from("submenu")),
                ],
                vec![entry(
                    4,
                    &[
                        ("label", Value::from("Minimise to tray")),
                        ("toggle-type", Value::from("checkmark")),
                        ("toggle-state", Value::from(1_i32)),
                    ],
                    Vec::new(),
                )],
            ),
            entry(
                5,
                &[
                    ("label", Value::from("Sync now")),
                    ("enabled", Value::from(false)),
                ],
                Vec::new(),
            ),
        ];
        (
            7,
            (
                0,
                HashMap::new(),
                children
                    .into_iter()
                    .filter_map(|child| OwnedValue::try_from(child).ok())
                    .collect(),
            ),
        )
    }

    fn event(&self, id: i32, event_id: String, _data: OwnedValue, _timestamp: u32) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("{event_id} {id}"));
    }
}

/// How long the whole transcript, runtime teardown included, may take. It
/// catches a step blocked outside the runtime, where no step timer can fire.
fn transcript_cap() -> Duration {
    tilo_test_env::hang_bound().saturating_mul(2)
}

/// What the transcript is waiting for, for the failure that names it.
static PENDING: Mutex<&str> = Mutex::new("starting the runtime");

fn pending(what: &'static str) {
    *PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = what;
}

async fn step<T>(what: &'static str, future: impl std::future::Future<Output = T>) -> T {
    // A D-Bus call has no reply timeout of its own, so without this bound a
    // lost reply hangs the test until the harness kills it, naming nothing.
    pending(what);
    tilo_test_env::within(what, future).await
}

async fn eventually(what: &'static str, mut check: impl FnMut() -> bool) {
    step(what, async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
}

#[test]
fn a_registered_item_is_reported_published_as_actions_and_reachable() {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(transcript());
        }));
        pending("the runtime to shut down");
        runtime.shutdown_timeout(tilo_test_env::hang_bound());
        let _ = done.send(outcome);
    });
    match finished.recv_timeout(transcript_cap()) {
        Ok(Ok(())) => {}
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the tray transcript's thread ended without an outcome")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
            "the tray transcript is stuck after {} s, still waiting for {}",
            transcript_cap().as_secs(),
            PENDING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        ),
    }
}

#[allow(
    clippy::future_not_send,
    reason = "the transcript runs on its own runtime's block_on and is never sent"
)]
async fn transcript() {
    let tray = Tray::new();
    let calls: Calls = Arc::default();
    let (shell_socket, app_socket) = UnixStream::pair().expect("socket pair");
    let shell_side = Builder::unix_stream(shell_socket)
        .server(TEST_GUID)
        .expect("guid")
        .p2p();
    let app_side = Builder::unix_stream(app_socket)
        .p2p()
        .serve_at(
            "/StatusNotifierItem",
            FakeItem {
                calls: Arc::clone(&calls),
            },
        )
        .expect("serve item")
        .serve_at(
            "/MenuBar",
            FakeMenu {
                calls: Arc::clone(&calls),
            },
        )
        .expect("serve menu");
    let (_watcher, app_conn) = step("the p2p handshake", async {
        futures_util::try_join!(
            watcher::serve(shell_side, Arc::clone(&tray)),
            app_side.build()
        )
    })
    .await
    .expect("p2p pair with the watcher served");

    let watcher_proxy = step(
        "the watcher proxy",
        zbus::proxy::Builder::<zbus::Proxy<'_>>::new(&app_conn)
            .destination(WATCHER_BUS_NAME)
            .expect("destination")
            .path(WATCHER_PATH)
            .expect("path")
            .interface("org.kde.StatusNotifierWatcher")
            .expect("interface")
            .build(),
    )
    .await
    .expect("watcher proxy");
    step(
        "the registration reply",
        watcher_proxy.call::<_, _, ()>("RegisterStatusNotifierItem", &("/StatusNotifierItem",)),
    )
    .await
    .expect("registration answered");

    let key = "/StatusNotifierItem";
    eventually("the item to be read", || tray.item(key).is_some()).await;

    // What `state-dump` carries.
    let dump = tray.state_json();
    let item = dump
        .as_array()
        .and_then(|items| items.first())
        .expect("one item");
    assert_eq!(item["title"], "KeePassXC");
    assert_eq!(item["icon"], "pixels");
    let labels: Vec<&str> = item["menu"]
        .as_array()
        .expect("menu")
        .iter()
        .filter_map(|entry| entry["label"].as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["Lock database", "Settings › Minimise to tray", "Sync now"],
        "separators go, submenus flatten, mnemonics are stripped"
    );
    let registered: Vec<String> = step(
        "the registered items property",
        watcher_proxy.get_property("RegisteredStatusNotifierItems"),
    )
    .await
    .expect("items property");
    assert_eq!(registered, vec![key.to_string()]);

    // What `actions list` carries. Fixture chrome inputs: production ones
    // would query the session's audio, settings and identity.
    pending("the action catalogue");
    let chrome = ChromeState::new_with_fixture_handle(
        crate::shell::state::WorkspaceTree::bootstrap_default().shared(),
        crate::shell::hosted_inputs::HostedTileInputs::default(),
    );
    let mut registry = SearchRegistry::new();
    registry.register(Rc::new(TrayItemsProvider::new(Arc::clone(&tray))));
    let ids: Vec<String> = {
        let state = chrome.borrow();
        ActionCatalog::build(
            &registry,
            "",
            SearchScope::All,
            None,
            &state,
            &AutoCommit::new(),
            &ROOT,
            &AllVisible,
        )
        .enumerate()
        .into_iter()
        .filter(|action| action.resource.provider == tray_items::PROVIDER_ID)
        .map(|action| action.resource.id)
        .collect()
    };
    assert_eq!(
        ids,
        vec![
            tray_items::activate_id(key),
            tray_items::menu_id(key, 1),
            tray_items::menu_id(key, 4),
        ],
        "a disabled entry is not offered"
    );

    // Invoking a menu action reaches the application.
    pending("the menu action to be invoked");
    let lock = tray_items::menu_id(key, 1);
    {
        let state = chrome.borrow();
        let catalog = ActionCatalog::build_exact(
            &registry,
            &ResourceRef::new(tray_items::PROVIDER_ID, &lock),
            SearchScope::All,
            None,
            &state,
            &AutoCommit::new(),
            &ROOT,
            &AllVisible,
        );
        drop(state);
        catalog
            .select(&ResourceRef::new(tray_items::PROVIDER_ID, lock.clone()))
            .expect("lock action")
            .invoke_nullary(&mut AutoCommit::new(), &ROOT, &chrome)
            .expect("invoked");
    }
    eventually("the menu event to reach the app", || {
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&"clicked 1".to_string())
    })
    .await;

    tray.request(key, super::TrayRequest::Activate { x: 3, y: 4 })
        .expect("activate sent");
    eventually("activate to reach the app", || {
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&"activate 3 4".to_string())
    })
    .await;
    assert!(
        tray.request(key, super::TrayRequest::Menu { id: 5 })
            .is_err(),
        "a disabled entry cannot be chosen"
    );
}
