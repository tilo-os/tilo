//! A record's writes in the clear before its first sealed write are its
//! base; after it, a write in the clear is a downgrade the host never keeps
//! or forwards, decided by the write's place in the workspace's order.
use super::*;

/// A write of `seq` to the fixture's record, sealed or in the clear, framed as
/// the fixture's source publishes it.
fn write(fixture: &Fixture, seq: u64, sealed: bool) -> Envelope {
    let (app, record) = ("so.tilo.notes", "welcome");
    let workspace = "room";
    let author = collab_types::Ed25519PublicKey::from_bytes(fixture.source.peer.device_public_key);
    let artifact = collab_types::document_artifact_id(app, workspace, record);
    let attestation = collab_types::document_attestation_id(app, workspace, record);
    let payload = vec![u8::try_from(seq).unwrap(); 8];
    let op = if sealed {
        collab_types::SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            author,
            attestation,
            collab_types::OpKind::Change,
            std::time::UNIX_EPOCH,
            seq,
            collab_types::EncryptionMetadata::mls_application(1, collab_types::ChannelId::new()),
        )
    } else {
        collab_types::SignedOp::new_unsigned(
            payload,
            artifact,
            author,
            attestation,
            collab_types::OpKind::Change,
            std::time::UNIX_EPOCH,
            seq,
        )
    };
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::AppDocument {
        peer: tilo_peer_state::peer_handle(&fixture.source.peer.device_public_key),
        app_id: app.into(),
        cluster_id: workspace.into(),
        record_id: record.into(),
        envelope: Box::new(op),
    })
    .unwrap();
    Envelope::Delta {
        workspace_id: fixture.store.workspace_id(),
        author_device_public_key: fixture.source.peer.device_public_key,
        authored_at_ns: seq,
        delta: crate::coedit::CoeditPayload::RetainedDelta {
            channel: neighbour_wire::app_document_channel(app, record),
            body,
        }
        .encode()
        .unwrap(),
        signature: [0; 64],
    }
}

fn kept(fixture: &Fixture, frame: &Envelope) -> bool {
    let raw = crate::transport::encode(frame).unwrap();
    fixture.store.load_retained().unwrap().contains(&raw)
}

/// The host keeps a record's cleartext base and its sealed writes, and no
/// write in the clear after its seal: not when it arrives right behind the
/// seal, and not after the host read the record's history afresh, as it does
/// once restarted.
#[tokio::test]
async fn a_host_keeps_no_cleartext_write_after_its_records_seal() {
    let fixture = Fixture::new();
    let base = write(&fixture, 1, false);
    let seal = write(&fixture, 2, true);
    let after = write(&fixture, 3, false);
    for frame in [&base, &seal, &after] {
        fixture.publish_checkpoint_frame(frame.clone(), true).await;
    }
    assert!(kept(&fixture, &base), "the base is kept");
    assert!(kept(&fixture, &seal), "the seal is kept");
    assert!(!kept(&fixture, &after), "the downgrade is not kept");

    // Read afresh from what the store retains.
    fixture
        .store
        .forget_channel_listing(&neighbour_wire::app_document_channel(
            "so.tilo.notes",
            "welcome",
        ));
    let later = write(&fixture, 4, false);
    let sealed = write(&fixture, 5, true);
    for frame in [&later, &sealed] {
        fixture.publish_checkpoint_frame(frame.clone(), true).await;
    }
    assert!(!kept(&fixture, &later), "the downgrade is not kept");
    assert!(kept(&fixture, &sealed), "a sealed write is kept");
}
