//! Messages from outside the room, and a channel's transcript as content.
//!
//! `deliver-message` ([`crate::parts::ChatPart::deliver_message`]) adds a
//! line someone wrote elsewhere (a seeded scenario, an import, an
//! integration relaying it) shown under their name: the message stays the
//! deliverer's, as a bridge's post does, so who put it in the room is never
//! lost. Given the time it was written, its id derives from what it says
//! ([`delivered_id`]), so delivering it again changes nothing.
//!
//! `export-content` keeps a channel's transcript (authors by the name
//! readers see, times as RFC 3339) as content, for an agent to hand on.

use std::fmt::Write as _;

use ui::tilo_resource::Id;

use crate::record::Message;

/// The id of a message delivered into `channel` by `author` at `ts`
/// saying `body`: the same delivery always names the same message. Two
/// FNV-1a passes give 128 bits.
#[must_use]
pub fn delivered_id(channel: &Id, author: &str, ts: i64, body: &str) -> Id {
    fn fnv1a(bytes: &[u8], basis: u64) -> u64 {
        bytes.iter().fold(basis, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }
    let ts = ts.to_string();
    let joined = [channel.as_str(), author.trim(), &ts, body].join("\u{1f}");
    Id::from_raw(format!(
        "d{:016x}{:016x}",
        fnv1a(joined.as_bytes(), 0xcbf2_9ce4_8422_2325),
        fnv1a(joined.as_bytes(), 0x0102_0304_0506_0708)
    ))
}

/// The name readers see for `message`: the name it was delivered or
/// bridged under, else its author's as `name` gives it.
pub fn shown_author(message: &Message, name: &dyn Fn(&str) -> String) -> String {
    message.appearance.as_ref().map_or_else(
        || name(message.author.0.as_str()),
        |shown| shown.name.clone(),
    )
}

/// `ts` (Unix seconds) as RFC 3339 in UTC.
#[must_use]
pub fn rfc3339(ts: i64) -> String {
    let (year, month, day) = ui::date::civil_from_days(ts.div_euclid(86_400));
    let of_day = ts.rem_euclid(86_400);
    // format: machine (RFC 3339 in a transcript file, read by programs)
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// What a channel's transcript is made of: its id and name, and its
/// messages oldest first with their ids.
pub struct Channel<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub messages: Vec<(&'a Id, &'a Message)>,
}

/// The transcript of `channel` as `format` (`md`, the default, or `json`),
/// each author named by `name` unless shown under another name.
///
/// # Errors
/// An unknown format.
pub fn transcript(
    channel: &Channel<'_>,
    format: Option<&str>,
    name: &dyn Fn(&str) -> String,
) -> Result<Vec<u8>, String> {
    match format.map_or("md", str::trim) {
        "md" | "markdown" | "" => {
            let mut out = format!("# #{}\n\n", channel.name);
            for (_, message) in &channel.messages {
                let reply = if message.parent.is_some() {
                    "  ↳ "
                } else {
                    "- "
                };
                let _ = writeln!(
                    out,
                    "{reply}**{}** ({}): {}",
                    shown_author(message, name),
                    rfc3339(message.ts),
                    message.text.replace('\n', "\n    ")
                );
            }
            Ok(out.into_bytes())
        }
        "json" => serde_json::to_vec_pretty(&serde_json::json!({
            "channel": { "id": channel.id, "name": channel.name },
            "messages": channel.messages.iter().map(|(id, message)| serde_json::json!({
                "id": id.as_str(),
                "author": message.author.0,
                "authorName": shown_author(message, name),
                "date": rfc3339(message.ts),
                "parent": message.parent.as_ref().map(Id::as_str),
                "text": message.text,
            })).collect::<Vec<_>>(),
        }))
        .map_err(|error| error.to_string()),
        other => Err(format!("`{other}` is not a format: md or json")),
    }
}

/// What `export-content` answers once the transcript is kept as `hash`:
/// its content address (also as the operation's resource, so the room
/// keeps it), the format and how much it holds.
#[must_use]
pub fn exported(
    hash: &str,
    format: Option<&str>,
    messages: usize,
    bytes: usize,
) -> ui::framework::Effected {
    let address = ui::tilo_actions::content_resource(hash);
    ui::framework::Effected::Done(ui::tilo_actions::ActionOutput {
        value: Some(serde_json::json!({
            "content": address,
            "format": if format.map(str::trim) == Some("json") { "json" } else { "md" },
            "messages": messages,
            "bytes": bytes,
        })),
        resources: address.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests;
