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

/// The people the call hears talking, as person ids (user root keys in
/// lowercase hex, sorted and distinct): whoever publishes one of the `heard`
/// namespaces in `ticket`'s current grant, and this device's person while
/// its own microphone is heard (`own`).
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
                .map(|participant| hex::encode(participant.user_public_key))
                .collect();
            Ok(people.into_iter().collect())
        })
        .unwrap_or_default()
}
