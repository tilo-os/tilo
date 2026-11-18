//! What a write made: each item it created, by its id and the address it is
//! published under, and the answer that carries them beside what the action
//! itself said.

/// Each item a write made: its id, and the address it is published under.
pub(super) type Made = Vec<(String, String)>;

/// What a chain's or a container's action answers: every item it made, with
/// its id and address (`made`, `minted`), beside whatever the action itself
/// answered, and the addresses a caller follows
/// ([`tilo_actions::ActionOutput::resources`]).
pub(super) fn answering(
    value: Option<serde_json::Value>,
    made: &[(String, String)],
) -> tilo_actions::ActionOutput {
    let minted: Vec<String> = made.iter().map(|(id, _)| id.clone()).collect();
    let resources = made.iter().map(|(_, address)| address.clone()).collect();
    tilo_actions::ActionOutput {
        value: ui_core::bound_dispatch::with_made(value, &minted, made),
        resources,
    }
}
