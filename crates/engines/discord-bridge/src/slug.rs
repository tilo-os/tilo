//! Names as Discord takes them. Text channel names are lower-case slugs,
//! and webhook usernames may not contain "discord" or "clyde". A name is
//! cleaned the same way every time, so the reconcile compares like with like
//! and never loops on a name Discord would rewrite.

/// Discord's limit for a channel name.
pub const MAX_CHANNEL_NAME_CHARS: usize = 100;
/// Discord's limit for a webhook username.
pub const MAX_USERNAME_CHARS: usize = 80;
/// The slug of a name with nothing Discord keeps.
pub const FALLBACK_SLUG: &str = "channel";
/// The username of an author whose name has nothing Discord keeps.
pub const FALLBACK_USERNAME: &str = "Tilo member";

/// The slug Discord would make of `name` for a text channel: lower case,
/// spaces and separators as `-`, other punctuation and symbols dropped,
/// never empty.
#[must_use]
pub fn channel_slug(name: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '_' {
            if dash && !slug.is_empty() {
                slug.push('-');
            }
            dash = false;
            slug.push(c);
        } else if c.is_whitespace() || c == '-' || c == '.' || c == '/' {
            dash = true;
        }
    }
    let slug: String = slug.chars().take(MAX_CHANNEL_NAME_CHARS).collect();
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        FALLBACK_SLUG.to_owned()
    } else {
        slug
    }
}

/// The name a channel of `kind` shows in Discord: text channels slugged,
/// categories and voice channels as written (trimmed, bounded).
#[must_use]
pub fn discord_channel_name(kind: discord_wire::op::ChannelKind, name: &str) -> String {
    use discord_wire::op::ChannelKind;
    match kind {
        ChannelKind::Text | ChannelKind::Thread => channel_slug(name),
        ChannelKind::Voice | ChannelKind::Category => {
            let trimmed: String = name.trim().chars().take(MAX_CHANNEL_NAME_CHARS).collect();
            if trimmed.is_empty() {
                FALLBACK_SLUG.to_owned()
            } else {
                trimmed
            }
        }
    }
}

/// Names for `names` in order, unique: a name already taken gets `-2`,
/// `-3`… appended, so two Tilo channels that slug alike stay apart.
#[must_use]
pub fn unique(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut taken = std::collections::BTreeSet::new();
    names
        .into_iter()
        .map(|name| {
            let mut candidate = name.clone();
            let mut n = 2_u32;
            while taken.contains(&candidate) {
                candidate = format!("{name}-{n}");
                n = n.saturating_add(1);
            }
            taken.insert(candidate.clone());
            candidate
        })
        .collect()
}

/// A webhook username Discord accepts for `name`. Discord refuses names
/// containing "discord" or "clyde" and treats `@`, `#`, `:` and code fences
/// as markup, so those are removed; an automated author gets the robot mark
/// because a webhook has no other way to say so.
#[must_use]
pub fn webhook_username(name: &str, automated: bool) -> String {
    let mut cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '@' | '#' | ':' | '`') && !c.is_control())
        .collect();
    // Removing one word can join the halves of another, so repeat until
    // neither is left.
    loop {
        let before = cleaned.len();
        for banned in ["discord", "clyde"] {
            cleaned = remove_ascii_case_insensitive(&cleaned, banned);
        }
        if cleaned.len() == before {
            break;
        }
    }
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let base = if cleaned.is_empty() {
        FALLBACK_USERNAME.to_owned()
    } else {
        cleaned
    };
    let named = if automated {
        format!("🤖 {base}")
    } else {
        base
    };
    named.chars().take(MAX_USERNAME_CHARS).collect()
}

/// The webhook username of a person who linked their Discord account: their
/// Tilo name and their Discord handle, "Ada (@ada)", so they read as
/// the same person on both sides. Within Discord's 80 characters the name
/// gives way, ending in "…", and the handle stays whole. Discord's markup and
/// banned words are cleaned out of both as in [`webhook_username`]; a handle
/// it would refuse whole is left out.
#[must_use]
pub fn linked_webhook_username(name: &str, handle: &str) -> String {
    let handle = webhook_username(handle, false);
    if handle == FALLBACK_USERNAME || handle.is_empty() {
        return webhook_username(name, false);
    }
    let suffix = format!(" (@{handle})");
    let workspace = MAX_USERNAME_CHARS.saturating_sub(suffix.chars().count());
    let name = webhook_username(name, false);
    let name = if name.chars().count() > workspace {
        let kept: String = name.chars().take(workspace.saturating_sub(1)).collect();
        format!("{}…", kept.trim_end())
    } else {
        name
    };
    format!("{name}{suffix}")
        .chars()
        .take(MAX_USERNAME_CHARS)
        .collect()
}

fn remove_ascii_case_insensitive(text: &str, needle: &str) -> String {
    let mut out = text.to_owned();
    while let Some(at) = out.to_ascii_lowercase().find(needle) {
        out.replace_range(at..at.saturating_add(needle.len()), "");
    }
    out
}
