//! A session's page holder: the document core over the replica a session client's engine admits
//! into, read as the device that replica is held as, with the device's keys as its own workspace
//! service asks them. What a test of a session reads its workspace through, as a page does.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_session_client::engine::Engine;

use super::HeldDocuments;

/// The page holder `engine` reads `workspace` through: `None` while the engine holds no such
/// workspace. A read goes on from what the holder last opened, so a caller that reads more than
/// once keeps the holder rather than asking for a new one.
pub fn page_holder(engine: &Arc<Engine>, workspace: WorkspaceId) -> Option<HeldDocuments> {
    let replica = engine.owner().replica(workspace).ok()?;
    let reader = engine
        .owner()
        .with_current(workspace, |view| Ok(view.device().device_public_key))
        .ok()?;
    let handed = Arc::downgrade(engine);
    let behind = Arc::downgrade(engine);
    let keys = crate::own_device::key_groups(
        engine.keys(),
        Some(Box::new(move |workspace| {
            WorkspaceId::parse(workspace).and_then(|id| handed.upgrade()?.handed_keys(id))
        })),
        None,
        Some(Arc::new(move |group, held| {
            if let Some(link) = behind.upgrade().and_then(|engine| engine.link()) {
                link.want_frames(group, held);
            }
        })),
    )
    .keys;
    Some(HeldDocuments::over(
        &workspace.to_string(),
        replica,
        reader,
        None,
        Some(keys),
    ))
}
