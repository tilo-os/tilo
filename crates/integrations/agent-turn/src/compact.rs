//! Keeping a long turn within its model's context: what tools answered long ago gives way first.
//!
//! The person's request, the model's own words and calls, and the latest round's answers stay
//! whole. Past the turn's bound, the oldest tool answers are replaced by a line saying they were
//! left out, and older pictures by a note, until the conversation fits again. The model can ask
//! the tool again for anything it still needs.

use agent_provider_config::protocol::{Content, Message, Role};

/// What an answer left out reads.
pub(crate) const LEFT_OUT: &str =
    "(an earlier answer, left out to keep within your context: call the tool again if you need it)";

/// The characters `message` carries.
fn size(message: &Message) -> usize {
    message
        .content
        .iter()
        .map(|content| match content {
            Content::Text { text } | Content::Reasoning { text, .. } => text.len(),
            Content::Image { base64, .. } => base64.len(),
            Content::ToolCall(call) => call.input.to_string().len(),
            Content::ToolResult(result) => result.content.len(),
        })
        .sum()
}

/// Leave out old tool answers and pictures from `messages` until they carry at most `most`
/// characters, or nothing older than the latest round is left to leave out.
pub(crate) fn compact(messages: &mut [Message], most: usize) {
    let mut total: usize = messages.iter().map(size).sum();
    if total <= most {
        return;
    }
    // The latest tool answers stay: the model has not read them yet.
    let last_tool = messages
        .iter()
        .rposition(|message| matches!(message.role, Role::Tool))
        .unwrap_or(0);
    for message in messages.iter_mut().take(last_tool) {
        if total <= most {
            return;
        }
        let before = size(message);
        for content in &mut message.content {
            match content {
                Content::ToolResult(result) if result.content != LEFT_OUT => {
                    LEFT_OUT.clone_into(&mut result.content);
                }
                Content::Image { .. } => {
                    *content = Content::Text {
                        text: "(a picture shown earlier)".to_owned(),
                    };
                }
                _ => {}
            }
        }
        total = total.saturating_sub(before.saturating_sub(size(message)));
    }
}
