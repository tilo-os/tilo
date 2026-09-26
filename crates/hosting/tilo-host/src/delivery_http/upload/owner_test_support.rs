//! Real file-worker construction for owner integration tests; no alternate witness.
use super::*;
pub(crate) fn stage_for_owner_test(
    store: &crate::store::SessionStore,
    inventory: &crate::store::HostWorkInventory,
    pool: &HostBytes,
    identity: StageIdentity,
    bytes: &[u8],
) -> CompletedUpload {
    assert_eq!(identity.total, bytes.len() as u64);
    assert_eq!(identity.digest, *blake3::hash(bytes).as_bytes());
    let now: u64 = web_time::SystemTime::now()
        .duration_since(web_time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let reservation = store.reserve_staging(inventory, identity).unwrap();
    let mut stage = UploadStage::open(reservation, pool, now).unwrap();
    for (index, bytes) in bytes.chunks(240 * 1024).enumerate() {
        let chunk = StateItemChunk::new(
            identity.delivery,
            identity.digest,
            identity.total,
            (index * 240 * 1024) as u64,
            bytes,
        )
        .unwrap();
        let chunk = UploadChunk::copy(&chunk, pool).unwrap();
        stage = stage.append(chunk, now).unwrap();
    }
    stage.complete().unwrap()
}
