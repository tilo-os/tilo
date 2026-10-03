//! Talking to one `org.kde.StatusNotifierItem` and its
//! `com.canonical.dbusmenu` menu.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt as _;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::{ItemAddress, ItemStatus, MenuEntry, Tray, TrayItem, TrayRequest, icon};

pub const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
pub const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
/// Where an item registered by bus name alone lives.
pub const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";
/// A peer-to-peer connection has no bus names, but a proxy wants one; the
/// peer ignores it.
const PEER_DESTINATION: &str = "org.kde.StatusNotifierItem";
/// Deepest submenu nesting read from a client.
const MAX_MENU_DEPTH: usize = 8;
/// Most entries kept from one menu.
const MAX_MENU_ENTRIES: usize = 256;

async fn proxy<'a>(
    address: &'a ItemAddress,
    path: &'a str,
    interface: &'static str,
) -> zbus::Result<zbus::Proxy<'a>> {
    zbus::proxy::Builder::<zbus::Proxy<'_>>::new(&address.connection)
        .destination(address.destination.as_deref().unwrap_or(PEER_DESTINATION))?
        .path(path)?
        .interface(interface)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

/// The key an item registered as `service` by `sender` is known under,
/// and where it lives: a service that is an object path lives on the
/// sender's connection; a bus name lives at [`DEFAULT_ITEM_PATH`].
pub fn locate(service: &str, sender: Option<&str>) -> (Option<String>, String) {
    if service.starts_with('/') {
        (sender.map(str::to_string), service.to_string())
    } else if service.is_empty() {
        (sender.map(str::to_string), DEFAULT_ITEM_PATH.to_string())
    } else {
        (Some(service.to_string()), DEFAULT_ITEM_PATH.to_string())
    }
}

pub fn key_for(destination: Option<&str>, path: &str) -> String {
    format!("{}{path}", destination.unwrap_or_default())
}

/// Read the item at `address` as it is right now. Every property is
/// optional in practice, so a missing one reads as its default.
///
/// # Errors
///
/// When the item's object cannot be addressed at all.
pub async fn read(address: ItemAddress, key: String) -> zbus::Result<TrayItem> {
    let item = proxy(&address, &address.path, ITEM_INTERFACE).await?;
    let id: String = item.get_property("Id").await.unwrap_or_default();
    let title: String = item
        .get_property::<String>("Title")
        .await
        .ok()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| id.clone());
    let status = item
        .get_property::<String>("Status")
        .await
        .map_or(ItemStatus::Active, |status| ItemStatus::parse(&status));
    let (pixmap_property, name_property) = if status == ItemStatus::NeedsAttention {
        ("AttentionIconPixmap", "AttentionIconName")
    } else {
        ("IconPixmap", "IconName")
    };
    let mut pixmaps: Vec<(i32, i32, Vec<u8>)> =
        item.get_property(pixmap_property).await.unwrap_or_default();
    let mut icon_name: Option<String> = item
        .get_property::<String>(name_property)
        .await
        .ok()
        .filter(|name| !name.is_empty());
    if pixmaps.is_empty() && icon_name.is_none() && status == ItemStatus::NeedsAttention {
        pixmaps = item.get_property("IconPixmap").await.unwrap_or_default();
        icon_name = item
            .get_property::<String>("IconName")
            .await
            .ok()
            .filter(|name| !name.is_empty());
    }
    let theme_path: Option<String> = item.get_property("IconThemePath").await.ok();
    let item_is_menu: bool = item.get_property("ItemIsMenu").await.unwrap_or(false);
    let menu_path = item
        .get_property::<OwnedObjectPath>("Menu")
        .await
        .ok()
        .map(|path| path.as_str().to_string())
        .filter(|path| path != "/" && !path.is_empty());
    drop(item);
    let display = if title.is_empty() { key.clone() } else { title };
    let icon = icon::resolve(
        &pixmaps,
        icon_name.as_deref(),
        theme_path.as_deref(),
        &display,
    );
    let address = ItemAddress {
        menu_path,
        ..address
    };
    let menu = match &address.menu_path {
        Some(path) => read_menu(&address, path).await.unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(TrayItem {
        key,
        id,
        title: display,
        status,
        icon,
        item_is_menu,
        menu,
        address,
    })
}

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// Read the whole menu at `path` and flatten it to its choosable entries.
///
/// # Errors
///
/// When the menu does not answer `GetLayout`.
pub async fn read_menu(address: &ItemAddress, path: &str) -> zbus::Result<Vec<MenuEntry>> {
    let menu = proxy(address, path, MENU_INTERFACE).await?;
    // Toolkits that fill their menu on demand (Qt, Electron) do it here.
    let _: zbus::Result<bool> = menu.call("AboutToShow", &(0_i32,)).await;
    let (_revision, (_, _, children)): (u32, Layout) = menu
        .call("GetLayout", &(0_i32, -1_i32, Vec::<&str>::new()))
        .await?;
    let mut entries = Vec::new();
    for child in &children {
        flatten(child, "", 0, &mut entries);
    }
    Ok(entries)
}

fn unwrap_variant<'v>(value: &'v Value<'v>) -> &'v Value<'v> {
    match value {
        Value::Value(inner) => unwrap_variant(inner),
        other => other,
    }
}

fn prop<'v>(props: &'v [(String, &'v Value<'v>)], name: &str) -> Option<&'v Value<'v>> {
    props
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| unwrap_variant(value))
}

fn prop_str(props: &[(String, &Value<'_>)], name: &str) -> Option<String> {
    match prop(props, name)? {
        Value::Str(text) => Some(text.as_str().to_string()),
        _ => None,
    }
}

fn prop_bool(props: &[(String, &Value<'_>)], name: &str) -> Option<bool> {
    match prop(props, name)? {
        Value::Bool(flag) => Some(*flag),
        _ => None,
    }
}

fn prop_i32(props: &[(String, &Value<'_>)], name: &str) -> Option<i32> {
    match prop(props, name)? {
        Value::I32(number) => Some(*number),
        _ => None,
    }
}

/// `_File` → `File`; `__` is a literal underscore.
fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                out.push('_');
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn flatten(node: &Value<'_>, prefix: &str, depth: usize, out: &mut Vec<MenuEntry>) {
    if depth > MAX_MENU_DEPTH || out.len() >= MAX_MENU_ENTRIES {
        return;
    }
    let Value::Structure(structure) = unwrap_variant(node) else {
        return;
    };
    let [Value::I32(id), Value::Dict(dict), Value::Array(children)] = structure.fields() else {
        return;
    };
    let props: Vec<(String, &Value<'_>)> = dict
        .iter()
        .filter_map(|(key, value)| match key {
            Value::Str(key) => Some((key.as_str().to_string(), value)),
            _ => None,
        })
        .collect();
    if prop_bool(&props, "visible") == Some(false)
        || prop_str(&props, "type").as_deref() == Some("separator")
    {
        return;
    }
    let label = strip_mnemonic(&prop_str(&props, "label").unwrap_or_default());
    let path = if prefix.is_empty() {
        label.clone()
    } else {
        format!("{prefix} › {label}")
    };
    let is_submenu = !children.inner().is_empty()
        || prop_str(&props, "children-display").as_deref() == Some("submenu");
    if is_submenu {
        for child in children.inner() {
            flatten(child, &path, depth.saturating_add(1), out);
        }
        return;
    }
    if label.trim().is_empty() {
        return;
    }
    let toggled = prop_str(&props, "toggle-type")
        .filter(|kind| kind == "checkmark" || kind == "radio")
        .map(|_| prop_i32(&props, "toggle-state") == Some(1));
    out.push(MenuEntry {
        id: *id,
        label: path,
        enabled: prop_bool(&props, "enabled").unwrap_or(true),
        toggled,
    });
}

/// Deliver `request` to the item.
///
/// # Errors
///
/// The bus error, or a missing menu for a menu request.
pub async fn send(address: &ItemAddress, request: TrayRequest) -> zbus::Result<()> {
    match request {
        TrayRequest::Activate { x, y } => {
            proxy(address, &address.path, ITEM_INTERFACE)
                .await?
                .call::<_, _, ()>("Activate", &(x, y))
                .await
        }
        TrayRequest::SecondaryActivate { x, y } => {
            proxy(address, &address.path, ITEM_INTERFACE)
                .await?
                .call::<_, _, ()>("SecondaryActivate", &(x, y))
                .await
        }
        TrayRequest::ContextMenu { x, y } => {
            proxy(address, &address.path, ITEM_INTERFACE)
                .await?
                .call::<_, _, ()>("ContextMenu", &(x, y))
                .await
        }
        TrayRequest::Menu { id } => {
            let path = address
                .menu_path
                .as_deref()
                .ok_or_else(|| zbus::Error::Failure("the item exports no menu".into()))?;
            let timestamp = u32::try_from(tilo_clock::unix_now()).unwrap_or(0);
            proxy(address, path, MENU_INTERFACE)
                .await?
                .call::<_, _, ()>("Event", &(id, "clicked", Value::from(0_i32), timestamp))
                .await
        }
    }
}

/// Read the item and add it to `tray`, then — on a message bus — keep it
/// current: re-read on its change signals and drop it when its owner
/// leaves the bus. Returns once the first read has landed.
///
/// # Errors
///
/// When the item cannot be reached at all.
pub async fn track(
    tray: Arc<Tray>,
    connection: zbus::Connection,
    destination: Option<String>,
    path: String,
) -> zbus::Result<String> {
    let key = key_for(destination.as_deref(), &path);
    let address = ItemAddress {
        connection: connection.clone(),
        destination: destination.clone(),
        path,
        menu_path: None,
    };
    let item = read(address.clone(), key.clone()).await?;
    tray.upsert(item);
    if connection.is_bus()
        && let Some(destination) = destination
    {
        drop(tokio::spawn(follow(
            tray,
            address,
            destination,
            key.clone(),
        )));
    }
    Ok(key)
}

async fn follow(tray: Arc<Tray>, address: ItemAddress, destination: String, key: String) {
    let Ok(item) = proxy(&address, &address.path, ITEM_INTERFACE).await else {
        return;
    };
    let Ok(mut item_signals) = item.receive_all_signals().await else {
        return;
    };
    let mut menu_signals = match &address.menu_path {
        Some(path) => match proxy(&address, path, MENU_INTERFACE).await {
            Ok(menu) => menu.receive_all_signals().await.ok(),
            Err(_) => None,
        },
        None => None,
    };
    let Ok(bus) = zbus::fdo::DBusProxy::new(&address.connection).await else {
        return;
    };
    let Ok(mut owners) = bus
        .receive_name_owner_changed_with_args(&[(0, destination.as_str())])
        .await
    else {
        return;
    };
    loop {
        let changed = tokio::select! {
            signal = item_signals.next() => signal.is_some(),
            signal = async {
                match menu_signals.as_mut() {
                    Some(stream) => stream.next().await,
                    None => std::future::pending().await,
                }
            } => signal.is_some(),
            owner = owners.next() => {
                let gone = owner
                    .and_then(|signal| signal.args().ok().map(|args| args.new_owner().is_none()))
                    .unwrap_or(true);
                if gone {
                    break;
                }
                false
            }
        };
        if !changed {
            break;
        }
        match read(address.clone(), key.clone()).await {
            Ok(item) => tray.upsert(item),
            Err(_) => break,
        }
    }
    if tray.remove(&key)
        && let Ok(emitter) =
            zbus::object_server::SignalEmitter::new(&address.connection, super::WATCHER_PATH)
    {
        let _ = super::watcher::Watcher::status_notifier_item_unregistered(&emitter, &key).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bus_name_lives_at_the_default_path_and_a_path_on_its_sender() {
        assert_eq!(
            locate("org.kde.StatusNotifierItem-4-1", Some(":1.9")),
            (
                Some("org.kde.StatusNotifierItem-4-1".into()),
                DEFAULT_ITEM_PATH.into()
            )
        );
        assert_eq!(
            locate("/org/ayatana/NotificationItem/nm", Some(":1.9")),
            (
                Some(":1.9".into()),
                "/org/ayatana/NotificationItem/nm".into()
            )
        );
        assert_eq!(key_for(None, "/StatusNotifierItem"), "/StatusNotifierItem");
    }

    #[test]
    fn mnemonics_are_not_shown() {
        assert_eq!(strip_mnemonic("_Quit"), "Quit");
        assert_eq!(strip_mnemonic("snake__case"), "snake_case");
    }
}
