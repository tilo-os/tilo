#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op { View, Edit, Admin }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceId {
    App(String),
    File(u64),
    Workspace(u64),
}

#[derive(Clone, Debug)]
pub struct PermissionScope { pub resource: ResourceId, pub op: Op }

#[derive(Clone, Debug, Default)]
pub struct PermissionContext {
    // v1: single-user; every op allowed. Populated in Plan 4.
    #[allow(dead_code)]
    placeholder: (),
}

impl PermissionContext {
    pub fn single_user() -> Self { Self::default() }
    pub fn allows(&self, _scope: &PermissionScope) -> bool { true }
}
