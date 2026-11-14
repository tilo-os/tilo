//! What a peer taking the window's changes gets: replaying the action each records gives the
//! same change, to the byte.

/// Replay the action each change `doc` holds past `from` records, as a peer taking it does:
/// every change comes out the same, to the byte. How many recorded an action.
pub(super) fn replayed(
    doc: &mut automerge::AutoCommit,
    from: &[automerge::ChangeHash],
    host: &tilolang_tilo::host::Host,
) -> usize {
    let mut registry = ui::tilo_actions::ActionRegistry::new();
    tilolang_tilo::history::register(&mut registry);
    let arrived: Vec<_> = doc
        .get_changes(from)
        .iter()
        .map(automerge::Change::hash)
        .collect();
    let mut reproduced = 0;
    for change in arrived {
        let verified =
            host.with_schemas(|| ui::tilo_actions::verify_change(doc, change, &registry));
        match verified {
            Ok(ui::tilo_actions::Verified::Reproduced) => reproduced += 1,
            Ok(ui::tilo_actions::Verified::NothingRecorded) => {}
            other => panic!("a peer replaying a change gets another one: {other:?}"),
        }
    }
    reproduced
}
