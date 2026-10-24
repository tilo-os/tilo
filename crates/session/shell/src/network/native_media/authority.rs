use super::Ticket;

/// What the app sees of `ticket`'s current grant.
pub(super) fn inventory(ticket: &Ticket) -> runtime_collab::native_media::Inventory {
    ticket
        .inspect_current(|revision, grant| {
            let mut publishers = if grant.directions.subscribe {
                grant.subscriptions.clone()
            } else {
                Vec::new()
            };
            publishers.sort();
            Ok(runtime_collab::native_media::Inventory {
                revision: Some(revision),
                publication: grant
                    .directions
                    .publish
                    .then(|| grant.publication.clone())
                    .flatten(),
                publishers,
            })
        })
        .unwrap_or_default()
}

/// Who a call participant is to an app: its person (a user root key in
/// lowercase hex), or for someone a bridge carries in from another service
/// their guest id (`<service>:<account>`); `None` for a bridge's own
/// participation, which only listens to carry the call out. A bridge's
/// participations are held by the bridge's person, so never shown as it.
pub(super) fn call_id(
    participant: &tilo_relay_protocol::voice::VoiceParticipant,
) -> Option<String> {
    match &participant.bridged {
        Some(bridged) => bridged
            .account
            .as_deref()
            .map(|account| runtime_collab::native_media::Guest::id_of(&bridged.service, account)),
        None => Some(hex::encode(participant.user_public_key)),
    }
}

/// Who the call hears talking, by [`call_id`] (sorted and distinct):
/// whoever publishes one of the `heard` namespaces in `ticket`'s current
/// grant, and this device's person while its own microphone is heard
/// (`own`).
pub(super) fn speakers(ticket: &Ticket, heard: &[String], own: bool) -> Vec<String> {
    ticket
        .inspect_current(|_, grant| {
            let mine = grant.publication.as_ref().filter(|_| own);
            let people: std::collections::BTreeSet<String> = grant
                .participants
                .iter()
                .filter(|participant| {
                    heard.contains(&participant.namespace) || mine == Some(&participant.namespace)
                })
                .filter_map(call_id)
                .collect();
            Ok(people.into_iter().collect())
        })
        .unwrap_or_default()
}
