//! Where Chat's conversations are reached from outside the app: the shell, an app or a
//! service that writes to someone invokes these actions of Chat (`apps/tl/chat`), the way a
//! person does in its window.

/// The resource kind of a workspace's conversations, the root every channel and direct
/// conversation is listed under.
pub const CONVERSATIONS_KIND: &str = "tl.so.tilo.chat.Conversations";

/// A channel's resource kind.
pub const TEXT_CHANNEL_KIND: &str = "tl.so.tilo.chat.TextChannel";

/// A direct conversation's resource kind: one person or agent and the workspace.
pub const DIRECT_KIND: &str = "tl.so.tilo.chat.Direct";

/// A thread's resource kind.
pub const THREAD_KIND: &str = "tl.so.tilo.chat.Thread";

/// A message's resource kind.
pub const MESSAGE_KIND: &str = "tl.so.tilo.chat.Message";

/// A workspace's conversations, as Chat publishes them: its one root, made by the first action
/// that reaches it (`tl:<app>:root-<type id>`, the type id the one `apps/tl/chat/app.ids` gives
/// `Conversations`).
pub const CONVERSATIONS: &str = "tl:so.tilo.chat:root-lmdoxbnn";

/// Writes to a person or an agent in the one direct conversation with them, on
/// [`CONVERSATIONS`]: `who` names them (`{"person": key}` or `{"agent": id}`) and `body` is what
/// the message says, its text or its rich text encoded whole (`{"rich": blocks}`). It answers the
/// message it sent.
pub const WRITE_TO: &str = "write_to";

/// Opens the one direct conversation with `who`, on [`CONVERSATIONS`], and answers it.
pub const MESSAGE_PERSON: &str = "message_person";

/// Sends `body` in the channel, thread or direct conversation it is invoked on.
pub const SEND: &str = "send";

/// Files `body` as a problem report in the workspace's `#feedback` channel, on
/// [`CONVERSATIONS`], which the first report opens. It answers the message it filed.
pub const REPORT_PROBLEM: &str = "report_problem";

#[cfg(test)]
mod tests {
    #[test]
    fn the_conversations_root_is_the_one_chat_declares() {
        let ids = include_str!("../../../../apps/tl/chat/app.ids");
        let declared = ids
            .lines()
            .find_map(|line| line.strip_prefix("Conversations = "))
            .expect("Chat declares its conversations");
        assert_eq!(
            super::CONVERSATIONS,
            format!("tl:{}:root-{}", crate::apps::CHAT, declared.trim())
        );
        assert_eq!(
            super::CONVERSATIONS_KIND,
            format!("tl.{}.Conversations", crate::apps::CHAT)
        );
    }
}
