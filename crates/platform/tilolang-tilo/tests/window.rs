//! The window a test's code runs as, and what a change reads back from a store alone.

use automerge::AutoCommit;

thread_local! {
    /// The window this thread's code runs as ([`store`]): its scope, entered for the thread, and
    /// the content store lent it.
    static WINDOW: (
        tilo_wake::WakeScope,
        tilo_content::ContentStore,
        tilo_wake::WakeScopeGuard,
    ) = {
        let scope = tilo_wake::WakeScope::new();
        let content = tilo_content::ContentStore::memory().sealing();
        tilo_actions::content::register(&scope.handle(), content.clone());
        let entered = scope.enter();
        (scope, content, entered)
    };
}

/// The content store of the window the code on this thread runs as, opened the first time it is
/// asked for: its scope entered and its store registered for it, sealed as a record's store is.
/// An action too large to record whole keeps its large parts there and every read of one finds
/// them, as the host lends each window its record's store.
#[must_use]
pub fn store() -> tilo_content::ContentStore {
    WINDOW.with(|(_, content, _)| content.clone())
}

/// What the change `change` decodes as in `peer`, a replica of the record as it holds it, its
/// action's large parts read from `store` alone: on a thread no window lent a store.
pub fn decoded(
    mut peer: AutoCommit,
    change: automerge::ChangeHash,
    store: tilo_content::ContentStore,
) -> Result<tilo_actions::Verified, tilo_actions::ActionError> {
    let decoded = std::thread::spawn(move || {
        let mut registry = tilo_actions::ActionRegistry::new();
        tilolang_tilo::history::register(&mut registry);
        registry.connect_content(store);
        tilo_actions::verify_change(&mut peer, change, &registry)
    })
    .join();
    match decoded {
        Ok(decoded) => decoded,
        // The thread's own panic is this test's: nothing was read off it.
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
