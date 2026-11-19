//! Keeping a long turn within its model's context: what tools answered long ago gives way first.
//!
//! The person's request, the model's own words and calls, and the latest round's answers stay
//! whole. Once the conversation passes its high mark, the oldest tool answers are replaced by a
//! line saying they were left out, and older pictures by a note, down to a lower mark (60% of the
//! high mark by default). Dropping down to a lower mark in one step keeps the conversation prefix
//! unchanged across multiple following rounds so provider prompt caches stay warm. The model can
//! ask the tool again for anything it still needs.

use agent_provider_config::protocol::{Content, Message, Role};

/// What an answer left out reads.
pub(crate) const LEFT_OUT: &str =
    "(an earlier answer, left out to keep within your context: call the tool again if you need it)";

/// The default lower mark as a percentage of the context high mark once compaction triggers.
pub(crate) const DEFAULT_LOW_MARK_PERCENT: u8 = 60;

/// Compute the default lower mark (60% of `high`).
#[must_use]
pub(crate) fn default_low_mark(high: usize) -> usize {
    low_mark(high, DEFAULT_LOW_MARK_PERCENT)
}

/// Compute `percent`% of `high`, clamped to `0..=100` so the lower mark never exceeds `high`.
#[must_use]
pub(crate) fn low_mark(high: usize, percent: u8) -> usize {
    let clamped = u128::from(percent.min(100));
    let scaled = (high as u128)
        .saturating_mul(clamped)
        .checked_div(100)
        .unwrap_or(0);
    usize::try_from(scaled).unwrap_or(high)
}

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

/// Leave out old tool answers and pictures from `messages` once they exceed `most` characters,
/// freeing down to the default lower mark (60% of `most`), or until nothing older than the latest
/// round is left to leave out.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "called by unit tests alongside compact_to")
)]
pub(crate) fn compact(messages: &mut [Message], most: usize) {
    compact_to(messages, most, default_low_mark(most));
}

/// Leave out old tool answers and pictures from `messages` once they exceed `high` characters,
/// freeing down to `low` characters (clamped to `high`), or until nothing older than the latest
/// round is left to leave out.
pub(crate) fn compact_to(messages: &mut [Message], high: usize, low: usize) {
    let mut total: usize = messages.iter().map(size).sum();
    if total <= high {
        return;
    }
    let target = low.min(high);
    // The latest tool answers stay: the model has not read them yet.
    let last_tool = messages
        .iter()
        .rposition(|message| matches!(message.role, Role::Tool))
        .unwrap_or(0);
    for message in messages.iter_mut().take(last_tool) {
        if total <= target {
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

#[cfg(test)]
mod tests {
    use agent_provider_config::protocol::{Content, Message, Role, ToolResult};

    use super::{DEFAULT_LOW_MARK_PERCENT, LEFT_OUT, compact, compact_to, default_low_mark, size};

    fn tool_message(id: &str, bytes: usize) -> Message {
        Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: id.into(),
                content: "x".repeat(bytes),
                is_error: false,
            })],
        }
    }

    fn tool_text(message: &Message) -> &str {
        match &message.content[0] {
            Content::ToolResult(result) => result.content.as_str(),
            other => panic!("expected tool result, got {other:?}"),
        }
    }

    #[test]
    fn default_low_mark_is_sixty_percent() {
        assert_eq!(DEFAULT_LOW_MARK_PERCENT, 60);
        assert_eq!(default_low_mark(400_000), 240_000);
        assert_eq!(default_low_mark(0), 0);
        assert_eq!(super::low_mark(1_000, 150), 1_000);
    }

    #[test]
    fn under_high_mark_leaves_prefix_untouched() {
        let mut messages = vec![
            Message::text(Role::User, "request"),
            tool_message("1", 1_000),
            tool_message("2", 1_000),
            tool_message("3", 1_000),
        ];
        // Above 60% of 3_100 (1_860), but still under the high mark (3_100).
        let before = messages.clone();
        compact(&mut messages, 3_100);
        assert_eq!(messages, before);
    }

    #[test]
    fn over_high_mark_frees_down_to_sixty_percent_and_keeps_latest() {
        // High mark = 10_000 -> default low mark = 6_000.
        // 6 tool messages of 2_000 chars each = 12_000 (> 10_000).
        let mut messages = vec![Message::text(Role::User, "request")];
        for i in 1..=6 {
            messages.push(tool_message(&i.to_string(), 2_000));
        }
        compact(&mut messages, 10_000);

        let total: usize = messages.iter().map(size).sum();
        assert!(
            total <= 6_000,
            "compaction should drop down to 60% (6_000), got {total}"
        );
        // Oldest 4 dropped (leaving ~4_000 + small placeholders <= 6_000); newest 2 stay whole.
        for message in &messages[1..=4] {
            assert_eq!(tool_text(message), LEFT_OUT);
        }
        assert_eq!(tool_text(&messages[5]).len(), 2_000);
        assert_eq!(tool_text(&messages[6]).len(), 2_000);
    }

    #[test]
    fn custom_low_mark_controls_how_much_is_freed() {
        let mut messages = vec![Message::text(Role::User, "request")];
        for i in 1..=6 {
            messages.push(tool_message(&i.to_string(), 2_000));
        }
        // High = 10_000, low = 8_500 -> only needs to drop 2 oldest messages (~8_200 <= 8_500).
        compact_to(&mut messages, 10_000, 8_500);

        assert_eq!(tool_text(&messages[1]), LEFT_OUT);
        assert_eq!(tool_text(&messages[2]), LEFT_OUT);
        for message in &messages[3..=6] {
            assert_eq!(tool_text(message).len(), 2_000);
        }
    }

    #[test]
    fn low_mark_above_high_mark_clamps_to_high_mark() {
        let mut messages = vec![
            Message::text(Role::User, "request"),
            tool_message("1", 2_000),
            tool_message("2", 2_000),
            tool_message("3", 2_000),
        ];
        // Total is 6_007 > high (5_000); low (9_000) clamps to 5_000, dropping only message 1.
        compact_to(&mut messages, 5_000, 9_000);
        assert_eq!(tool_text(&messages[1]), LEFT_OUT);
        assert_eq!(tool_text(&messages[2]).len(), 2_000);
        assert_eq!(tool_text(&messages[3]).len(), 2_000);
    }

    #[test]
    fn multi_round_growth_keeps_prefix_stable_across_rounds() {
        const HIGH: usize = 400_000;
        const ROUND_BYTES: usize = 24_000;
        let mut messages = vec![
            Message::text(Role::System, "system"),
            Message::text(Role::User, "request"),
        ];
        let mut rewrites = 0usize;
        for round in 0..40 {
            let prefix_before = messages.clone();
            messages.push(tool_message(&round.to_string(), ROUND_BYTES));
            compact(&mut messages, HIGH);
            if messages[..prefix_before.len()] != prefix_before[..] {
                rewrites = rewrites.saturating_add(1);
            }
        }
        // Without a lower mark, 40 rounds of 24 KB rewrite the prefix ~24 times.
        // With a 60% lower mark, it compacts only 3-4 times total.
        assert!(
            (2..=5).contains(&rewrites),
            "expected 2..=5 prefix rewrites across 40 rounds, got {rewrites}"
        );
    }
}
