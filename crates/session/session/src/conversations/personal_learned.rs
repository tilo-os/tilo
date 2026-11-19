use std::time::SystemTime;

use collab_types::DocumentMutation;
use tilo_peer_state::PersonalSync;
use tilo_resource::{Invoke, Replica};
use ui::resource::DocumentId;

use super::{CHAT_OWNER, Learned};

/// Keep the workspace where a fact was learned as context inside the person's store.
pub(super) fn record_id(workspace: &str) -> DocumentId {
    DocumentId::derived(
        &format!("chat.learned:{workspace}"),
        &DocumentId::for_tab(0),
    )
}

pub(super) fn read(bytes: &[u8]) -> Result<Learned, String> {
    open(bytes)?.state().map_err(|error| error.to_string())
}

fn open(bytes: &[u8]) -> Result<Replica<Learned>, String> {
    let mut doc = automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    tilo_resource::evolution::migrate::<Learned>(&mut doc).map_err(|error| error.to_string())?;
    Ok(Replica::from_doc(doc))
}

pub(super) fn restore(
    personal: &PersonalSync,
    id: &DocumentId,
    earlier: Vec<u8>,
) -> Result<(), String> {
    personal
        .publish_if(|store, key, now| {
            if store.holds_document(CHAT_OWNER, id.as_str()) {
                return Ok(None);
            }
            store
                .publish(
                    CHAT_OWNER,
                    id.as_str(),
                    &DocumentMutation::Snapshot(earlier),
                    key,
                    now,
                )
                .map(Some)
        })
        .map(drop)
}

pub(super) fn change(
    personal: &PersonalSync,
    id: &DocumentId,
    agent: &str,
    earlier: Option<Vec<u8>>,
    invoke: Invoke<Learned>,
) -> Result<(), String> {
    personal
        .publish(|store, key, now| {
            let held = store.document(CHAT_OWNER, id.as_str());
            let mut record = held
                .as_ref()
                .or(earlier.as_ref())
                .map_or_else(|| Ok(Replica::new()), |bytes| open(bytes))?
                .as_acting(tilo_actions::Acting::agent(agent).handed())
                .in_app(CHAT_OWNER, "Chat");
            let now_ms = now
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
                });
            record.set_now_ms(now_ms);
            let base = record.heads();
            record.apply(invoke).map_err(|error| error.to_string())?;
            let mutation = if held.is_some() {
                DocumentMutation::Incremental(record.doc_mut().save_after(&base))
            } else {
                DocumentMutation::Snapshot(record.save())
            };
            store.publish(CHAT_OWNER, id.as_str(), &mutation, key, now)
        })
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ed25519_dalek::SigningKey;
    use tilo_app::tilo_workspace_agent::learned;
    use tilo_resource::NonBlank;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
    use ui::permissions::MemberId;

    use super::*;
    use crate::personal::linked::{hand, store_of};

    fn device(root: &SigningKey, seed: u8) -> PersonalSync {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device-{seed}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        store_of(&cert, Arc::new(key))
    }

    fn fact(text: &str) -> Invoke<Learned> {
        Learned::LEARN.with(learned::learned::Learn {
            text: NonBlank::parse(text).unwrap(),
            source: NonBlank::parse("The keeper said so").unwrap(),
        })
    }

    #[test]
    fn facts_move_to_the_personal_store_and_follow_the_persons_devices() {
        let root = SigningKey::from_bytes(&[8; 32]);
        let laptop = device(&root, 12);
        let tablet = device(&root, 13);
        let context = record_id("earlier");
        let unrelated = record_id("other");
        let mut previous = Replica::<Learned>::new().as_member(MemberId::person("keeper"));
        previous.apply(fact("Earlier workspace fact")).unwrap();
        let earlier = previous.save();

        restore(&laptop, &context, earlier.clone()).unwrap();
        change(&laptop, &context, "scout", None, fact("New fact")).unwrap();
        restore(&laptop, &context, earlier).unwrap();
        hand(&laptop, &tablet);

        let kept = tablet.read(|store| {
            assert!(store.document(CHAT_OWNER, unrelated.as_str()).is_none());
            store.document(CHAT_OWNER, context.as_str()).unwrap()
        });
        let learned = read(&kept).unwrap();
        assert_eq!(learned.facts.len(), 2);
        assert!(
            learned
                .facts
                .iter()
                .any(|(_, fact)| fact.text == "Earlier workspace fact")
        );
        assert!(
            learned.facts.iter().any(|(_, fact)| {
                fact.text == "New fact" && fact.by.as_deref() == Some("scout")
            })
        );
    }
}
