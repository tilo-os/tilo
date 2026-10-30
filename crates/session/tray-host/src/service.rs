//! The tray as resources: one per item, with its primary action and every
//! enabled entry of its menu, so the desktop's search, a script, an agent
//! and a screen reader reach what a click on its icon reaches, by address.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionState, DoorError, ResourceSpec};
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
    #[effect(own_device, row, label = "Open", icon = "app-window")]
    Activate,
    /// Choose an entry of the item's menu.
    #[effect(own_device, row)]
    Choose {
        #[param("The entry's id in the item's menu")]
        entry: i64,
    },
}

/// The token of the item at `key`: its bus name says which app holds it,
/// which a published id must not.
fn token(key: &str) -> String {
    let digest = blake3::hash(key.as_bytes());
    digest.to_hex().get(..16).unwrap_or_default().to_owned()
}

fn item_spec(item: &TrayItem) -> ResourceSpec {
    let token = token(&item.key);
    let mut resource = ResourceSpec::new(
        format!("{ITEM_PREFIX}{token}"),
        ITEM_KIND,
        item.title.clone(),
    );
    if !item.item_is_menu {
        let mut open = TrayAction::Activate.bound();
        open.type_mut().description = Some(item.title.clone());
        resource.actions.push(open);
    }
    resource.children = item
        .menu
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| {
            let mut choose = TrayAction::Choose {
                entry: i64::from(entry.id),
            }
            .bound();
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
            ResourceSpec::new(
                format!("{ITEM_PREFIX}{token}{ENTRY_SEPARATOR}{}", entry.id),
                ENTRY_KIND,
                entry.label.clone(),
            )
            .with_action(choose)
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
            ResourceSpec::new(TRAY, TRAY_KIND, "Tray")
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
    ) -> Result<Started, DoorError> {
        let effect = decode::<TrayAction>(&self.resources(), resource, action, arguments)?;
        let unknown = || DoorError::Unknown(ActionAddress::new(resource, action));
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
            .map_err(DoorError::Failed)?;
        Ok(Started::Accepted)
    }

    fn poll(&mut self) -> bool {
        let now = self.tray.revision();
        std::mem::replace(&mut self.seen, now) != now
    }
}
