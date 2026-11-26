//! What a branch of a workspace leaves behind (`docs/architecture/workspace.md`, "Branching a
//! workspace"): one rule, for a device branching a workspace it holds and for a host branching
//! one it keeps.

/// Chat's records: every conversation of the workspace, which stays behind.
pub use tilo_app_id::apps::CHAT;
/// The id Chat keeps its own conversations under: the one it was published as before it took the
/// id of the app it replaced (`previously:`). What the replaced app kept stays under [`CHAT`].
pub const CHAT_DOCUMENTS: &str = "so.tilo.lang.chat";
/// Terminal's records: its environments, which the runtime branches itself.
pub use tilo_app_id::apps::TERMINAL;
/// The record each person's place in the workspace's tilolang apps is kept in, one for all of them.
pub const PERSONAL_PLACES: (&str, &str) = ("tilo.host", "tilolang.personal");

/// Whether the record `record` of `app` stays behind when its workspace is branched: what is tied
/// to other people, and what the branch makes of its own.
#[must_use]
pub fn stays(app: &str, record: &str) -> bool {
    let root = collab_types::document_access_target(app, record).unwrap_or(record);
    if collab_types::is_unread_record(app, root) {
        return true;
    }
    match app {
        // Every conversation, each person's drafts and read marks, and moderation history stay
        // behind. The workspace's own emoji and agents are Chat's to keep but the workspace's, so
        // they come along.
        CHAT => collab_types::shared_workspace_document(app, record).is_none(),
        // Chat's own conversations, every one of them. The runtime branches the environments,
        // with their own files; the branch's layout is written from its own windows, never the
        // source's layout of everyone's.
        CHAT_DOCUMENTS | TERMINAL | crate::ownership::DESKTOP_APP => true,
        _ => collab_types::workspace_document(app, record)
            .is_some_and(|known| (known.app, known.kind) == PERSONAL_PLACES),
    }
}
