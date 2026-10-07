//! Genuine delegated host enrollment through the signed governor protocol.
use super::{Qualification, native};
use std::{path::Path, sync::Arc};
use tilo_joined_room::testing::governor::TestGovernor;

pub(super) struct EnrolledHost {
    pub native: Arc<native::Native>,
    pub key: ed25519_dalek::SigningKey,
    pub certificate: tilo_user_identity::DeviceCert,
    pub task: String,
}
pub(super) fn delegated(installation_root: &Path) -> Qualification<EnrolledHost> {
    enrolled(installation_root, false)
}
pub(super) fn member(installation_root: &Path) -> Qualification<EnrolledHost> {
    enrolled(installation_root, true)
}
fn enrolled(installation_root: &Path, member: bool) -> Qualification<EnrolledHost> {
    let owner = tilo_joined_room::open_file(&installation_root.join("native.json"))?;
    let mut governor = TestGovernor::new();
    let (key, certificate) = if member {
        let key = ed25519_dalek::SigningKey::from_bytes(&[63; 32]);
        let certificate = tilo_joined_room::testing::certificate(
            &ed25519_dalek::SigningKey::from_bytes(&[64; 32]),
            &key,
            "Non-host member",
        );
        (key, certificate)
    } else {
        (governor.host_key().clone(), governor.host().clone())
    };
    let task = governor.queue_task("fixture-agent", "Qualify native managed controller")?;
    let room = tilo_desktop_model::RoomId::parse(governor.room()).ok_or("delegated room absent")?;
    tilo_joined_room::testing::enroll_certified_device(&owner, &mut governor, &key, &certificate)?;
    Ok(EnrolledHost {
        native: Arc::new(native::Native { owner, room }),
        key,
        certificate,
        task,
    })
}
