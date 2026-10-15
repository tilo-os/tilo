//! Native-only fixture producer. The normal app reservation initializes each
//! canonical document once; peers only receive those resulting signed bytes.
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::{SeedDocument, SeedState, StatePeer};
use tilo_user_identity::DeviceCert;
use ui::{
    portable_storage::PortableStorage,
    record::{RecordCheckout, RecordId, RecordOwners},
};
use web_time::SystemTime;
pub fn fixture_seed(
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
) -> Result<SeedState, String> {
    let cache = PortableStorage::new("native-certified-seed", room)?;
    let mut owners = RecordOwners::portable(cache);
    let mut documents = vec![];
    for (definition, checkout) in [
        (
            word_processor::definition(),
            RecordCheckout::new(
                RecordId::for_tab(1),
                word_processor::definition().record_kind.clone(),
            ),
        ),
        (
            chat::definition(),
            RecordCheckout::package_primary(chat::definition().record_kind.clone()),
        ),
    ] {
        let records = owners
            .open(definition.app_id, room)
            .map_err(|e| e.to_string())?;
        let mut reserved = records
            .reserve_record(&checkout)
            .map_err(|e| e.to_string())?;
        reserved
            .initialize(|doc| {
                if checkout.id.is_package_primary() {
                    chat::demo::start(doc, "native", "Native workspace peer", "NW")
                        .map_err(|error| error.to_string())
                } else {
                    word_processor::initialize_empty_document(doc)
                }
            })
            .map_err(|e| e.to_string())?;
        let bytes = records
            .read(&checkout.id, |doc| doc.clone().save())
            .ok_or("seed owner missing")?;
        documents.push(SeedDocument {
            app_id: definition.app_id.into(),
            record_id: if checkout.id.is_package_primary() {
                String::new()
            } else {
                checkout.id.as_str().into()
            },
            kind: definition.record_kind.as_str().into(),
            bytes,
        });
    }
    Ok(SeedState {
        room_changes: tilo_peer_state::seed_room(room, authority, roster)?,
        documents,
    })
}
pub fn fixture_writer_insert(
    peer: &mut StatePeer,
    key: &SigningKey,
    text: &str,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    let app = word_processor::definition().app_id;
    let record = RecordId::for_tab(1);
    let bytes = peer
        .document(app, record.as_str())
        .ok_or("Writer record missing")?;
    let mut doc = automerge::AutoCommit::load(&bytes).map_err(|e| e.to_string())?;
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let containers = word_processor::model::open(&doc)?;
    let block = tilo_doc::model::blocks_in_order(&doc, &containers)
        .first()
        .cloned()
        .ok_or("Writer body empty")?;
    let before = doc.get_heads();
    word_processor::model::apply(
        &mut doc,
        &word_processor::actions::RichTextAction::SpliceText {
            region: word_processor::actions::RegionKind::Body,
            block,
            at: 0,
            del: 0,
            ins: text.into(),
        },
    )
    .map_err(|e| e.to_string())?;
    collab_types::CommitNamed::commit_edit(&mut doc, "word-processor", "body");
    peer.publish_incremental(app, record.as_str(), doc.save_after(&before), key, now)
}
