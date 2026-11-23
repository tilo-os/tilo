//! The person's list of their workspaces: a resource of their own workspace, which every device
//! of theirs reads and changes through its typed actions.
//!
//! It names each workspace of theirs in their order, with the name they last saw it by, the
//! device hosting it, where each device of theirs that holds it reaches that host, and the devices
//! that keep it off their desktop. Whose each workspace is, and the part the person takes in it,
//! is the workspace's own governance's to say, never this list's. A workspace taken off the list
//! is remembered as gone ([`WorkspaceList::left`]), so a device that was away when it went lets it
//! go too.

use std::collections::BTreeMap;

use collab_types::WorkspaceId;
use tilo_resource::{ActionCx, Collection, Id, Model, Refusal, resource};

/// The app the list is a resource of.
pub const APP: &str = "so.tilo.identity";
/// The list's resource in the person's own workspace.
pub const RESOURCE: &str = "identity.workspaces";
/// The kind the list is published as.
pub const KIND: &str = "tilo.identity.workspaces";

/// The most workspaces the list names.
pub const MAX_WORKSPACES: usize = 4096;
/// The longest name the list keeps of a workspace or a device.
const MAX_NAME: usize = 256;
/// The most bytes of one host address, hex.
const MAX_ADDRESS: usize = 16 * 1024;

/// The person's workspaces.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct WorkspaceList {
    /// Each workspace in the person's order, by its id.
    pub workspaces: Collection<ListedWorkspace>,
    /// The workspaces taken off the list, by id, with when (ms since the epoch).
    pub left: BTreeMap<String, i64>,
    /// The person's devices that show their workspaces, by key (hex), with the name each reads by.
    pub devices: BTreeMap<String, String>,
}

/// One workspace of the list.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ListedWorkspace {
    /// The name the person last saw it by.
    pub name: String,
    /// The device hosting it (its key, hex): where a device of theirs that does not hold it yet
    /// enters it. Empty while no device said.
    pub host: String,
    /// Where each device of theirs that holds it reaches its host, by that device's key (hex):
    /// the host's signed address, postcard in hex.
    pub addresses: BTreeMap<String, String>,
    /// The devices that keep it off their desktop, by key (hex). Every other device shows it,
    /// one linked later included.
    pub kept_off: BTreeMap<String, bool>,
}

impl ListedWorkspace {
    /// The device hosting it, when the list says.
    #[must_use]
    pub fn host(&self) -> Option<[u8; 32]> {
        key_of(&self.host)
    }

    /// Whether it shows on `device`.
    #[must_use]
    pub fn shows_on(&self, device: &[u8; 32]) -> bool {
        !self
            .kept_off
            .get(&hex::encode(device))
            .copied()
            .unwrap_or(false)
    }

    /// Where its host is reached, as a device of the person's that holds it said: the address
    /// its host device wrote first, then any other.
    #[must_use]
    pub fn address(&self) -> Option<Vec<u8>> {
        self.addresses
            .get(&self.host)
            .or_else(|| self.addresses.values().next())
            .and_then(|address| hex::decode(address).ok())
    }
}

/// `key` as a device key, from lowercase hex.
fn key_of(key: &str) -> Option<[u8; 32]> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(key, &mut bytes).ok().map(|()| bytes)
}

impl WorkspaceList {
    /// The list a document of it holds, as `bytes` saved it.
    ///
    /// # Errors
    /// The bytes hold no document of the list.
    pub fn of_document(bytes: &[u8]) -> Result<Self, String> {
        tilo_resource::Replica::<Self>::load(bytes)
            .and_then(|replica| replica.state())
            .map_err(|error| error.to_string())
    }

    /// The workspaces in the person's order.
    pub fn listed(&self) -> impl Iterator<Item = (WorkspaceId, &ListedWorkspace)> {
        self.workspaces
            .iter()
            .filter_map(|(id, listed)| Some((WorkspaceId::parse(id.as_str())?, listed)))
    }

    /// The workspace `workspace`, when the list names it.
    #[must_use]
    pub fn get(&self, workspace: WorkspaceId) -> Option<&ListedWorkspace> {
        self.workspaces.get(&Id::from_raw(workspace.to_string()))
    }

    /// Whether `workspace` was taken off the list.
    #[must_use]
    pub fn went(&self, workspace: WorkspaceId) -> bool {
        self.get(workspace).is_none() && self.left.contains_key(&workspace.to_string())
    }

    /// The person's devices that show their workspaces, with the name each reads by.
    #[must_use]
    pub fn named_devices(&self) -> Vec<([u8; 32], String)> {
        self.devices
            .iter()
            .filter_map(|(key, name)| Some((key_of(key)?, name.clone())))
            .collect()
    }

    fn entry(&mut self, workspace: &str) -> Result<&mut ListedWorkspace, Refusal> {
        self.workspaces
            .get_mut(&Id::from_raw(workspace))
            .ok_or_else(|| Refusal::new("the list does not name that workspace"))
    }
}

#[resource(kind = "tilo.identity.workspaces", read_content = tilo_resource::read::fields)]
impl WorkspaceList {
    /// List `workspace` as `name`, at the end of the list, or call it `name` where the list
    /// names it already.
    #[action]
    pub fn list_workspace(&mut self, workspace: String, name: String) -> Result<(), Refusal> {
        validate_workspace(&workspace)?;
        validate_name(&name)?;
        self.left.remove(&workspace);
        if let Ok(listed) = self.entry(&workspace) {
            listed.name = name;
            return Ok(());
        }
        if self.workspaces.len() >= MAX_WORKSPACES {
            return Err(Refusal::new(format!(
                "a list names at most {MAX_WORKSPACES} workspaces"
            )));
        }
        self.workspaces.push(
            Id::from_raw(workspace),
            ListedWorkspace {
                name,
                ..ListedWorkspace::default()
            },
        );
        Ok(())
    }

    /// Take `workspace` off the list: every device of the person's lets it go.
    #[action]
    pub fn unlist_workspace(
        &mut self,
        cx: &mut ActionCx,
        workspace: String,
    ) -> Result<(), Refusal> {
        validate_workspace(&workspace)?;
        self.workspaces.remove(&Id::from_raw(workspace.clone()));
        self.left.insert(workspace, cx.now_ms());
        Ok(())
    }

    /// Put `workspace` right after `after` in the list, or first where `after` is empty.
    #[action]
    pub fn place_workspace(&mut self, workspace: String, after: String) -> Result<(), Refusal> {
        let id = Id::from_raw(workspace);
        let from = self
            .workspaces
            .position(&id)
            .ok_or_else(|| Refusal::new("the list does not name that workspace"))?;
        let to = if after.is_empty() {
            0
        } else {
            let at = self
                .workspaces
                .position(&Id::from_raw(after))
                .ok_or_else(|| Refusal::new("the list does not name the workspace to follow"))?;
            if at < from { at + 1 } else { at }
        };
        self.workspaces.move_to(&id, to);
        Ok(())
    }

    /// Show `workspace` on `device`, or keep it off there.
    #[action]
    pub fn show_workspace(
        &mut self,
        workspace: String,
        device: String,
        shown: bool,
    ) -> Result<(), Refusal> {
        validate_key(&device)?;
        let listed = self.entry(&workspace)?;
        if shown {
            listed.kept_off.remove(&device);
        } else {
            listed.kept_off.insert(device, true);
        }
        Ok(())
    }

    /// Say that `host` hosts `workspace`: where a device of the person's that does not hold it
    /// enters it.
    #[action(housekeeping)]
    pub fn host_workspace(&mut self, workspace: String, host: String) -> Result<(), Refusal> {
        validate_key(&host)?;
        self.entry(&workspace)?.host = host;
        Ok(())
    }

    /// Say where `device`, which holds `workspace`, reaches its host: the host's signed address,
    /// postcard in hex.
    #[action(housekeeping)]
    pub fn reach_workspace(
        &mut self,
        workspace: String,
        device: String,
        address: String,
    ) -> Result<(), Refusal> {
        validate_key(&device)?;
        if address.len() > MAX_ADDRESS || hex::decode(&address).is_err() {
            return Err(Refusal::new("a host's address is postcard bytes in hex"));
        }
        self.entry(&workspace)?.addresses.insert(device, address);
        Ok(())
    }

    /// Name `device` among the person's devices that show their workspaces.
    #[action(housekeeping)]
    pub fn name_device(&mut self, device: String, name: String) -> Result<(), Refusal> {
        validate_key(&device)?;
        validate_name(&name)?;
        self.devices.insert(device, name);
        Ok(())
    }
}

/// `workspace` as a workspace's id.
fn validate_workspace(workspace: &str) -> Result<(), Refusal> {
    WorkspaceId::parse(workspace)
        .filter(|id| !id.is_nil())
        .map(drop)
        .ok_or_else(|| Refusal::new("that is not a workspace"))
}

/// `key` as 32 bytes in lowercase hex.
fn validate_key(key: &str) -> Result<(), Refusal> {
    key_of(key)
        .filter(|bytes| hex::encode(bytes) == key)
        .map(drop)
        .ok_or_else(|| Refusal::new("a device is named by 32 bytes in hex"))
}

fn validate_name(name: &str) -> Result<(), Refusal> {
    if name.len() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(Refusal::new(format!(
            "a name is at most {MAX_NAME} bytes, without control characters"
        )));
    }
    Ok(())
}

/// The list's kind, hosted by the identity: what applies its actions.
#[must_use]
pub fn kind() -> tilo_resource::ResourceKind {
    tilo_resource::ResourceKind::of::<WorkspaceList>().hosted_by(APP, "Identity")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replica() -> tilo_resource::Replica<WorkspaceList> {
        tilo_resource::Replica::new().as_invoker("person")
    }

    const A: &str = "00000000-0000-0000-0000-00000000000a";
    const B: &str = "00000000-0000-0000-0000-00000000000b";

    #[test]
    fn two_devices_moving_different_workspaces_both_land() {
        let mut laptop = replica();
        for (id, name) in [(A, "Notes"), (B, "Photos")] {
            laptop
                .apply(WorkspaceList::LIST_WORKSPACE.with((id.to_owned(), name.to_owned())))
                .unwrap();
        }
        let mut phone = laptop.fork();
        laptop
            .apply(WorkspaceList::PLACE_WORKSPACE.with((B.to_owned(), String::new())))
            .unwrap();
        let device = hex::encode([7; 32]);
        phone
            .apply(WorkspaceList::SHOW_WORKSPACE.with((A.to_owned(), device, false)))
            .unwrap();
        phone
            .apply(WorkspaceList::LIST_WORKSPACE.with((A.to_owned(), "Notebook".to_owned())))
            .unwrap();
        laptop.sync_with(&mut phone).unwrap();
        let list = laptop.state().unwrap();
        let named: Vec<(String, String)> = list
            .listed()
            .map(|(id, listed)| (id.to_string(), listed.name.clone()))
            .collect();
        assert_eq!(
            named,
            [
                (B.to_owned(), "Photos".to_owned()),
                (A.to_owned(), "Notebook".to_owned())
            ]
        );
        assert!(!list.listed().nth(1).unwrap().1.shows_on(&[7; 32]));
        assert!(list.listed().nth(1).unwrap().1.shows_on(&[8; 32]));
    }

    #[test]
    fn a_workspace_taken_off_is_remembered_as_gone_until_listed_again() {
        let mut list = replica();
        let a = WorkspaceId::parse(A).unwrap();
        list.apply(WorkspaceList::LIST_WORKSPACE.with((A.to_owned(), "Notes".to_owned())))
            .unwrap();
        list.apply(WorkspaceList::UNLIST_WORKSPACE.with(A.to_owned()))
            .unwrap();
        assert!(list.state().unwrap().went(a));
        list.apply(WorkspaceList::LIST_WORKSPACE.with((A.to_owned(), "Notes".to_owned())))
            .unwrap();
        assert!(!list.state().unwrap().went(a));
    }
}
