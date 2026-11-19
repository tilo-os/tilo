//! The types Chat's rows are items of where no document of its own holds
//! them: a conversation's call and the conversation with one person or agent,
//! which the window knows of before the chat keeps a message in it.

use crate::effects::ChatEffect;

/// A conversation's call: an item of the `chat.call` type, whose address is
/// the conversation's row with `/call` after it.
#[derive(tilo_resource::Row)]
#[row(kind = "chat.call", label = "Call", effects = ChatEffect)]
pub struct Call;

/// The conversation with one person or agent, addressed by their public key
/// before either has written in it.
#[derive(tilo_resource::Row)]
#[row(
    kind = "chat.conversation",
    label = "Conversation",
    effects = ChatEffect
)]
pub struct DirectConversation;
