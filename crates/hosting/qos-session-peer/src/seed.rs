//! Initialize canonical app records only during explicit creation of a new session.
use qos_peer_state::{SeedDocument, SeedState};
use qos_user_identity::DeviceCert;
use ui::{
    portable_storage::PortableStorage,
    record::{RecordCheckout, RecordId, RecordOwners},
};
pub(crate) fn seed(
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
) -> Result<SeedState, String> {
    let cache = PortableStorage::new("native-session-initialize", room)?;
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
                    chat::doc::ChatDoc::seed_starter(doc, "native", "Native workspace peer", "NW");
                    Ok(())
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
        room_changes: qos_peer_state::seed_room(room, authority, roster)?,
        documents,
    })
}
