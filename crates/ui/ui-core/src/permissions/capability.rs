//! Widgets guarded by a permission scope; the toolkit's own capability, reached
//! here for callers that already import the permission module.

pub use crate::widget::permission::{PermissionGuarded, permission_scope};
