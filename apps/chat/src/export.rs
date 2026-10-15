//! A conversation as a transcript: what `export-content` keeps as content.
//!
//! Authors are shown by name, never by participant id, and times as RFC 3339
//! in UTC, so the transcript reads the same wherever it is opened.

use std::fmt::Write as _;

use crate::model::{Channel, Message};

/// `seconds` since the Unix epoch, RFC 3339 in UTC.
fn rfc3339(seconds: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0).map_or_else(
        || seconds.to_string(),
        |date| date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

/// `messages` of `channel`, oldest first, as `format` (`md` or `json`), each
/// author named by `name_of`.
///
/// # Errors
/// An unknown format, or JSON that does not encode.
pub fn transcript(
    channel: &Channel,
    messages: &[&Message],
    format: &str,
    name_of: impl Fn(&str) -> String,
) -> Result<Vec<u8>, String> {
    let title = channel.kind.label(if channel.name.trim().is_empty() {
        "channel"
    } else {
        channel.name.trim()
    });
    match format {
        "json" => {
            let rows: Vec<serde_json::Value> = messages
                .iter()
                .map(|message| {
                    serde_json::json!({
                        "id": message.id,
                        "author": name_of(&message.author),
                        "authorId": message.author,
                        "ts": rfc3339(message.ts),
                        "text": message.text,
                        "parent": message.parent,
                        "edited": message.edited,
                    })
                })
                .collect();
            serde_json::to_vec_pretty(&serde_json::json!({
                "channel": format!("chat.channel:{}", channel.id),
                "name": title,
                "messages": rows,
            }))
            .map_err(|error| error.to_string())
        }
        "md" => {
            let line = |out: &mut String, indent: &str, message: &Message| {
                let text = message.text.trim().replace('\n', &format!("\n{indent}  "));
                let _ = writeln!(
                    out,
                    "{indent}- {} **{}**: {text}",
                    rfc3339(message.ts),
                    name_of(&message.author)
                );
            };
            let mut out = format!("# {title}\n\n");
            let top_level = |message: &&&Message| {
                message.parent.as_deref().is_none_or(|parent| {
                    parent.is_empty() || !messages.iter().any(|other| other.id == parent)
                })
            };
            for message in messages.iter().filter(top_level) {
                line(&mut out, "", message);
                for reply in messages
                    .iter()
                    .filter(|reply| reply.parent.as_deref() == Some(message.id.as_str()))
                {
                    line(&mut out, "  ", reply);
                }
            }
            Ok(out.into_bytes())
        }
        other => Err(format!("no format `{other}`: md or json")),
    }
}
