//! Chat's resources, as the app that draws them and every device holding a
//! workspace's keys read and change them.
//!
//! The workspace's chat is [`Chat`]: its channels and categories, each person's
//! read marks, reviewed feedback, moderation and what an agent keeps of a
//! conversation it answers in. What a person is writing and has not sent is
//! [`ChatDrafts`], their own resource. The channel's messages are kept in parts
//! beside the resource (`chat.messages`), each a resource of its own, so the chat
//! stays small however long it goes on.
//!
//! The verbs below are what the app's window invokes and what a workspace's
//! server applies from the app's package, so a server takes the app's work with no
//! build of its own.

pub mod chat;
pub mod drafts;
pub mod effects;
pub mod feedback;
pub mod model;
pub mod parts;
pub mod rows;

pub use chat::{
    CHAT_CHANNEL, Category, ChannelPlace, Chat, Conversation, PeopleNames, Summary, VOICE_CHANNEL,
    WorkspaceFiles, author, conversation_entry, member_marks, seconds, unnamed_label,
};
pub use drafts::ChatDrafts;
pub use effects::ChatEffect;
pub use feedback::{Report, TriageEntry, TriageUpdate};
pub use model::ChannelKind;
pub use parts::ReadMark;
