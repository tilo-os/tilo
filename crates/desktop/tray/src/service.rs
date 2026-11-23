//! The tray as resources: one per item, with its primary action and every
//! enabled entry of its menu, so the desktop's search, a script, an agent
//! and a screen reader reach what a click on its icon reaches, by address.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionState, ApiError, ResourceSpec};
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode};

use crate::{Tray, TrayItem, TrayRequest};

/// The resource the items are published under.
pub const TRAY: &str = "tray";
pub const TRAY_KIND: &str = "so.tilo.tray";
/// The kind of an item's resource, and of one entry of its menu.
pub const ITEM_KIND: &str = "so.tilo.tray.item";
pub const ENTRY_KIND: &str = "so.tilo.tray.entry";
const ITEM_PREFIX: &str = "tray-item:";
/// Between an item's token and a menu entry's id, in the entry's resource.
const ENTRY_SEPARATOR: char = '/';

/// What a tray item offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.tray.item")]
pub enum TrayAction {
    /// Do what clicking the item's icon does: usually show its window.
    #[effect(kind = "outside")]
    #[effect(own_device, row, icon = "app-window", on = TrayItemRow)]
    Activate,
    /// Choose an entry of the item's menu.
    #[effect(kind = "outside")]
    #[effect(own_device, row, on = TrayEntryRow)]
    Choose {
        #[param("The entry's id in the item's menu")]
        entry: i64,
    },
}

/// The tray, as a declared type: every item the apps hold.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.tray", label = "Tray")]
pub struct TrayRoot;

/// One tray item, as a declared type: what clicking its icon does.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.tray.item", label = "Tray item", effects = TrayAction)]
pub struct TrayItemRow;

/// One enabled entry of an item's menu, as a declared type: choosing it does
/// what the item's menu does.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.tray.entry",
    label = "Tray menu entry",
    effects = TrayAction
)]
pub struct TrayEntryRow;

/// The token of the item at `key`: its bus name says which app holds it,
/// which a published id must not.
fn token(key: &str) -> String {
    let digest = blake3::hash(key.as_bytes());
    digest.to_hex().get(..16).unwrap_or_default().to_owned()
}

fn item_spec(item: &TrayItem) -> ResourceSpec {
    let token = token(&item.key);
    let id = format!("{ITEM_PREFIX}{token}");
    let mut bindings = tilo_resource::RowBindings::new();
    if !item.item_is_menu {
        bindings = bindings.offer(TrayAction::ACTIVATE);
    }
    let mut resource = tilo_resource::LiveRow::typed_with::<TrayItemRow>(
        id.clone(),
        item.title.clone(),
        &bindings,
    )
    .into_resource();
    for action in &mut resource.actions {
        action.type_mut().description = Some(item.title.clone());
    }
    resource.children = item
        .menu
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| {
            let bindings = tilo_resource::RowBindings::new()
                .bind(TrayAction::CHOOSE, [("entry", i64::from(entry.id).into())]);
            let mut child = tilo_resource::LiveRow::typed_with::<TrayEntryRow>(
                format!("{id}{ENTRY_SEPARATOR}{}", entry.id),
                entry.label.clone(),
                &bindings,
            )
            .into_resource();
            for choose in &mut child.actions {
                let ty = choose.type_mut();
                ty.label.clone_from(&entry.label);
                ty.description = Some(item.title.clone());
                if let Some(on) = entry.toggled {
                    ty.state = ActionState {
                        badge: Some(if on { "On" } else { "Off" }.to_owned()),
                        toggles: true,
                        shortcut: None,
                    };
                }
            }
            child
        })
        .collect();
    resource
}

/// The tray, published.
pub struct TrayService {
    tray: Arc<Tray>,
    /// The tray's revision when it was last published.
    seen: u64,
}

impl TrayService {
    #[must_use]
    pub fn new(tray: Arc<Tray>) -> Self {
        let seen = tray.revision();
        Self { tray, seen }
    }
}

impl Service for TrayService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            tilo_resource::typed_resource::<TrayRoot>(TRAY, crate::i18n::name(TRAY_KIND, "Tray"))
                .with_children(self.tray.snapshot().iter().map(item_spec)),
        ]
    }

    fn read(&self, _resource: &str) -> Option<Value> {
        None
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let effect = decode::<TrayAction>(&self.resources(), resource, action, arguments)?;
        let unknown = || ApiError::Unknown(ActionAddress::new(resource, action));
        let wanted = resource
            .strip_prefix(ITEM_PREFIX)
            .map(|rest| {
                rest.split_once(ENTRY_SEPARATOR)
                    .map_or(rest, |(item, _)| item)
            })
            .ok_or_else(unknown)?;
        let item = self
            .tray
            .snapshot()
            .into_iter()
            .find(|item| token(&item.key) == wanted)
            .ok_or_else(unknown)?;
        let request = match effect {
            TrayAction::Activate => TrayRequest::Activate { x: 0, y: 0 },
            TrayAction::Choose { entry } => TrayRequest::Menu {
                id: i32::try_from(entry).map_err(|_| unknown())?,
            },
        };
        self.tray
            .request(&item.key, request)
            .map_err(ApiError::Failed)?;
        Ok(Started::Accepted)
    }

    fn poll(&mut self) -> bool {
        let now = self.tray.revision();
        std::mem::replace(&mut self.seen, now) != now
    }
}
