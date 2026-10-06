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
